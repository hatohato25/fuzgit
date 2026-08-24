//! `gz fetch` — fetch の対象を決めて取得する（FR-18 / FR-23）。
//!
//! 主機能は「リモートを選ぶ」ことではなく「fetch 対象の決定」であり、
//! 選択の余地がある場合にだけ finder を起動する（リモートが 1 つだけなら起動しない）。
//! 対象の範囲は [`FetchScope`] で切り替える。現在のリポジトリのリモートを対象にする
//! 既定の経路（[`run_current`]）と、`--siblings` で兄弟リポジトリを複数選んで
//! 1 件ずつ取得する経路（[`run_siblings`]）を持つ。
//!
//! fuzgit で初めてネットワークを伴うコマンドだが、ネットワークへ出るのは
//! 対象が決まったあとに実行する `git fetch` だけである（`--siblings` では選択件数分を
//! 並列に実行し、対話が必要になって失敗したものだけを継承 stdio で実行し直す。
//! [`fetch_each`]）。
//! 候補生成とプレビューはローカル情報（`.git/config` の URL と保存済みの
//! リモート追跡参照）のみを読む（design.md「候補生成・プレビューでネットワーク
//! アクセスを行わない」）。プレビューは選択項目ごとに都度実行されるため、
//! ここで往復遅延や認証プロンプトを挟むと描画がそのままブロックされる。
//!
//! タイムアウト・リトライ・認証情報の取り扱いは行わない。到達不能・認証拒否は
//! git の標準メッセージのまま非ゼロ終了する。

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow, bail};
use gix::bstr::ByteSlice as _;

use crate::commands::{HEADER_SEPARATOR, aligned_candidates, last_column_range, selection_header};
use crate::error::Error;
use crate::finder::{
    FinderItem, FinderOptions, Highlight, HighlightColor, PreviewSource, SelectionMode,
    select_many_with, select_one_with,
};
use crate::git::exec::{CapturedRun, capture_git_noninteractive_in, run_git, run_git_in};
use crate::git::read::{branch_tracking_args, remote_tracking_refs_args, remote_url_args, remotes};
use crate::git::siblings::{self, SiblingRepository, SiblingScan};
use crate::i18n::{Language, Messages};
use crate::notify::{notify, notify_setting, should_notify};

/// 「すべてのリモート」を表す固定候補のキー。
///
/// リモート名は `refs/remotes/<name>/...` の一部になるため `git check-ref-format` の
/// 規則に従い、`*` を含むことができない。したがってこの識別子が実在のリモート名と
/// 衝突することはなく、選択結果の解決でリモートと取り違える余地もない
/// （FR-14 の復帰メニューと同じ固定キー方式）。
const ALL_REMOTES_KEY: &str = "*all*";

/// すべてのリモートから取得する `git fetch` のオプション。
const ALL_REMOTES_OPTION: &str = "--all";

/// リモートで削除されたブランチの追跡参照を掃除する `git fetch` のオプション。
const PRUNE_OPTION: &str = "--prune";

/// HEAD がブランチを指していない兄弟リポジトリの表示。
const DETACHED_LABEL: &str = "detached HEAD";

/// 任意のロックを取らずに git を実行するオプション。
///
/// プレビューは他人のリポジトリに対して選択項目ごとに実行されるため、
/// 他のプロセスが動作中でも干渉しないよう、ロックを要する操作を行わせない（man git）。
const NO_OPTIONAL_LOCKS: &str = "--no-optional-locks";

/// `gz fetch --siblings` の同時実行数を上書きする git config のキー（FR-28）。
///
/// CLI フラグ（`--jobs`）は設けない。同時実行数は「毎回選ぶ」性質の値ではなく、恒久的な
/// 調整は設定で足りるためである。置き場所を `fuzgit.*` に揃えているのは、fuzgit の設定を
/// 1 つの名前空間から辿れるようにするため（`fuzgit.lang` と同じ方針）。
///
/// **この設定を読むのは `gz fetch --siblings` だけ**である。`gz pull` / `gz sync` /
/// `--siblings` の無い `gz fetch` は直列・継承 stdio のままであり、同時実行数という概念を
/// 持たない（design.md「設定の読み取り」）。
const FETCH_JOBS_CONFIG_KEY: &str = "fuzgit.fetchJobs";

/// 同時実行数の既定値（4）。
///
/// CPU 数に比例させない。fetch は I/O バウンドであり、CPU 数は通信先が受け入れられる
/// 同時接続数とは無関係だからである。同一ホストへ集中しがちな用途であることと、
/// `git fetch` 1 件につき補助プロセス（`git-remote-https` / `ssh` / `index-pack`）が
/// 立つことを踏まえた値（design.md「同時実行数の既定値と上書き手段」）。
///
/// `const` で `NonZeroUsize` を組み立てるために [`NonZeroUsize::MIN`]（＝1）からの加算で
/// 書く。`NonZeroUsize::new(4)` は `Option` を返すため、ここで取り出そうとすると
/// 本番コードに `unwrap` が現れてしまう。
const DEFAULT_FETCH_JOBS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(3);

/// リモートで削除されたブランチの追跡参照を掃除するかどうか。
///
/// 掃除はローカルの参照を消す操作であるため、真偽値を持ち回さず
/// ユーザーの明示指定（`--prune`）だけで有効になることを型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneMode {
    /// 既存の追跡参照に触れない（既定）。
    Keep,
    /// リモートに存在しなくなった追跡参照を削除する（`--prune`）。
    Prune,
}

impl PruneMode {
    /// `git fetch` に付けるオプション。
    fn option(self) -> Option<&'static str> {
        match self {
            PruneMode::Keep => None,
            PruneMode::Prune => Some(PRUNE_OPTION),
        }
    }
}

/// fetch 対象を探す範囲。
///
/// fuzgit の全コマンドは現在のリポジトリのみを操作することを前提としており、
/// その唯一の例外（兄弟リポジトリ）はユーザーの明示指定でのみ有効になる。
/// 真偽値を commands 層へ持ち回さず、範囲の違いを型で表す
/// （既存の `MergeMode` / `DiffMode` / `SyncMode` と同方針）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchScope {
    /// 現在のリポジトリに登録されたリモートだけを対象にする（既定）。
    Current,
    /// 現在のリポジトリと同じ階層に並ぶリポジトリも対象に含める（`--siblings`）。
    Siblings,
}

/// fetch の対象。
///
/// 候補一覧はリモート名に固定候補「すべてのリモート」を加えたものであり、
/// 両者で `git fetch` へ渡す引数が異なるため、選択結果を型で区別して持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchTarget {
    /// 選択された 1 つのリモート。
    Remote(String),
    /// 登録されているすべてのリモート（`git fetch --all`）。
    All,
}

impl FetchTarget {
    /// 実行前の案内・エラーメッセージに示す対象の呼称。
    fn description(&self, messages: &dyn Messages) -> String {
        match self {
            FetchTarget::Remote(name) => messages.fetch().remote_description(name),
            FetchTarget::All => messages.fetch().all_remotes_label().to_owned(),
        }
    }
}

/// 対象の決め方。
///
/// 「選択の余地がある場合にだけ finder を起動する」という判断を、
/// 端末を占有する処理から切り離して単体テストできるようにするための型。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchDecision {
    /// リモートが 1 つも登録されておらず、fetch できない。
    NoRemote,
    /// 選択の余地が無いため、finder を起動せずに確定した対象。
    Fixed(FetchTarget),
    /// 候補一覧から 1 件選ぶ。
    Choose,
}

impl FetchDecision {
    /// 登録されているリモートの一覧から対象の決め方を求める。
    ///
    /// リモートが 1 つだけの場合に固定候補「すべてのリモート」と 2 択にしても、
    /// どちらを選んでも同じリモートへ通信するため選択の意味が無い。
    fn from_remotes(remotes: &[String]) -> Self {
        match remotes {
            [] => FetchDecision::NoRemote,
            [only] => FetchDecision::Fixed(FetchTarget::Remote(only.clone())),
            _ => FetchDecision::Choose,
        }
    }
}

/// fetch の対象を決めて `git fetch` を実行する。
///
/// 実行は継承 stdio で行い、更新された参照の一覧表示・認証プロンプト・進捗は git に委ねる。
///
/// # Errors
///
/// リモート一覧の取得、選択（中断を含む）、`git fetch` の実行に失敗した場合にエラーを返す。
/// リモートが 1 つも登録されていない場合は、追加方法を示して失敗する。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    scope: FetchScope,
    prune: PruneMode,
) -> Result<()> {
    match scope {
        FetchScope::Current => run_current(language, messages, repository, prune),
        FetchScope::Siblings => run_siblings(language, messages, repository, prune),
    }
}

/// 現在のリポジトリのリモートを対象に `git fetch` を実行する。
fn run_current(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    prune: PruneMode,
) -> Result<()> {
    let remotes = remotes(repository).context(messages.common().remote_list_read_failed())?;

    let target = match FetchDecision::from_remotes(&remotes) {
        FetchDecision::NoRemote => bail!(messages.fetch().no_remotes()),
        FetchDecision::Fixed(target) => {
            // finder を出さない以上、何に対して通信したのかはここでしか示せない
            // （`git fetch` は更新が無ければ何も出力しないことがある）
            report_target(messages, &mut std::io::stderr(), &target)?;
            target
        }
        FetchDecision::Choose => {
            let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
                messages.fetch().remote_header_subject(),
                messages.fetch().remote_header_outcome(),
            ));
            let selected = select_one_with(items(language, messages, &remotes), &options)?;

            // `git fetch` はパス以外の位置引数を取り `--` で保護できないため、
            // 選択結果が候補一覧に含まれることを確かめてから引数に渡す
            // （design.md セキュリティ設計）
            resolve(messages, &remotes, &selected)?
        }
    };

    let arguments = fetch_args(&target, prune);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments)
        .with_context(|| messages.fetch().fetch_failed(&target.description(messages)))?;

    Ok(())
}

/// 兄弟リポジトリを選び、選択された順ではなく候補順に `git fetch --all` を実行する（[`fetch_each`]）。
///
/// 通信先が複数になるため、対象は必ずユーザーの選択（または「現在のリポジトリ 1 件のみ」という
/// 選択の余地が無い状況）で決まる。
fn run_siblings(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    prune: PruneMode,
) -> Result<()> {
    let scan = siblings::discover(repository).context(messages.fetch().sibling_scan_failed())?;

    let targets = match SiblingsDecision::from_candidates(&scan.candidates) {
        SiblingsDecision::NoCandidate => bail!(messages.fetch().no_sibling_candidates()),
        SiblingsDecision::Fixed => {
            // finder が出ない理由を示す。対象そのものは進捗表示に出る
            report_line(
                messages,
                &mut std::io::stderr(),
                messages.fetch().single_sibling_reason(),
            )?;
            scan.candidates.iter().collect()
        }
        SiblingsDecision::Choose => {
            // 候補行と事前選択は同じ組み立て結果から作る。事前選択は表示文字列の完全一致で
            // 判定される（`crate::finder::FinderOptions`）一方、列の幅は候補一覧全体で決まるため
            let lines = aligned_candidates(&scan.candidates, sibling_cells);
            let options = FinderOptions::new(SelectionMode::Multi)
                .with_header(sibling_header(messages, &scan, prune))
                .with_preselect(preselect(&lines));
            let selected = select_many_with(sibling_items(language, messages, &lines)?, &options)?;

            // skim は選択した順に返すため、候補一覧の順序（現在のリポジトリが先頭、
            // 以降は名前順）へ揃え直したうえで、キーが候補に含まれることを検証する
            in_candidate_order(messages, &scan.candidates, &selected)?
        }
    };

    // 同時実行数と通知の設定は 1 件も実行しないうちに解決する
    // （不正な設定のまま通信を始めない。数十秒かけてからエラーにしない）
    let jobs = fetch_jobs(repository)?;
    let notification = notify_setting(repository)?;

    // 通知の閾値と比べるのは取得そのものに掛かった時間だけであり、finder で候補を
    // 選んでいる間は含めない（ユーザーが端末の前にいる時間であるため）
    let started = Instant::now();
    let summary = fetch_each(
        messages,
        &targets,
        prune,
        jobs,
        &mut std::io::stderr(),
        |directory, arguments| capture_git_noninteractive_in(language, directory, arguments),
        |directory, arguments| run_git_in(language, directory, arguments),
    )?;
    let elapsed = started.elapsed();
    report_line(
        messages,
        &mut std::io::stderr(),
        &summary_line(messages, &summary),
    )?;

    // 集計を書き出した**後**に通知する。通知が出ない環境でも集計は必ず出ることを
    // 構造で担保するためであり、失敗した場合も通知するのは「終わった」ことを
    // 伝えるのが目的だからである（FR-29）。本文に [`summary_line`] を使わないのは、
    // それが失敗したリポジトリ名（ユーザー由来の文字列）を含むためで、通知へ載せるのは
    // 件数だけに限る（design.md「並列 fetch と完了通知のセキュリティ上の考慮」）
    if should_notify(notification, elapsed) {
        notify(
            messages,
            messages.fetch().notification_title(),
            &messages
                .common()
                .run_summary(summary.succeeded, summary.failed.len()),
        );
    }

    if summary.has_failure() {
        bail!(messages.fetch().partial_failure());
    }

    Ok(())
}

/// 兄弟リポジトリの対象の決め方。
///
/// 「選択の余地がある場合にだけ finder を起動する」判断を、端末を占有する処理から
/// 切り離して単体テストできるようにするための型（[`FetchDecision`] と同じ役割）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SiblingsDecision {
    /// fetch できるリポジトリが 1 件も無い。
    NoCandidate,
    /// 現在のリポジトリ 1 件だけであり、選択の余地が無い。
    Fixed,
    /// 候補一覧から複数選択する。
    Choose,
}

impl SiblingsDecision {
    /// 候補一覧から対象の決め方を求める。
    ///
    /// finder を省略するのは候補が現在のリポジトリ 1 件だけの場合に限る。
    /// 1 件でもそれが兄弟（現在のリポジトリが除外された場合）なら、
    /// 他人のリポジトリへ通信するかどうかは見てから決められるようにする。
    fn from_candidates(candidates: &[SiblingRepository]) -> Self {
        match candidates {
            [] => SiblingsDecision::NoCandidate,
            [only] if only.is_current => SiblingsDecision::Fixed,
            _ => SiblingsDecision::Choose,
        }
    }
}

/// 兄弟リポジトリ一覧のヘッダーを組み立てる。
///
/// 除外件数は「黙って消さない」ために示し、`--prune` は適用範囲（選択したすべての
/// リポジトリ）が現在のリポジトリだけと誤解されないよう明示する。
fn sibling_header(messages: &dyn Messages, scan: &SiblingScan, prune: PruneMode) -> String {
    let mut sections = vec![messages.fetch().siblings_header().to_owned()];

    if scan.excluded > 0 {
        sections.push(messages.fetch().excluded_count(scan.excluded));
    }

    if prune == PruneMode::Prune {
        sections.push(messages.fetch().prune_scope_note().to_owned());
    }

    sections.join(HEADER_SEPARATOR)
}

/// 兄弟リポジトリ 1 件分の候補行を列へ分解する。
///
/// ディレクトリ名の長さは候補ごとにまちまちであるため、列として返して
/// [`aligned_candidates`] に幅を揃えさせる（右の列の開始位置が候補ごとにずれないように）。
///
/// ブランチがある場合はリモートごとに `<リモート>/<ブランチ>` を組み立て、プレビューの
/// 「ブランチの追跡状況」と同じ見え方に寄せる。
///
/// ただし `SiblingRepository` はリモートとブランチの対応情報を持たないため、`origin/master`
/// という表示は追跡参照 `refs/remotes/origin/master` が実在することを意味しない
/// （そのリモートに同名ブランチが無い場合や、別のリモートを追跡している場合がある）。
/// 実際の追跡状況を示すのはプレビューの「ブランチの追跡状況」セクションであり、
/// ここでの `/` は識別と絞り込みのための整形にすぎない。
fn sibling_cells(candidate: &SiblingRepository) -> Vec<String> {
    let Some(_branch) = candidate.current_branch.as_deref() else {
        // detached HEAD には対にするブランチが無いため、リモート一覧と状態をそのまま並べる。
        return vec![
            candidate.name.clone(),
            candidate.remotes.join(", "),
            sibling_destination(candidate),
        ];
    };

    vec![candidate.name.clone(), sibling_destination(candidate)]
}

/// 候補行の**右端の列**、すなわち fetch がどこから取り込むことになるのかを示す部分。
///
/// [`sibling_cells`] から切り出してあるのは、色を付ける範囲を求める
/// [`sibling_highlights`] がこの列だけを必要とするためである。行全体の列を組み直すと、
/// 候補 1 件につきリモート数に比例した確保を捨てることになる。
fn sibling_destination(candidate: &SiblingRepository) -> String {
    let Some(branch) = candidate.current_branch.as_deref() else {
        return DETACHED_LABEL.to_owned();
    };

    candidate
        .remotes
        .iter()
        .map(|remote| format!("{remote}/{branch}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 候補行のうち色を付ける範囲。
///
/// 色を付けるのは**右端の列**、すなわち fetch がどこから取り込むことになるのか
/// （`<リモート>/<ブランチ>` の並び）を示す部分である。左端のディレクトリ名は絞り込みの
/// 主対象であり、一覧の大半を占めるため色を付けない（`commit_highlights` と同じ判断）。
///
/// 配色は git に合わせる。upstream を示す青は `git branch -vv` が `[origin/main]` に
/// 使う色（実測確認済み）であり、detached の黄は「対にするブランチが無く、取り込み先を
/// 示せない」という注意喚起としての fuzgit の判断である。
///
/// 範囲は**整形後の行**に対するバイト位置である。ディレクトリ名には多バイト文字が入り得る
/// うえ、列の幅は候補一覧全体で決まる（[`aligned_candidates`]）ため、位置は列の内容から
/// 計算せず、整形側が保証する不変条件から [`last_column_range`] に求めさせる。
fn sibling_highlights(line: &str, candidate: &SiblingRepository) -> Vec<Highlight> {
    let destination = sibling_destination(candidate);
    let range = last_column_range(line, &destination);
    if range.is_empty() {
        // リモートを 1 つも持たない候補。色を付ける対象が無い
        return Vec::new();
    }

    let color = if candidate.current_branch.is_some() {
        HighlightColor::Blue
    } else {
        HighlightColor::Yellow
    };

    vec![Highlight::new(range.start, range.end, color)]
}

/// 起動時に選択済みにする候補の表示文字列を集める。
///
/// 受け取るのは [`aligned_candidates`] が組み立てた候補と表示行の対であり、
/// finder へ渡す候補行と同じ文字列がそのまま事前選択になる。
fn preselect(candidates: &[(&SiblingRepository, String)]) -> Vec<String> {
    candidates
        .iter()
        .filter(|(candidate, _)| candidate.is_current)
        .map(|(_, line)| line.clone())
        .collect()
}

/// 兄弟リポジトリを finder の候補へ変換する。
///
/// # Errors
///
/// ワークツリーのパスを文字列として扱えない場合にエラーを返す
/// （選択結果の照合キーに使うため、表示できないパスのまま進めない）。
fn sibling_items(
    language: Language,
    messages: &dyn Messages,
    candidates: &[(&SiblingRepository, String)],
) -> Result<Vec<FinderItem>> {
    candidates
        .iter()
        .map(|(candidate, line)| {
            Ok(FinderItem::new(
                line.clone(),
                sibling_key(messages, candidate)?.to_owned(),
                sibling_preview_source(messages, candidate),
                language.messages(),
            )
            .with_highlights(sibling_highlights(line, candidate)))
        })
        .collect()
}

/// 候補の照合キー（正規化済み絶対パス）を返す。
///
/// # Errors
///
/// パスが UTF-8 でない場合にエラーを返す。
fn sibling_key<'a>(messages: &dyn Messages, candidate: &'a SiblingRepository) -> Result<&'a str> {
    candidate
        .workdir
        .to_str()
        .ok_or_else(|| anyhow!(messages.fetch().path_not_utf8(&candidate.workdir)))
}

/// 兄弟リポジトリ 1 件のプレビュー内容を組み立てる。
///
/// 参照するのは `.git/config` の URL と保存済みの参照だけで、いずれもネットワークを
/// 伴わない（design.md「候補生成・プレビューでネットワークアクセスを行わない」）。
/// **作業ツリーを走査する情報は載せない**（[`sibling_tracking_args`] を参照）。
/// 実行するディレクトリはプロセスの cwd として渡し、引数配列にパスを載せない。
fn sibling_preview_source(messages: &dyn Messages, candidate: &SiblingRepository) -> PreviewSource {
    let mut sections: Vec<(String, PreviewSource)> = candidate
        .remotes
        .iter()
        .map(|remote| {
            (
                remote.clone(),
                PreviewSource::GitIn {
                    directory: candidate.workdir.clone(),
                    args: remote_url_args(remote),
                },
            )
        })
        .collect();

    sections.push((
        messages.fetch().tracking_state_section().to_owned(),
        PreviewSource::GitIn {
            directory: candidate.workdir.clone(),
            args: sibling_tracking_args(),
        },
    ));

    PreviewSource::Composite(sections)
}

/// 兄弟リポジトリのブランチの追跡状況を示す `git for-each-ref` の引数を組み立てる。
///
/// 以前は `gz status` と同じ `git status --branch --short` を用いていたが、`git status` は
/// 追跡状況を出す前に index を refresh する（全ファイルを stat し、stat 情報が食い違えば
/// 内容を読み直す）ため、規模の大きい作業ツリーでは秒単位を要する。プレビューは
/// カーソル移動のたびに同期実行されるので、そのまま描画の待ち時間になる
/// （実測と経緯は tasks.md のメモを参照）。作業ツリーを走査しない
/// [`branch_tracking_args`] へ寄せ、リポジトリの規模によらず一定の速さにする。
///
/// [`NO_OPTIONAL_LOCKS`] は他人のリポジトリでロックを取らないための保険として残す
/// （参照を読むだけの `for-each-ref` はもともとロックを取らないため、費用は掛からない）。
fn sibling_tracking_args() -> Vec<String> {
    let mut args = vec![NO_OPTIONAL_LOCKS.to_owned()];
    args.extend(branch_tracking_args());
    args
}

/// 選択されたキーを候補一覧の順序へ並べ直す。
///
/// # Errors
///
/// 候補一覧に無いキーが含まれていた場合にエラーを返す（対象を取り違えたまま
/// 他人のリポジトリへ通信しないよう、暗黙に読み飛ばさない）。
fn in_candidate_order<'a>(
    messages: &dyn Messages,
    candidates: &'a [SiblingRepository],
    selected: &[String],
) -> Result<Vec<&'a SiblingRepository>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|key| !candidates.iter().any(|candidate| has_key(candidate, key)))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            messages
                .fetch()
                .sibling_selection_not_found(&missing.join(", "))
        );
    }

    Ok(candidates
        .iter()
        .filter(|candidate| selected.iter().any(|key| has_key(candidate, key)))
        .collect())
}

/// 候補が指定のキー（正規化済み絶対パス）を持つかどうかを判定する。
///
/// 候補側のパスと文字列を `Path` として突き合わせるだけで、正規化のやり直しはしない
/// （キーは [`sibling_key`] が候補から作った文字列であり、既に正規化済み）。
fn has_key(candidate: &SiblingRepository, key: &str) -> bool {
    candidate.workdir == Path::new(key)
}

/// 2 フェーズ実行（並列 → 失敗分の直列）の集計。
///
/// 対象ごとの成否は「その対象を**最後に**実行したフェーズの結果」であり、並列で失敗して
/// 直列で成功した対象は成功として 1 回だけ数える。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct FetchSummary {
    /// 取得に成功したリポジトリの件数。
    succeeded: usize,
    /// 取得に失敗したリポジトリのディレクトリ名（候補一覧の順）。
    ///
    /// 実行順ではなく候補順で並べる。並列フェーズの実行順は決まらないため、実行順で並べると
    /// 同じ入力から違う表示が出る。
    failed: Vec<String>,
}

impl FetchSummary {
    /// 1 件でも失敗したかどうか。
    fn has_failure(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// `gz fetch --siblings` の同時実行数を解決する。
///
/// **`git` プロセスを起動しない。**`gix` のプロセス内読み取り（`config_snapshot`）で
/// 現在のリポジトリの設定（system / global / local / worktree の階層がそのまま効く）から
/// 引く。兄弟ごとに設定を変えられるようにはしない。1 回の起動で実行方法は 1 つに定める
/// （design.md「設定の読み取り」）。
///
/// 値の解釈は純関数（[`parse_fetch_jobs`]）へ分けてあり、この関数は設定を読むだけである
/// （`i18n::resolve` の「純関数と取得層の分離」と同じ形。解釈規則を環境に依存せず
/// 単体テストできるようにするため）。
///
/// # Errors
///
/// `fuzgit.fetchJobs` に 0・負数・整数でない値が設定されている場合は
/// [`Error::InvalidFetchJobs`]。呼び出し側は**実行を始める前に**これを解決し、
/// 不正な設定のまま通信を始めないようにする。
pub fn fetch_jobs(repository: &gix::Repository) -> crate::error::Result<NonZeroUsize> {
    parse_fetch_jobs(fetch_jobs_setting(repository).as_deref())
}

/// `git config fuzgit.fetchJobs` の値をそのまま読む（取得層）。
///
/// 空文字も整数でない値も**ここでは落とさない**。「未設定として扱う」「不正値として停止する」
/// という判断はすべて [`parse_fetch_jobs`] が持ち、この関数は読み取りだけを担う
/// （判断を 2 か所に分けると、どちらを直せばよいのか分からなくなるため）。
///
/// UTF-8 でない値も「未設定」へ倒さず、ロッシー変換した文字列を不正値として解釈側へ渡す
/// （`fuzgit.lang` の層 3 と同じ扱い）。
fn fetch_jobs_setting(repository: &gix::Repository) -> Option<String> {
    repository
        .config_snapshot()
        .string(FETCH_JOBS_CONFIG_KEY)
        .map(|value| value.to_str_lossy().into_owned())
}

/// `fuzgit.fetchJobs` の値を同時実行数として解釈する（純関数）。
///
/// 未設定（`None`）と空文字は [`DEFAULT_FETCH_JOBS`] とする。空文字を「未設定」とみなすのは
/// `FUZGIT_LANG` / `fuzgit.lang` の扱いと揃えるためで、`git config fuzgit.fetchJobs ""` が
/// 言語設定では規定動作になるのにここだけ停止する、という食い違いを作らない。
///
/// # Errors
///
/// 0・負数・整数でない値は [`Error::InvalidFetchJobs`] で停止する。既定値へ黙って倒すと、
/// 利用者が指定したつもりの同時実行数と実際の動作が食い違ったまま通信が始まる
/// （暗黙のフォールバック禁止。`fuzgit.lang` の明示指定と同じ扱い）。
fn parse_fetch_jobs(value: Option<&str>) -> crate::error::Result<NonZeroUsize> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(DEFAULT_FETCH_JOBS);
    };

    // 0・負数・整数でない値を [`NonZeroUsize`] への解釈でまとめて弾く。3 者を区別しないのは、
    // どれも「1 以上の整数を書く」という同じ直し方に収束し、区別しても次の操作が変わらないため
    value.parse().map_err(|_| Error::InvalidFetchJobs {
        value: value.to_owned(),
    })
}

/// 選択されたリポジトリを並列で取得し、失敗したものだけを直列で実行し直す（FR-28）。
///
/// # 並列化しない、という判断を覆した理由
///
/// 以前は「git 自身も複数リモートの fetch を既定で逐次実行する（man git-fetch）」ことを根拠に
/// 並列化しないと決めていた。この根拠が語っているのは**1 つのリポジトリの複数リモート**の話で
/// あり、`--siblings` の対象は**別々のリポジトリ**であって同じ `.git` を書かない
/// （[`siblings::discover`] が同じワークツリーを重複して返さない）。所要時間は通信の往復に
/// 支配され対象数に比例して伸びるため、ここでは並列化する。覆したのはこの 1 点だけであり、
/// `gz pull` の取り込み（同一リポジトリへの書き込み）と `--siblings` を伴わない `gz fetch`
/// （対象が実質 1 つ）は直列のままである。
///
/// # 2 フェーズ実行
///
/// 1. **並列フェーズ**: 同時に `jobs` 件まで実行する。複数の git の出力が端末で混ざると
///    どのリポジトリのものか読み取れないため、出力は対象ごとにキャプチャして受け取り、
///    進捗行と本文を 1 回のロックでまとめて書き出す。対話（認証情報の入力）はこのフェーズでは
///    構造的に禁じてあるため、passphrase や資格情報を要する対象はここで失敗する
/// 2. **直列フェーズ**: 並列フェーズで失敗した対象**だけ**を、端末を継承した従来どおりの実行で
///    1 件ずつ実行し直す。プロンプトが出ても直前の進捗行でどのリポジトリのものか分かる。
///    **リトライではなく実行は 1 回だけ**であり、自動再試行はしない。並列フェーズでの失敗理由を
///    ここで再掲しないのは、実行し直した git 自身がその場で理由を出すためである（二重に並べない）
///
/// 対象の最終的な成否は「その対象を**最後に**実行したフェーズの結果」であり、二重に数えない。
/// 失敗した対象の一覧は実行順ではなく候補一覧の順に並ぶ（並列フェーズの実行順は決まらないため、
/// 実行順で並べると同じ入力から違う表示が出る）。
///
/// 実行そのものを引数で受け取るのは、ネットワークや git の有無に依存せず集計と中断の判断を
/// 単体テストできるようにするため（本番は表示言語を束ねた
/// [`capture_git_noninteractive_in`] / [`run_git_in`] を渡す）。`parallel` は複数のワーカーから
/// 同時に呼ばれるため `Fn + Sync`、`serial` は 1 本の流れでしか呼ばれないため `FnMut` でよい。
///
/// # Errors
///
/// git の起動自体に失敗した場合（[`Error::GitNotFound`] / [`Error::GitSpawnFailed`]）は
/// 環境の問題でありリポジトリごとの失敗ではないため、**未着手の対象を実行せず**その場で返す
/// （既に走っている git は完了を待つ。強制終了する経路は作らない）。
/// 進捗の書き込みに失敗した場合も同様。
fn fetch_each(
    messages: &dyn Messages,
    targets: &[&SiblingRepository],
    prune: PruneMode,
    jobs: NonZeroUsize,
    writer: &mut (impl std::io::Write + Send),
    parallel: impl Fn(&Path, &[&str]) -> crate::error::Result<CapturedRun> + Sync,
    mut serial: impl FnMut(&Path, &[&str]) -> crate::error::Result<()>,
) -> Result<FetchSummary> {
    let arguments = sibling_fetch_args(prune);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    let outcomes = fetch_in_parallel(messages, targets, &arguments, jobs, writer, parallel)?;

    let mut summary = FetchSummary::default();
    let mut retries: Vec<&SiblingRepository> = Vec::new();
    for (target, outcome) in targets.iter().copied().zip(&outcomes) {
        match outcome {
            Some(Outcome::Succeeded) => summary.succeeded += 1,
            // 結果を残せなかった対象（`None`）も直列フェーズへ回す。中断した場合は上で
            // エラーを返しているためここには来ないが、結果の無いものを成功へ倒さない
            Some(Outcome::Failed) | None => retries.push(target),
        }
    }

    if !retries.is_empty() {
        // 黙って実行し直さない。同じ対象の出力が二度出る理由と、ここから先は対話し得ることを示す
        report_line(
            messages,
            writer,
            &messages.fetch().serial_fallback(retries.len()),
        )?;
    }

    for (index, target) in retries.iter().enumerate() {
        // 認証プロンプトが出た場合にどのリポジトリのものか分かるよう、実行の前に書き出す
        report_line(
            messages,
            writer,
            &progress_line(index, retries.len(), target),
        )?;

        match serial(&target.workdir, &arguments) {
            Ok(()) => summary.succeeded += 1,
            // 個々のリポジトリの失敗（到達不能・認証拒否など）は記録して次へ進む。
            // git 自身が理由をその場で表示済みであり、ここでは再掲しない
            Err(Error::GitRunFailed { .. }) => summary.failed.push(target.name.clone()),
            Err(error) => {
                return Err(anyhow::Error::from(error)
                    .context(messages.fetch().sibling_start_failed(&target.name)));
            }
        }
    }

    Ok(summary)
}

/// 並列フェーズ。対象を候補順に取り出して同時に `jobs` 件まで実行し、対象ごとの結果を返す。
///
/// 戻り値は候補一覧と同じ添字で、`None` は「結果を残せなかった対象」を表す
/// （中断した場合はエラーを返すため、呼び出し側が `None` を受け取ることは無い）。
///
/// # Errors
///
/// [`Error::GitNotFound`] / [`Error::GitSpawnFailed`] を最初に観測したものを、
/// どのリポジトリで起きたかの文脈を添えて返す。進捗の書き込みに失敗した場合も同様。
fn fetch_in_parallel<W: std::io::Write + Send>(
    messages: &dyn Messages,
    targets: &[&SiblingRepository],
    arguments: &[&str],
    jobs: NonZeroUsize,
    writer: &mut W,
    parallel: impl Fn(&Path, &[&str]) -> crate::error::Result<CapturedRun> + Sync,
) -> Result<Vec<Option<Outcome>>> {
    let total = targets.len();
    let shared = Mutex::new(ParallelPhase {
        writer,
        completed: 0,
        outcomes: vec![None; total],
        stopped: false,
        failure: None,
    });

    // 次に取る対象は共有のカウンタで決める。あらかじめ配らずに取りに行かせることで、
    // 対象ごとの所要時間の偏り（通信量が違う）がそのまま待ち時間にならない
    let next = AtomicUsize::new(0);

    // 対象より多くのワーカーを立てても何も取れないまま終わるだけなので、対象数で頭打ちにする
    let workers = jobs.get().min(total);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    // 添字で取り出すことで、範囲の判定と対象の取得を 1 か所に閉じる
                    let Some(target) = targets.get(index) else {
                        break;
                    };

                    // 中断が決まっていれば新しい対象は取らない。ここで止まるのは
                    // **未着手の対象**だけであり、既に走っている git は完了を待つ
                    if locked(&shared).stopped {
                        break;
                    }

                    let result = parallel(&target.workdir, arguments);
                    locked(&shared).record(messages, index, total, target, result);
                }
            });
        }
    });

    let phase = shared
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match phase.failure {
        Some(failure) => Err(failure),
        None => Ok(phase.outcomes),
    }
}

/// 並列フェーズにおける対象 1 件分の結果。
///
/// 「まだ実行していない」はこの列挙に含めず `Option` の `None` で表す。実行していないことと
/// 失敗したことを同じ型で表すと、直列フェーズへ回す判断で取り違えるため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// `git fetch` が正常終了した。
    Succeeded,
    /// `git fetch` が非ゼロ終了した（到達不能・認証拒否・対話が必要になった等）。
    Failed,
}

/// 並列フェーズのワーカーが共有する状態。
///
/// 書き出し先・完了件数・結果・中断の判断を**1 つの [`Mutex`] にまとめて**守る。
/// ロックを分けると進捗行と本文の間に他の対象の出力が割り込み得るため、
/// 「1 対象分の出力は他の対象の出力と混ざらない」という不変条件を構造で担保できなくなる。
struct ParallelPhase<'writer, W> {
    /// 進捗行と git の出力の書き出し先。
    writer: &'writer mut W,
    /// 並列フェーズを終えた対象の件数（成否を問わない）。進捗行の分子になる。
    completed: usize,
    /// 対象ごとの結果（候補一覧と同じ添字）。
    outcomes: Vec<Option<Outcome>>,
    /// 新しい対象を取るのをやめるかどうか。
    stopped: bool,
    /// 最初に観測した中断の理由。
    failure: Option<anyhow::Error>,
}

impl<W: std::io::Write> ParallelPhase<'_, W> {
    /// 対象 1 件の結果を記録し、進捗行と git の出力を続けて書き出す。
    ///
    /// 書き出しをこの 1 か所に閉じ、呼び出し側がロックを取ったまま 1 回だけ呼ぶことで、
    /// 進捗行と本文の間に他の対象の出力が割り込む余地を無くす。
    fn record(
        &mut self,
        messages: &dyn Messages,
        index: usize,
        total: usize,
        target: &SiblingRepository,
        result: crate::error::Result<CapturedRun>,
    ) {
        let run = match result {
            Ok(run) => Some(run),
            // 個々のリポジトリの失敗は直列フェーズへ回す。理由はそこで git 自身が示すため、
            // このフェーズでは画面へ出さない（[`Error::GitRunFailed`] は出力を持たない）
            Err(Error::GitRunFailed { .. }) => None,
            // 環境の問題はリポジトリごとの失敗ではないため、未着手の対象を実行せずに止める
            Err(error) => {
                self.stop(
                    anyhow::Error::from(error)
                        .context(messages.fetch().sibling_start_failed(&target.name)),
                );
                return;
            }
        };

        self.completed += 1;
        let outcome = match run {
            Some(_) => Outcome::Succeeded,
            None => Outcome::Failed,
        };
        // 添字は必ず範囲内（ワーカーは `targets.get` で取れた添字しか渡さない）。`[]` で書くと
        // 範囲外が panic になるため、結果を落として直列フェーズへ回す形にしてある
        if let Some(slot) = self.outcomes.get_mut(index) {
            *slot = Some(outcome);
        }

        let line = progress_line(self.completed - 1, total, target);
        if let Err(failure) = self.write_block(messages, &line, run.as_ref()) {
            self.stop(failure);
        }
    }

    /// 進捗行と git の出力を続けて書き出す。
    ///
    /// # Errors
    ///
    /// 書き込みに失敗した場合にエラーを返す。
    fn write_block(
        &mut self,
        messages: &dyn Messages,
        line: &str,
        run: Option<&CapturedRun>,
    ) -> Result<()> {
        report_line(messages, self.writer, line)?;

        if let Some(run) = run {
            // 中身を解釈せずそのまま渡す（fuzgit は git の出力を読まない）。標準出力と
            // 標準エラーの相対順序は保てないが、`git fetch` の更新表は実質すべて標準エラーへ
            // 出るため、読み手には 1 続きの本文に見える
            self.writer
                .write_all(&run.stdout)
                .and_then(|()| self.writer.write_all(&run.stderr))
                .context(messages.common().stderr_write_failed())?;
        }

        Ok(())
    }

    /// 中断を決める。理由は最初に観測したものだけを残す。
    ///
    /// 上書きしないのは、後から観測した理由が「最初の中断に巻き込まれた結果」であり得るため
    /// （git を起動できない環境では、走っている対象の数だけ同じエラーが並ぶ）。
    fn stop(&mut self, failure: anyhow::Error) {
        self.stopped = true;
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
    }
}

/// 並列フェーズの共有状態のロックを取る。
///
/// ロックが毒されている（ワーカーの内側で panic した）場合も中身をそのまま使う。守っているのは
/// 書き出し先と集計だけであり、途中で panic しても解釈できない状態は残らない。ここで panic を
/// 重ねると、[`std::thread::scope`] が本来伝播させる最初の panic を覆い隠してしまう。
fn locked<T>(shared: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `git fetch [--prune] --all` の引数を組み立てる。
///
/// 対象は登録されているすべてのリモート（リポジトリごとにリモートを選ばせない）。
fn sibling_fetch_args(prune: PruneMode) -> Vec<String> {
    let mut args = vec!["fetch".to_owned()];
    if let Some(option) = prune.option() {
        args.push(option.to_owned());
    }
    args.push(ALL_REMOTES_OPTION.to_owned());

    args
}

/// 進捗の 1 行（`[<位置>/<全体>] <名前>`）を組み立てる。
///
/// `index` は 0 起点の位置であり、フェーズによって何番目かの意味が変わる。並列フェーズでは
/// **完了した件数 − 1**（実行の開始順は決まらないため、完了した順に番号を振る）、直列フェーズでは
/// 実行の前に書き出すため**これから実行する対象の位置**である。書式はどちらも同じで、
/// 読み手には「全体のうち何件目か」だけが見える。
fn progress_line(index: usize, total: usize, target: &SiblingRepository) -> String {
    format!(
        "[{position}/{total}] {name}",
        position = index + 1,
        name = target.name
    )
}

/// 実行結果の集計を 1 行に組み立てる。
fn summary_line(messages: &dyn Messages, summary: &FetchSummary) -> String {
    let mut line = messages
        .common()
        .run_summary(summary.succeeded, summary.failed.len());

    if summary.has_failure() {
        line.push_str(&messages.common().failed_targets(&summary.failed.join(", ")));
    }

    line
}

/// 1 行を書き出す。
///
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_line(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    line: &str,
) -> Result<()> {
    writeln!(writer, "{line}").context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// finder を省略して確定した対象を伝える 1 行を組み立てる。
fn fixed_target_message(messages: &dyn Messages, target: &FetchTarget) -> String {
    messages.fetch().fixed_target(&target.description(messages))
}

/// finder を省略して確定した対象を書き出す。
///
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_target(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    target: &FetchTarget,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = fixed_target_message(messages, target)
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// 候補（リモート一覧＋固定候補「すべてのリモート」）を組み立てる。
///
/// 組み立てるのはリモートが 2 つ以上あり、選択の余地がある場合だけ
/// （1 つの場合は [`FetchDecision::from_remotes`] が finder を経ずに対象を確定する）。
/// 固定候補は「個々のリモート」と「すべてのリモート」で `git fetch` の引数が
/// 変わるために必要であり、リモートの後ろに置いて位置を一定に保つ。
fn items(language: Language, messages: &dyn Messages, remotes: &[String]) -> Vec<FinderItem> {
    let mut items: Vec<FinderItem> = remotes
        .iter()
        .map(|remote| to_item(language, messages, remote))
        .collect();
    items.push(all_remotes_item(language, messages, remotes));
    items
}

/// リモート 1 件を finder の候補へ変換する。
fn to_item(language: Language, messages: &dyn Messages, remote: &str) -> FinderItem {
    FinderItem::new(
        remote.to_owned(),
        remote.to_owned(),
        preview_source(messages, remote),
        language.messages(),
    )
}

/// リモート 1 件のプレビュー内容を組み立てる。
///
/// 参照するのは `.git/config` の URL と、前回までの fetch で保存済みのリモート追跡参照だけで、
/// いずれもネットワークを伴わない。生成は他コマンドと同じく選択項目ごとの遅延実行であり、
/// カーソルが当たっていない候補の分は実行されない。
fn preview_source(messages: &dyn Messages, remote: &str) -> PreviewSource {
    PreviewSource::Composite(vec![
        (
            messages.fetch().url_section().to_owned(),
            PreviewSource::Git(remote_url_args(remote)),
        ),
        (
            messages.fetch().tracking_section().to_owned(),
            PreviewSource::Git(remote_tracking_refs_args(remote)),
        ),
    ])
}

/// 「すべてのリモート」の固定候補を組み立てる。
fn all_remotes_item(language: Language, messages: &dyn Messages, remotes: &[String]) -> FinderItem {
    FinderItem::new(
        messages.fetch().all_remotes_label().to_owned(),
        ALL_REMOTES_KEY.to_owned(),
        all_remotes_preview(remotes),
        language.messages(),
    )
}

/// 「すべてのリモート」候補のプレビュー内容を組み立てる。
///
/// リモートごとの URL だけを並べる。追跡参照まで並べるとリモート数に比例して
/// プレビューの git 実行が増えるうえ、この候補で確かめたいこと（どのリモートへ
/// 問い合わせに行くのか）から離れるため。
fn all_remotes_preview(remotes: &[String]) -> PreviewSource {
    PreviewSource::Composite(
        remotes
            .iter()
            .map(|remote| (remote.clone(), PreviewSource::Git(remote_url_args(remote))))
            .collect(),
    )
}

/// 選択されたキーを fetch の対象へ解決する。
///
/// # Errors
///
/// キーが固定候補でも候補一覧のリモートでもない場合にエラーを返す
/// （対象を取り違えたまま git を実行しないよう、暗黙に読み飛ばさない）。
fn resolve(messages: &dyn Messages, remotes: &[String], selected: &str) -> Result<FetchTarget> {
    if selected == ALL_REMOTES_KEY {
        return Ok(FetchTarget::All);
    }

    remotes
        .iter()
        .find(|remote| *remote == selected)
        .map(|remote| FetchTarget::Remote(remote.clone()))
        .ok_or_else(|| anyhow!(messages.fetch().selection_not_found(selected)))
}

/// `git fetch [--prune] <remote>` / `git fetch [--prune] --all` の引数を組み立てる。
///
/// リモート名は gix が列挙した候補に由来する値だけを渡す。
fn fetch_args(target: &FetchTarget, prune: PruneMode) -> Vec<String> {
    let mut args = vec!["fetch".to_owned()];
    if let Some(option) = prune.option() {
        args.push(option.to_owned());
    }

    match target {
        FetchTarget::Remote(name) => args.push(name.clone()),
        FetchTarget::All => args.push(ALL_REMOTES_OPTION.to_owned()),
    }

    args
}

#[cfg(test)]
mod tests {
    use skim::prelude::SkimItem as _;

    use super::*;
    use crate::commands::COLUMN_SEPARATOR;

    /// 既定（日本語）の文言一式。文言そのものを固定するテスト以外はこれを使う。
    fn messages() -> &'static dyn Messages {
        Language::Japanese.messages()
    }

    fn remotes() -> Vec<String> {
        ["origin", "upstream"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// 開発者の `~/.gitconfig` に影響されずに検証するため、リポジトリ内の設定だけを
    /// 読み込む形でリポジトリを開く（`i18n::resolve` の設定テストと同じ方針）。
    fn open_isolated(path: &Path) -> gix::Repository {
        gix::open_opts(path, gix::open::Options::isolated())
            .expect("initialized repository must be openable")
    }

    #[test]
    fn the_default_number_of_jobs_is_four() {
        // CPU 数に比例させない固定値であるため、値そのものを固定して意図しない変更を検出する
        assert_eq!(DEFAULT_FETCH_JOBS.get(), 4);
    }

    #[test]
    fn an_unset_fetch_jobs_setting_falls_back_to_the_default() {
        assert_eq!(
            parse_fetch_jobs(None).expect("an unset value must be accepted"),
            DEFAULT_FETCH_JOBS
        );
    }

    #[test]
    fn an_empty_fetch_jobs_setting_is_treated_as_unset() {
        // 空文字の扱いは `FUZGIT_LANG` / `fuzgit.lang` と揃える
        assert_eq!(
            parse_fetch_jobs(Some("")).expect("an empty value must be treated as unset"),
            DEFAULT_FETCH_JOBS
        );
    }

    #[test]
    fn a_positive_fetch_jobs_setting_is_used_as_written() {
        for (value, expected) in [("1", 1), ("8", 8)] {
            let jobs = parse_fetch_jobs(Some(value)).expect("a positive integer must be accepted");
            assert_eq!(jobs.get(), expected, "{value} must be used as written");
        }
    }

    #[test]
    fn zero_jobs_is_rejected_instead_of_falling_back_to_the_default() {
        // 0 を既定値へ倒すと、指定したつもりの同時実行数と実際の動作が食い違ったまま通信が始まる
        let error = parse_fetch_jobs(Some("0")).expect_err("zero must stop the command");

        match error {
            Error::InvalidFetchJobs { value } => assert_eq!(value, "0"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_negative_fetch_jobs_setting_is_rejected() {
        let error =
            parse_fetch_jobs(Some("-1")).expect_err("a negative value must stop the command");

        match error {
            Error::InvalidFetchJobs { value } => assert_eq!(value, "-1"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_fetch_jobs_setting_that_is_not_an_integer_is_rejected() {
        let error = parse_fetch_jobs(Some("abc")).expect_err("a non-integer must stop the command");

        match error {
            // 読み取った値をそのまま保持する（何を直せばよいかを示せるようにするため）
            Error::InvalidFetchJobs { value } => assert_eq!(value, "abc"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn the_number_of_jobs_is_read_from_the_repository_configuration() {
        use crate::test_support::{TempDir, git_in, init_repository};

        let dir = TempDir::new("fetch-jobs-config");
        init_repository(dir.path());
        git_in(dir.path(), &["config", FETCH_JOBS_CONFIG_KEY, "8"]);

        let jobs = fetch_jobs(&open_isolated(dir.path())).expect("the setting should be readable");

        assert_eq!(jobs.get(), 8);
    }

    #[test]
    fn an_unset_configuration_key_is_reported_as_absent() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("fetch-jobs-unset");
        init_repository(dir.path());

        assert_eq!(fetch_jobs_setting(&open_isolated(dir.path())), None);
    }

    #[test]
    fn an_invalid_configured_value_stops_before_anything_runs() {
        use crate::test_support::{TempDir, git_in, init_repository};

        let dir = TempDir::new("fetch-jobs-invalid");
        init_repository(dir.path());
        git_in(dir.path(), &["config", FETCH_JOBS_CONFIG_KEY, "0"]);

        let error = fetch_jobs(&open_isolated(dir.path()))
            .expect_err("an invalid setting must stop the command");

        match error {
            Error::InvalidFetchJobs { value } => assert_eq!(value, "0"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn fetching_a_remote_passes_its_name_as_it_was_listed() {
        assert_eq!(
            fetch_args(&FetchTarget::Remote("origin".to_owned()), PruneMode::Keep),
            ["fetch", "origin"]
        );
    }

    #[test]
    fn fetching_everything_uses_the_all_option_instead_of_a_name() {
        let arguments = fetch_args(&FetchTarget::All, PruneMode::Keep);

        assert_eq!(arguments, ["fetch", ALL_REMOTES_OPTION]);
        assert!(
            !arguments.iter().any(|argument| argument == ALL_REMOTES_KEY),
            "the finder key must never reach git: {arguments:?}"
        );
    }

    #[test]
    fn pruning_is_added_only_when_it_was_asked_for() {
        assert_eq!(
            fetch_args(&FetchTarget::Remote("origin".to_owned()), PruneMode::Prune),
            ["fetch", PRUNE_OPTION, "origin"]
        );
        assert_eq!(
            fetch_args(&FetchTarget::All, PruneMode::Prune),
            ["fetch", PRUNE_OPTION, ALL_REMOTES_OPTION]
        );
    }

    #[test]
    fn the_tracking_references_are_left_alone_unless_pruning_was_asked_for() {
        assert_eq!(PruneMode::Keep.option(), None);
        assert_eq!(PruneMode::Prune.option(), Some(PRUNE_OPTION));
    }

    #[test]
    fn a_remote_name_resolves_to_that_remote() {
        assert_eq!(
            resolve(messages(), &remotes(), "upstream").expect("a listed remote should resolve"),
            FetchTarget::Remote("upstream".to_owned())
        );
    }

    #[test]
    fn the_fixed_key_resolves_to_every_remote() {
        assert_eq!(
            resolve(messages(), &remotes(), ALL_REMOTES_KEY).expect("the fixed key should resolve"),
            FetchTarget::All
        );
    }

    #[test]
    fn a_name_outside_of_the_candidates_is_rejected() {
        let err = resolve(messages(), &remotes(), "elsewhere")
            .expect_err("an unknown remote must be rejected");

        assert!(
            err.to_string().contains("elsewhere"),
            "the unknown remote should be named: {err:#}"
        );
    }

    #[test]
    fn the_label_of_the_fixed_candidate_is_not_mistaken_for_a_remote() {
        // 表示文字列と同じ名前のリモートがあっても、解決に使うのはキーだけ
        let label = messages().fetch().all_remotes_label();
        let remotes = vec![label.to_owned()];

        assert_eq!(
            resolve(messages(), &remotes, label).expect("the remote should resolve"),
            FetchTarget::Remote(label.to_owned())
        );
        assert_eq!(
            resolve(messages(), &remotes, ALL_REMOTES_KEY).expect("the fixed key should resolve"),
            FetchTarget::All
        );
    }

    #[test]
    fn the_fixed_candidate_comes_after_the_remotes() {
        let items = items(Language::Japanese, messages(), &remotes());

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            ["origin", "upstream", ALL_REMOTES_KEY]
        );
    }

    #[test]
    fn a_single_remote_is_fetched_without_starting_the_finder() {
        // 唯一のリモートと「すべてのリモート」の 2 択は、どちらを選んでも通信先が同じ
        assert_eq!(
            FetchDecision::from_remotes(&["origin".to_owned()]),
            FetchDecision::Fixed(FetchTarget::Remote("origin".to_owned()))
        );
    }

    #[test]
    fn several_remotes_are_left_to_the_finder() {
        assert_eq!(
            FetchDecision::from_remotes(&remotes()),
            FetchDecision::Choose
        );
    }

    #[test]
    fn three_remotes_are_left_to_the_finder_as_well() {
        let remotes = ["origin", "upstream", "fork"].map(str::to_owned);

        assert_eq!(FetchDecision::from_remotes(&remotes), FetchDecision::Choose);
    }

    #[test]
    fn no_remote_at_all_is_decided_before_any_selection() {
        assert_eq!(FetchDecision::from_remotes(&[]), FetchDecision::NoRemote);
    }

    #[test]
    fn the_fixed_target_is_named_in_the_line_that_replaces_the_finder() {
        // fetch は更新が無ければ何も出力しないことがあるため、通信先をここで示す
        let message = fixed_target_message(messages(), &FetchTarget::Remote("origin".to_owned()));

        assert!(
            message.contains("origin"),
            "the remote should be named: {message}"
        );
        assert!(
            message.contains("選択を省略しました"),
            "the reason the finder was skipped should be given: {message}"
        );
        assert_eq!(message.lines().count(), 1, "1 行に収める: {message}");
    }

    #[test]
    fn the_fixed_target_is_reported_to_the_given_writer() {
        let mut written = Vec::new();

        report_target(
            messages(),
            &mut written,
            &FetchTarget::Remote("origin".to_owned()),
        )
        .expect("writing to a buffer should succeed");

        let text = String::from_utf8(written).expect("the message should be utf-8");
        assert_eq!(
            text,
            format!(
                "{message}\n",
                message =
                    fixed_target_message(messages(), &FetchTarget::Remote("origin".to_owned()))
            )
        );
    }

    #[test]
    fn the_fixed_target_of_a_single_remote_fetches_only_that_remote() {
        // 1 件確定の経路でも引数の組み立ては共通（`--all` へ倒さない）
        let FetchDecision::Fixed(target) = FetchDecision::from_remotes(&["origin".to_owned()])
        else {
            panic!("a single remote should be fixed without a selection");
        };

        assert_eq!(fetch_args(&target, PruneMode::Keep), ["fetch", "origin"]);
        assert_eq!(
            fetch_args(&target, PruneMode::Prune),
            ["fetch", PRUNE_OPTION, "origin"]
        );
    }

    /// プレビューのセクション（見出しと実行する git 引数）を取り出す。
    fn sections(source: &PreviewSource) -> Vec<(String, Vec<String>)> {
        let PreviewSource::Composite(sections) = source else {
            panic!("unexpected preview: {source:?}");
        };

        sections
            .iter()
            .map(|(label, source)| {
                let PreviewSource::Git(arguments) = source else {
                    panic!("`{label}` should run git: {source:?}");
                };
                (label.clone(), arguments.clone())
            })
            .collect()
    }

    #[test]
    fn a_preview_reads_local_information_only() {
        // ネットワークへ出るのは決定後の `git fetch` だけ（design.md の設計原則）
        let sections = sections(&preview_source(messages(), "origin"));

        assert_eq!(
            sections
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            [
                messages().fetch().url_section(),
                messages().fetch().tracking_section()
            ]
        );
        assert_eq!(sections[0].1, remote_url_args("origin"));
        assert_eq!(sections[1].1, remote_tracking_refs_args("origin"));
    }

    #[test]
    fn no_preview_reaches_the_network() {
        let previews = [
            preview_source(messages(), "origin"),
            all_remotes_preview(&remotes()),
        ];

        for preview in &previews {
            for (label, arguments) in sections(preview) {
                assert!(
                    matches!(arguments[0].as_str(), "remote" | "for-each-ref"),
                    "`{label}` must not reach the network: {arguments:?}"
                );
                assert!(
                    !arguments.iter().any(|argument| argument == "fetch"
                        || argument == "ls-remote"
                        || argument == "--dry-run"),
                    "`{label}` must not query the remote: {arguments:?}"
                );
            }
        }
    }

    #[test]
    fn the_preview_of_every_remote_lists_each_url_under_its_own_name() {
        let sections = sections(&all_remotes_preview(&remotes()));

        assert_eq!(
            sections
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["origin", "upstream"]
        );
        assert_eq!(sections[0].1, remote_url_args("origin"));
        assert_eq!(sections[1].1, remote_url_args("upstream"));
    }

    #[test]
    fn the_target_is_named_in_the_failure_message() {
        assert_eq!(
            FetchTarget::Remote("origin".to_owned()).description(messages()),
            "リモート `origin`"
        );
        assert_eq!(
            FetchTarget::All.description(messages()),
            messages().fetch().all_remotes_label()
        );
    }

    // --- 兄弟リポジトリ（FR-23） ---

    /// 兄弟リポジトリの候補を組み立てる。
    fn sibling(name: &str, is_current: bool) -> SiblingRepository {
        SiblingRepository {
            workdir: std::path::PathBuf::from(format!("/repos/{name}")),
            name: name.to_owned(),
            current_branch: Some("main".to_owned()),
            remotes: vec!["origin".to_owned()],
            is_current,
        }
    }

    /// 現在のリポジトリ 1 件と兄弟 2 件の候補一覧（`discover()` と同じ並び）。
    fn siblings() -> Vec<SiblingRepository> {
        vec![
            sibling("mike", true),
            sibling("alpha", false),
            sibling("zulu", false),
        ]
    }

    /// 候補 1 件だけの表示行（揃える相手が居ないため列を連結しただけの行）。
    fn sibling_line(candidate: &SiblingRepository) -> String {
        sibling_cells(candidate).join(COLUMN_SEPARATOR)
    }

    /// 走査結果を組み立てる。
    fn scan(candidates: Vec<SiblingRepository>, excluded: usize) -> SiblingScan {
        SiblingScan {
            candidates,
            excluded,
        }
    }

    /// 候補のキーを並び順のまま取り出す。
    fn keys(targets: &[&SiblingRepository]) -> Vec<String> {
        targets
            .iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>()
    }

    /// 引数配列から git のサブコマンド（先頭のオプション群の次に来る引数）を取り出す。
    fn subcommand(arguments: &[String]) -> &str {
        arguments
            .iter()
            .map(String::as_str)
            .find(|argument| !argument.starts_with('-'))
            .unwrap_or_default()
    }

    /// プレビューのセクション（見出し・実行ディレクトリ・git 引数）を取り出す。
    fn git_in_sections(source: &PreviewSource) -> Vec<(String, std::path::PathBuf, Vec<String>)> {
        let PreviewSource::Composite(sections) = source else {
            panic!("unexpected preview: {source:?}");
        };

        sections
            .iter()
            .map(|(label, source)| {
                let PreviewSource::GitIn { directory, args } = source else {
                    panic!("`{label}` should run git in the sibling: {source:?}");
                };
                (label.clone(), directory.clone(), args.clone())
            })
            .collect()
    }

    /// 右端の列（取り込み先）の範囲を、期待値として組み立てる。
    ///
    /// 実装（`last_column_range`）と同じ不変条件を使うため、**計算の独立性は無い**。
    /// このヘルパが固定するのは「その範囲が意図した文字列を覆っていること」であり、
    /// 不変条件そのものは `commands::mod` 側の
    /// `the_last_column_is_the_suffix_of_the_formatted_line` が
    /// `aligned_candidates` の実出力に対して固定する。
    fn destination_range(line: &str, content: &str) -> (usize, usize) {
        let trimmed = line.trim_end();
        (trimmed.len() - content.len(), trimmed.len())
    }

    #[test]
    fn the_stream_destination_column_is_coloured() {
        // 色を付けるのは「どこから取り込むことになるのか」を示す右端の列だけ。
        // 左のディレクトリ名は絞り込みの主対象であり、一覧の大半を占めるため付けない
        let candidate = sibling("api", false);
        let lines = aligned_candidates(std::slice::from_ref(&candidate), sibling_cells);
        let (_, line) = &lines[0];
        let (start, end) = destination_range(line, "origin/main");

        assert_eq!(
            sibling_highlights(line, &candidate),
            [Highlight::new(start, end, HighlightColor::Blue)]
        );
    }

    #[test]
    fn a_detached_repository_marks_its_state_instead() {
        // detached には対にするブランチが無く、取り込み先を示せない。青ではなく注意の黄にする
        let mut candidate = sibling("api", false);
        candidate.current_branch = None;
        let lines = aligned_candidates(std::slice::from_ref(&candidate), sibling_cells);
        let (_, line) = &lines[0];
        let (start, end) = destination_range(line, DETACHED_LABEL);

        assert_eq!(
            sibling_highlights(line, &candidate),
            [Highlight::new(start, end, HighlightColor::Yellow)]
        );
    }

    #[test]
    fn the_coloured_range_survives_a_multibyte_directory_name() {
        // 範囲はバイト位置。列の幅は候補一覧全体で決まるため、整形後の行から求める
        let mut wide = sibling("日本語のリポジトリ", false);
        wide.remotes = vec!["origin".to_owned(), "upstream".to_owned()];
        let candidates = vec![wide, sibling("api", false)];
        let lines = aligned_candidates(&candidates, sibling_cells);

        for (candidate, line) in &lines {
            let content = sibling_cells(candidate)
                .pop()
                .expect("a candidate line has columns");
            let (start, end) = destination_range(line, &content);

            assert_eq!(
                sibling_highlights(line, candidate),
                [Highlight::new(start, end, HighlightColor::Blue)],
                "the destination column should be selected in {line:?}"
            );
        }
    }

    #[test]
    fn a_candidate_line_pairs_every_remote_with_the_branch() {
        let mut candidate = sibling("alpha", false);
        candidate.remotes = vec!["origin".to_owned(), "upstream".to_owned()];

        let line = sibling_line(&candidate);

        assert_eq!(line, "alpha  origin/main, upstream/main");
    }

    #[test]
    fn a_candidate_line_shows_the_directory_and_the_remote_branch() {
        let mut candidate = sibling("advent-calendar", false);
        candidate.current_branch = Some("master".to_owned());

        let line = sibling_line(&candidate);

        assert_eq!(line, "advent-calendar  origin/master");
    }

    #[test]
    fn a_detached_head_is_shown_as_such_instead_of_a_branch() {
        let mut candidate = sibling("alpha", false);
        candidate.current_branch = None;

        let line = sibling_line(&candidate);

        assert!(
            line.contains(DETACHED_LABEL),
            "a detached HEAD should be named: {line}"
        );
        // 対にするブランチが無いため、リモートは単独で並べる
        assert_eq!(line, "alpha  origin  detached HEAD");
    }

    #[test]
    fn the_header_states_how_many_repositories_were_left_out() {
        let header = sibling_header(messages(), &scan(siblings(), 2), PruneMode::Keep);

        assert!(header.contains('2'), "the count should be shown: {header}");
        assert!(
            header.contains("除外"),
            "the exclusion should be named: {header}"
        );
        assert_eq!(header.lines().count(), 1, "1 行に収める: {header}");
    }

    #[test]
    fn nothing_is_said_about_exclusions_when_there_were_none() {
        let header = sibling_header(messages(), &scan(siblings(), 0), PruneMode::Keep);

        assert!(
            !header.contains("除外"),
            "an exclusion must not be implied: {header}"
        );
    }

    #[test]
    fn the_header_states_that_pruning_applies_to_every_selected_repository() {
        let pruning = sibling_header(messages(), &scan(siblings(), 0), PruneMode::Prune);
        let keeping = sibling_header(messages(), &scan(siblings(), 0), PruneMode::Keep);

        assert!(
            pruning.contains(messages().fetch().prune_scope_note()),
            "the scope of --prune should be shown: {pruning}"
        );
        assert!(
            !keeping.contains(PRUNE_OPTION),
            "--prune must not be implied when it was not asked for: {keeping}"
        );
    }

    #[test]
    fn a_lone_current_repository_is_fetched_without_starting_the_finder() {
        assert_eq!(
            SiblingsDecision::from_candidates(&[sibling("mike", true)]),
            SiblingsDecision::Fixed
        );
    }

    #[test]
    fn a_lone_sibling_is_still_offered_for_selection() {
        // 現在のリポジトリが除外された場合。他人のリポジトリへ通信するかは見てから決める
        assert_eq!(
            SiblingsDecision::from_candidates(&[sibling("alpha", false)]),
            SiblingsDecision::Choose
        );
    }

    #[test]
    fn several_candidates_are_left_to_the_finder() {
        assert_eq!(
            SiblingsDecision::from_candidates(&siblings()),
            SiblingsDecision::Choose
        );
    }

    #[test]
    fn no_candidate_at_all_is_decided_before_any_selection() {
        assert_eq!(
            SiblingsDecision::from_candidates(&[]),
            SiblingsDecision::NoCandidate
        );
    }

    #[test]
    fn only_the_current_repository_is_preselected() {
        let candidates = siblings();
        let lines = aligned_candidates(&candidates, sibling_cells);

        assert_eq!(
            preselect(&lines),
            vec!["mike   origin/main".to_owned()],
            "選択済みにするのは現在のリポジトリだけ: {lines:?}"
        );
    }

    #[test]
    fn the_preselected_line_is_the_very_line_shown_in_the_list() {
        // 事前選択は表示文字列の完全一致で判定されるため、列を揃えたあとの行と
        // 一致していなければ機能しない（`crate::finder::FinderOptions::preselect`）
        let candidates = vec![sibling("mike", true), sibling("advent-calendar", false)];
        let lines = aligned_candidates(&candidates, sibling_cells);

        let items = sibling_items(Language::Japanese, messages(), &lines)
            .expect("a utf-8 path should be usable as a key");
        let displayed: Vec<String> = items.iter().map(|item| item.text().into_owned()).collect();

        let preselected = preselect(&lines);
        assert_eq!(
            preselected.len(),
            1,
            "unexpected preselection: {preselected:?}"
        );
        for line in &preselected {
            assert!(
                displayed.contains(line),
                "the preselection must be one of the listed lines: {line:?} / {displayed:?}"
            );
        }
        // 列を揃える前の行では一致しないこと（＝別々に組み立てると壊れること）も確かめる
        assert!(
            !preselected.contains(&sibling_line(&candidates[0])),
            "the padding is what makes the two ways differ: {preselected:?}"
        );
    }

    #[test]
    fn a_candidate_is_keyed_by_its_normalized_path() {
        let candidates = siblings();

        let lines = aligned_candidates(&candidates, sibling_cells);

        let items = sibling_items(Language::Japanese, messages(), &lines)
            .expect("a utf-8 path should be usable as a key");

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            ["/repos/mike", "/repos/alpha", "/repos/zulu"]
        );
    }

    #[test]
    fn the_preview_takes_no_optional_locks() {
        let arguments = sibling_tracking_args();

        assert_eq!(
            arguments.first().map(String::as_str),
            Some(NO_OPTIONAL_LOCKS),
            "the option must come before the subcommand: {arguments:?}"
        );
        assert_eq!(arguments[1..], branch_tracking_args()[..]);
    }

    #[test]
    fn no_sibling_preview_walks_the_work_tree() {
        // `git status` は追跡状況を出す前に index を refresh する（全ファイルを stat し、
        // 食い違えば内容を読み直す）ため、規模の大きい兄弟でプレビューが秒単位になる。
        // プレビューはカーソル移動のたびに同期実行されるので、走査を伴う経路を持たせない
        let sections = git_in_sections(&sibling_preview_source(
            messages(),
            &sibling("alpha", false),
        ));

        for (label, _, arguments) in sections {
            assert!(
                matches!(subcommand(&arguments), "remote" | "for-each-ref"),
                "`{label}` must not walk the work tree: {arguments:?}"
            );
        }
    }

    #[test]
    fn the_preview_runs_in_the_repository_of_the_candidate() {
        let candidate = sibling("alpha", false);

        let sections = git_in_sections(&sibling_preview_source(messages(), &candidate));

        for (label, directory, arguments) in &sections {
            assert_eq!(
                directory, &candidate.workdir,
                "`{label}` should run in the candidate's work tree"
            );
            assert!(
                !arguments
                    .iter()
                    .any(|argument| argument == &candidate.workdir.display().to_string()),
                "`{label}` must pass the directory as the cwd, not as an argument: {arguments:?}"
            );
        }

        assert_eq!(
            sections
                .iter()
                .map(|(label, _, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["origin", messages().fetch().tracking_state_section()]
        );
    }

    #[test]
    fn no_sibling_preview_reaches_the_network() {
        let sections = git_in_sections(&sibling_preview_source(
            messages(),
            &sibling("alpha", false),
        ));

        for (label, _, arguments) in sections {
            assert!(
                !arguments.iter().any(|argument| argument == "fetch"
                    || argument == "ls-remote"
                    || argument == "--dry-run"),
                "`{label}` must not query the remote: {arguments:?}"
            );
        }
    }

    #[test]
    fn the_selection_is_put_back_into_candidate_order() {
        let candidates = siblings();
        // skim は選択した順に返す
        let selected = ["/repos/zulu", "/repos/mike"].map(str::to_owned);

        let targets = in_candidate_order(messages(), &candidates, &selected)
            .expect("keys taken from the candidates should resolve");

        assert_eq!(keys(&targets), ["mike", "zulu"]);
    }

    #[test]
    fn a_key_outside_of_the_candidates_is_rejected() {
        let candidates = siblings();
        let selected = ["/repos/mike".to_owned(), "/elsewhere/evil".to_owned()];

        let err = in_candidate_order(messages(), &candidates, &selected)
            .expect_err("an unknown repository must be rejected");

        assert!(
            err.to_string().contains("/elsewhere/evil"),
            "the unknown repository should be named: {err:#}"
        );
    }

    #[test]
    fn a_partial_prefix_of_a_candidate_path_is_not_accepted() {
        let candidates = siblings();
        let selected = ["/repos/mik".to_owned()];

        assert!(
            in_candidate_order(messages(), &candidates, &selected).is_err(),
            "the key must match a candidate exactly"
        );
    }

    #[test]
    fn every_sibling_is_fetched_from_all_of_its_remotes() {
        assert_eq!(
            sibling_fetch_args(PruneMode::Keep),
            ["fetch", ALL_REMOTES_OPTION]
        );
    }

    #[test]
    fn pruning_applies_to_the_sibling_fetch_as_well() {
        assert_eq!(
            sibling_fetch_args(PruneMode::Prune),
            ["fetch", PRUNE_OPTION, ALL_REMOTES_OPTION]
        );
    }

    #[test]
    fn the_progress_line_counts_from_one() {
        let candidates = siblings();

        // 並列フェーズの引数は「完了した件数 − 1」であり、候補一覧の位置ではない。
        // 2 番目に完了したのが 3 つ目の候補なら `[2/3]` と出る
        assert_eq!(progress_line(0, 3, &candidates[0]), "[1/3] mike");
        assert_eq!(progress_line(1, 3, &candidates[2]), "[2/3] zulu");
        assert_eq!(progress_line(2, 3, &candidates[1]), "[3/3] alpha");
    }

    /// 同時実行数（テストは 1 以上の定数しか渡さない）。
    fn jobs(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).expect("the test must ask for at least one job")
    }

    /// 実行ディレクトリからリポジトリ名を取り出す（`sibling()` が `/repos/<名前>` を作る）。
    fn name_of(directory: &Path) -> String {
        directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// 名前を並べた候補一覧（`discover()` と同じく重複しないワークツリー）。
    fn siblings_named(names: &[&str]) -> Vec<SiblingRepository> {
        names.iter().map(|name| sibling(name, false)).collect()
    }

    /// 同時に実行されていた件数の推移。
    #[derive(Debug, Default, Clone, Copy)]
    struct Concurrency {
        /// いま実行中の件数。
        running: usize,
        /// これまでに同時実行された件数の最大値。
        peak: usize,
    }

    /// 実行器の呼び出しの記録。
    ///
    /// 並列フェーズの実行器は複数のワーカーから同時に呼ばれるため `Fn + Sync` でなければならず、
    /// `Rc<RefCell<…>>`（`Sync` でない）は使えない。記録はすべて `Mutex` で守る。
    #[derive(Debug, Default)]
    struct Recorder {
        /// 呼び出しの記録（実行ディレクトリと git 引数）。並列フェーズの実行順は決まらないため、
        /// 順序に依存する検査は `jobs = 1` のときだけ行う。
        calls: Mutex<Vec<(std::path::PathBuf, Vec<String>)>>,
        /// 同時実行の観測。
        concurrency: Mutex<Concurrency>,
    }

    impl Recorder {
        /// 呼び出しを記録し、実行中として数える。
        fn enter(&self, directory: &Path, args: &[&str]) {
            locked(&self.calls).push((
                directory.to_path_buf(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));

            let mut concurrency = locked(&self.concurrency);
            concurrency.running += 1;
            concurrency.peak = concurrency.peak.max(concurrency.running);
        }

        /// 実行が終わったことを記録する。
        fn leave(&self) {
            locked(&self.concurrency).running -= 1;
        }

        /// 呼び出された対象の名前（記録順）。
        fn names(&self) -> Vec<String> {
            locked(&self.calls)
                .iter()
                .map(|(directory, _)| name_of(directory))
                .collect()
        }

        /// 呼び出しの回数。
        fn count(&self) -> usize {
            locked(&self.calls).len()
        }

        /// 同時に実行されていた件数の最大値。
        fn peak(&self) -> usize {
            locked(&self.concurrency).peak
        }

        /// 呼び出しごとの git 引数。
        fn arguments(&self) -> Vec<Vec<String>> {
            locked(&self.calls)
                .iter()
                .map(|(_, arguments)| arguments.clone())
                .collect()
        }
    }

    /// リポジトリ 1 件分の fetch の失敗（非ゼロ終了）。
    fn run_failure() -> Error {
        Error::GitRunFailed {
            command: "git fetch".to_owned(),
            code: Some(128),
        }
    }

    /// 出力の無いキャプチャ結果（多くのテストは出力の中身を見ない）。
    fn silent_run() -> CapturedRun {
        CapturedRun {
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    /// 並列フェーズの実行器。`failing` に挙げた名前のリポジトリだけが非ゼロ終了する。
    fn parallel_runner<'a>(
        recorder: &'a Recorder,
        failing: &'a [&'a str],
    ) -> impl Fn(&Path, &[&str]) -> crate::error::Result<CapturedRun> + Sync + 'a {
        move |directory, args| {
            recorder.enter(directory, args);
            let result = if failing.contains(&name_of(directory).as_str()) {
                Err(run_failure())
            } else {
                Ok(silent_run())
            };
            recorder.leave();

            result
        }
    }

    /// 並列フェーズの実行器。`at` の名前のリポジトリで `error` を返す（環境の問題を模す）。
    fn parallel_runner_broken_at<'a>(
        recorder: &'a Recorder,
        at: &'a str,
        error: impl Fn() -> Error + Sync + 'a,
    ) -> impl Fn(&Path, &[&str]) -> crate::error::Result<CapturedRun> + Sync + 'a {
        move |directory, args| {
            recorder.enter(directory, args);
            let result = if name_of(directory) == at {
                Err(error())
            } else {
                Ok(silent_run())
            };
            recorder.leave();

            result
        }
    }

    /// 並列フェーズの実行器。どの対象でも git を起動できない環境を模す。
    fn parallel_runner_without_git(
        recorder: &Recorder,
    ) -> impl Fn(&Path, &[&str]) -> crate::error::Result<CapturedRun> + Sync + '_ {
        move |directory, args| {
            recorder.enter(directory, args);
            recorder.leave();

            Err(Error::GitNotFound)
        }
    }

    /// 並列フェーズの実行器。対象ごとに複数行の出力を返す（出力のまとまりを見るため）。
    fn parallel_runner_with_output(
        recorder: &Recorder,
    ) -> impl Fn(&Path, &[&str]) -> crate::error::Result<CapturedRun> + Sync + '_ {
        move |directory, args| {
            recorder.enter(directory, args);
            let name = name_of(directory);
            recorder.leave();

            Ok(CapturedRun {
                stdout: format!("{name} stdout 1\n{name} stdout 2\n").into_bytes(),
                stderr: format!("{name} stderr 1\n{name} stderr 2\n").into_bytes(),
            })
        }
    }

    /// 直列フェーズの実行器。`failing` に挙げた名前のリポジトリだけが非ゼロ終了する。
    fn serial_runner<'a>(
        recorder: &'a Recorder,
        failing: &'a [&'a str],
    ) -> impl FnMut(&Path, &[&str]) -> crate::error::Result<()> + 'a {
        move |directory, args| {
            recorder.enter(directory, args);
            let result = if failing.contains(&name_of(directory).as_str()) {
                Err(run_failure())
            } else {
                Ok(())
            };
            recorder.leave();

            result
        }
    }

    /// 書き出された内容を行に分ける。
    fn lines_of(written: Vec<u8>) -> Vec<String> {
        String::from_utf8(written)
            .expect("the output should be utf-8")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn each_repository_is_fetched_in_candidate_order_with_the_same_arguments() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let summary = fetch_each(
            messages(),
            &targets,
            PruneMode::Prune,
            jobs(1),
            &mut written,
            parallel_runner(&parallel_calls, &[]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a successful run should not fail");

        assert_eq!(summary.succeeded, 3);
        assert!(summary.failed.is_empty());
        assert_eq!(parallel_calls.names(), ["mike", "alpha", "zulu"]);
        for arguments in parallel_calls.arguments() {
            assert_eq!(arguments, sibling_fetch_args(PruneMode::Prune));
        }
        assert_eq!(
            serial_calls.count(),
            0,
            "nothing failed, so the serial phase must not run"
        );
    }

    #[test]
    fn the_progress_of_every_repository_is_written_as_it_completes() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(1),
            &mut written,
            parallel_runner(&parallel_calls, &[]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("the run should succeed");

        // ワーカーが 1 本なので完了順は候補順と一致する（＝従来の直列実行と同じ出力）
        assert_eq!(
            lines_of(written),
            ["[1/3] mike", "[2/3] alpha", "[3/3] zulu"]
        );
    }

    #[test]
    fn every_repository_goes_through_the_parallel_phase_exactly_once() {
        let candidates = siblings_named(&["alpha", "bravo", "charlie", "delta", "echo"]);
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let summary = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(3),
            &mut written,
            parallel_runner(&parallel_calls, &[]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a successful run should not fail");

        let mut names = parallel_calls.names();
        names.sort();
        assert_eq!(names, ["alpha", "bravo", "charlie", "delta", "echo"]);
        assert_eq!(summary.succeeded, 5);
    }

    #[test]
    fn the_number_of_repositories_running_at_once_never_exceeds_the_limit() {
        let candidates = siblings_named(&["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(2),
            &mut written,
            parallel_runner(&parallel_calls, &[]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a successful run should not fail");

        assert!(
            parallel_calls.peak() <= 2,
            "at most 2 repositories may run at once: {}",
            parallel_calls.peak()
        );
    }

    #[test]
    fn only_the_repositories_that_failed_in_parallel_are_run_serially() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(3),
            &mut written,
            parallel_runner(&parallel_calls, &["alpha"]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a repository failure is recorded, not propagated");

        assert_eq!(parallel_calls.count(), 3);
        assert_eq!(
            serial_calls.names(),
            ["alpha"],
            "only the repository that failed may be run again"
        );
    }

    #[test]
    fn a_repository_that_succeeds_in_the_serial_phase_is_counted_once() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let summary = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(3),
            &mut written,
            parallel_runner(&parallel_calls, &["alpha"]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a repository failure is recorded, not propagated");

        assert_eq!(summary.succeeded, 3, "the retried repository counts once");
        assert!(
            summary.failed.is_empty(),
            "the parallel failure was recovered: {:?}",
            summary.failed
        );
    }

    #[test]
    fn a_repository_that_fails_in_both_phases_is_listed_once_in_candidate_order() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let summary = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(3),
            &mut written,
            // 並列フェーズの実行順は決まらないため、失敗の一覧は候補順でなければ安定しない
            parallel_runner(&parallel_calls, &["zulu", "alpha"]),
            serial_runner(&serial_calls, &["zulu", "alpha"]),
        )
        .expect("a repository failure is recorded, not propagated");

        assert_eq!(summary.succeeded, 1);
        assert_eq!(summary.failed, ["alpha", "zulu"]);
    }

    #[test]
    fn a_failing_repository_does_not_stop_the_remaining_ones() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let summary = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(1),
            &mut written,
            parallel_runner(&parallel_calls, &["alpha"]),
            serial_runner(&serial_calls, &["alpha"]),
        )
        .expect("a repository failure is recorded, not propagated");

        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.failed, ["alpha"]);
        assert_eq!(
            parallel_calls.count(),
            3,
            "every repository must be visited"
        );
    }

    #[test]
    fn a_failure_to_start_git_stops_the_remaining_repositories() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let err = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(1),
            &mut written,
            parallel_runner_broken_at(&parallel_calls, "alpha", || Error::GitNotFound),
            serial_runner(&serial_calls, &[]),
        )
        .expect_err("a broken environment must stop the run");

        assert!(
            err.chain()
                .any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::GitNotFound))),
            "the reason should be kept: {err:#}"
        );
        assert_eq!(
            parallel_calls.count(),
            2,
            "the repositories after the failure must not be visited"
        );
        assert_eq!(
            serial_calls.count(),
            0,
            "a broken environment must not reach the serial phase"
        );
    }

    #[test]
    fn a_spawn_failure_stops_the_run_as_well() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let err = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(1),
            &mut written,
            parallel_runner_broken_at(&parallel_calls, "mike", || Error::GitSpawnFailed {
                args: "fetch --all".to_owned(),
                source: std::io::Error::other("denied"),
            }),
            serial_runner(&serial_calls, &[]),
        )
        .expect_err("a broken environment must stop the run");

        assert!(
            err.to_string().contains("mike"),
            "the repository should be named: {err:#}"
        );
        assert_eq!(parallel_calls.count(), 1);
    }

    #[test]
    fn a_broken_environment_leaves_the_untouched_repositories_alone() {
        let candidates = siblings_named(&["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]);
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        let err = fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(2),
            &mut written,
            parallel_runner_without_git(&parallel_calls),
            serial_runner(&serial_calls, &[]),
        )
        .expect_err("a broken environment must stop the run");

        assert!(
            err.chain()
                .any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::GitNotFound))),
            "the reason should be kept: {err:#}"
        );
        // それぞれのワーカーは 1 件目で中断を知るため、着手されるのは同時実行数までに収まる
        assert!(
            parallel_calls.count() <= 2,
            "at most one repository per worker may start: {}",
            parallel_calls.count()
        );
    }

    #[test]
    fn the_output_of_one_repository_is_not_split_by_another() {
        let candidates = siblings_named(&["alpha", "bravo", "charlie", "delta"]);
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(4),
            &mut written,
            parallel_runner_with_output(&parallel_calls),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a successful run should not fail");

        let lines = lines_of(written);
        assert_eq!(
            lines.len(),
            4 * 5,
            "1 対象につき進捗行 1 行と本文 4 行: {lines:?}"
        );
        for block in lines.chunks(5) {
            let name = block[0]
                .rsplit(' ')
                .next()
                .expect("the progress line names the repository");
            for line in block {
                assert!(
                    line.contains(name),
                    "the block of `{name}` must not be split: {block:?}"
                );
            }
        }
    }

    #[test]
    fn the_serial_phase_is_announced_before_it_starts() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(1),
            &mut written,
            parallel_runner(&parallel_calls, &["alpha"]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a repository failure is recorded, not propagated");

        let lines = lines_of(written);
        let notice = lines
            .iter()
            .position(|line| line == &messages().fetch().serial_fallback(1))
            .expect("the fallback must be announced");
        let retried = lines
            .iter()
            .position(|line| line == "[1/1] alpha")
            .expect("the retried repository should be shown");

        assert!(
            notice < retried,
            "the announcement must come first: {lines:?}"
        );
    }

    #[test]
    fn nothing_is_announced_when_every_repository_succeeds() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let parallel_calls = Recorder::default();
        let serial_calls = Recorder::default();
        let mut written = Vec::new();

        fetch_each(
            messages(),
            &targets,
            PruneMode::Keep,
            jobs(2),
            &mut written,
            parallel_runner(&parallel_calls, &[]),
            serial_runner(&serial_calls, &[]),
        )
        .expect("a successful run should not fail");

        let lines = lines_of(written);
        assert!(
            !lines.contains(&messages().fetch().serial_fallback(0)),
            "there is nothing to re-run: {lines:?}"
        );
        assert_eq!(
            lines.len(),
            3,
            "one progress line per repository: {lines:?}"
        );
    }

    #[test]
    fn a_run_without_failures_is_a_success() {
        let summary = FetchSummary {
            succeeded: 3,
            failed: Vec::new(),
        };

        assert!(!summary.has_failure());
        assert_eq!(summary_line(messages(), &summary), "成功 3 件 / 失敗 0 件");
    }

    #[test]
    fn the_summary_names_the_repositories_that_failed() {
        let summary = FetchSummary {
            succeeded: 1,
            failed: vec!["alpha".to_owned(), "zulu".to_owned()],
        };

        assert!(summary.has_failure());
        let line = summary_line(messages(), &summary);
        assert!(
            line.contains("成功 1 件 / 失敗 2 件"),
            "both counts should be shown: {line}"
        );
        assert!(
            line.contains("alpha") && line.contains("zulu"),
            "the failed repositories should be named: {line}"
        );
        assert_eq!(line.lines().count(), 1, "1 行に収める: {line}");
    }

    #[test]
    fn a_line_is_written_to_the_given_writer() {
        let mut written = Vec::new();

        let reason = messages().fetch().single_sibling_reason();

        report_line(messages(), &mut written, reason).expect("writing to a buffer should succeed");

        assert_eq!(
            String::from_utf8(written).expect("the message should be utf-8"),
            format!("{reason}\n")
        );
    }

    // --- 文言（FR-27） ---

    /// 引数を取らない文言をまとめて取り出す。
    fn plain_texts(language: Language) -> Vec<&'static str> {
        let fetch = language.messages().fetch();

        vec![
            fetch.remote_header_subject(),
            fetch.remote_header_outcome(),
            fetch.all_remotes_label(),
            fetch.no_remotes(),
            fetch.url_section(),
            fetch.tracking_section(),
            fetch.sibling_scan_failed(),
            fetch.no_sibling_candidates(),
            fetch.single_sibling_reason(),
            fetch.siblings_header(),
            fetch.prune_scope_note(),
            fetch.tracking_state_section(),
            fetch.partial_failure(),
        ]
    }

    /// 引数を取る文言と、そこへ展開されるべき引数。
    fn texts_with_arguments(language: Language) -> Vec<(String, &'static str)> {
        let fetch = language.messages().fetch();

        vec![
            (fetch.remote_description("origin"), "origin"),
            (fetch.fixed_target("the remote `origin`"), "origin"),
            (fetch.fetch_failed("the remote `origin`"), "origin"),
            (fetch.selection_not_found("elsewhere"), "elsewhere"),
            (fetch.excluded_count(2), "2"),
            (
                fetch.path_not_utf8(Path::new("/repos/alpha")),
                "/repos/alpha",
            ),
            (
                fetch.sibling_selection_not_found("/repos/alpha, /repos/zulu"),
                "/repos/zulu",
            ),
            (fetch.sibling_start_failed("alpha"), "alpha"),
            (fetch.serial_fallback(2), "2"),
        ]
    }

    #[test]
    fn every_fetch_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            for text in plain_texts(language) {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
        }
    }

    #[test]
    fn every_fetch_message_expands_its_arguments() {
        for language in [Language::Japanese, Language::English] {
            for (text, argument) in texts_with_arguments(language) {
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_fetch_wording_is_translated() {
        for (japanese, english) in plain_texts(Language::Japanese)
            .into_iter()
            .zip(plain_texts(Language::English))
        {
            assert_ne!(japanese, english, "the wording must be translated");
        }

        for ((japanese, _), (english, _)) in texts_with_arguments(Language::Japanese)
            .into_iter()
            .zip(texts_with_arguments(Language::English))
        {
            assert_ne!(japanese, english, "the wording must be translated");
        }
    }

    #[test]
    fn the_english_summary_of_a_sibling_run_is_translated_as_well() {
        // 集計は `gz pull` と共有する語彙から引く（[`CommonMessages::run_summary`]）
        let summary = FetchSummary {
            succeeded: 1,
            failed: vec!["alpha".to_owned()],
        };

        let japanese = summary_line(Language::Japanese.messages(), &summary);
        let english = summary_line(Language::English.messages(), &summary);

        assert_ne!(japanese, english, "the summary must be translated");
        assert!(
            english.contains('1') && english.contains("alpha"),
            "the counts and the failed repository should be shown: {english}"
        );
        assert_eq!(english.lines().count(), 1, "1 行に収める: {english}");
    }

    #[test]
    fn the_english_header_keeps_the_sections_on_one_line() {
        let header = sibling_header(
            Language::English.messages(),
            &scan(siblings(), 2),
            PruneMode::Prune,
        );

        assert_eq!(header.lines().count(), 1, "1 行に収める: {header}");
        assert!(
            header.contains(HEADER_SEPARATOR),
            "the sections should be separated: {header}"
        );
        assert!(
            header.contains(PRUNE_OPTION),
            "the option name is not translated: {header}"
        );
    }
}
