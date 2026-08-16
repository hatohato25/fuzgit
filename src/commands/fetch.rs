//! `gz fetch` — fetch の対象を決めて取得する（FR-18 / FR-23）。
//!
//! 主機能は「リモートを選ぶ」ことではなく「fetch 対象の決定」であり、
//! 選択の余地がある場合にだけ finder を起動する（リモートが 1 つだけなら起動しない）。
//! 対象の範囲は [`FetchScope`] で切り替える。現在のリポジトリのリモートを対象にする
//! 既定の経路（[`run_current`]）と、`--siblings` で兄弟リポジトリを複数選んで
//! 1 件ずつ取得する経路（[`run_siblings`]）を持つ。
//!
//! fuzgit で初めてネットワークを伴うコマンドだが、ネットワークへ出るのは
//! 対象が決まったあとに継承 stdio で実行する `git fetch` だけである
//! （`--siblings` では選択件数分を直列に実行する）。
//! 候補生成とプレビューはローカル情報（`.git/config` の URL と保存済みの
//! リモート追跡参照）のみを読む（design.md「候補生成・プレビューでネットワーク
//! アクセスを行わない」）。プレビューは選択項目ごとに都度実行されるため、
//! ここで往復遅延や認証プロンプトを挟むと描画がそのままブロックされる。
//!
//! タイムアウト・リトライ・認証情報の取り扱いは行わない。到達不能・認証拒否は
//! git の標準メッセージのまま非ゼロ終了する。

use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::commands::aligned_candidates;
use crate::error::Error;
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with, select_one,
};
use crate::git::exec::{run_git, run_git_in};
use crate::git::read::{branch_tracking_args, remote_tracking_refs_args, remote_url_args, remotes};
use crate::git::siblings::{self, SiblingRepository, SiblingScan};
use crate::i18n::{Language, Messages};

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

/// ヘッダー内の区切り（`gz status` と同じ体裁）。
const HEADER_SEPARATOR: &str = "  |  ";

/// HEAD がブランチを指していない兄弟リポジトリの表示。
const DETACHED_LABEL: &str = "detached HEAD";

/// 任意のロックを取らずに git を実行するオプション。
///
/// プレビューは他人のリポジトリに対して選択項目ごとに実行されるため、
/// 他のプロセスが動作中でも干渉しないよう、ロックを要する操作を行わせない（man git）。
const NO_OPTIONAL_LOCKS: &str = "--no-optional-locks";

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
            let selected = select_one(items(language, messages, &remotes))?;

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

/// 兄弟リポジトリを選び、選択された順ではなく候補順に 1 件ずつ `git fetch --all` を実行する。
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

    let summary = fetch_each(
        messages,
        &targets,
        prune,
        &mut std::io::stderr(),
        |directory, arguments| run_git_in(language, directory, arguments),
    )?;
    report_line(
        messages,
        &mut std::io::stderr(),
        &summary_line(messages, &summary),
    )?;

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
    let Some(branch) = candidate.current_branch.as_deref() else {
        // detached HEAD には対にするブランチが無いため、リモート一覧と状態をそのまま並べる。
        return vec![
            candidate.name.clone(),
            candidate.remotes.join(", "),
            DETACHED_LABEL.to_owned(),
        ];
    };

    let branches = candidate
        .remotes
        .iter()
        .map(|remote| format!("{remote}/{branch}"))
        .collect::<Vec<_>>()
        .join(", ");

    vec![candidate.name.clone(), branches]
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
            ))
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

/// 直列実行の結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct FetchSummary {
    /// 取得に成功したリポジトリの件数。
    succeeded: usize,
    /// 取得に失敗したリポジトリのディレクトリ名（実行順）。
    failed: Vec<String>,
}

impl FetchSummary {
    /// 1 件でも失敗したかどうか。
    fn has_failure(&self) -> bool {
        !self.failed.is_empty()
    }
}

/// 選択されたリポジトリへ 1 件ずつ `git fetch` を実行する。
///
/// 並列化しない（git 自身も複数リモートの fetch を既定で逐次実行する。man git-fetch）。
/// 認証プロンプトが出た場合にどのリポジトリのものか分かるよう、実行前に進捗を書き出す。
///
/// 実行そのものを引数で受け取るのは、ネットワークや git の有無に依存せず
/// 集計と中断の判断を単体テストできるようにするため（本番は表示言語を束ねた [`run_git_in`] を渡す）。
///
/// # Errors
///
/// git の起動自体に失敗した場合（[`Error::GitNotFound`] / [`Error::GitSpawnFailed`]）は
/// 環境の問題でありリポジトリごとの失敗ではないため、残りを実行せずその場で返す。
/// 進捗の書き込みに失敗した場合も同様。
fn fetch_each(
    messages: &dyn Messages,
    targets: &[&SiblingRepository],
    prune: PruneMode,
    writer: &mut impl std::io::Write,
    mut fetch: impl FnMut(&Path, &[&str]) -> crate::error::Result<()>,
) -> Result<FetchSummary> {
    let arguments = sibling_fetch_args(prune);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    let mut summary = FetchSummary::default();
    for (index, target) in targets.iter().enumerate() {
        // git fetch の更新表も stderr に出るため、この区切りが無いとどのリポジトリの
        // 出力なのか読み取れない（man git-fetch OUTPUT 節）
        report_line(
            messages,
            writer,
            &progress_line(index, targets.len(), target),
        )?;

        match fetch(&target.workdir, &arguments) {
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

/// 実行前に示す進捗の 1 行を組み立てる。
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

        assert_eq!(progress_line(0, 3, &candidates[0]), "[1/3] mike");
        assert_eq!(progress_line(2, 3, &candidates[2]), "[3/3] zulu");
    }

    /// 実行に成功したことにする実行器。
    fn succeed(_directory: &Path, _args: &[&str]) -> crate::error::Result<()> {
        Ok(())
    }

    /// 実行器へ渡された引数の記録（実行ディレクトリと git 引数）。
    type Calls = std::rc::Rc<std::cell::RefCell<Vec<(std::path::PathBuf, Vec<String>)>>>;

    /// 対象のリポジトリごとの結果を決められる実行器を作り、呼び出し履歴を記録する。
    fn recording(
        results: Vec<crate::error::Result<()>>,
    ) -> (
        Calls,
        impl FnMut(&Path, &[&str]) -> crate::error::Result<()>,
    ) {
        let calls: Calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = std::rc::Rc::clone(&calls);
        let mut results = results.into_iter();

        let runner = move |directory: &Path, args: &[&str]| {
            recorded.borrow_mut().push((
                directory.to_path_buf(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            results.next().unwrap_or(Ok(()))
        };

        (calls, runner)
    }

    /// リポジトリ 1 件分の fetch の失敗。
    fn run_failure() -> crate::error::Result<()> {
        Err(Error::GitRunFailed {
            command: "git fetch".to_owned(),
            code: Some(128),
        })
    }

    #[test]
    fn each_repository_is_fetched_in_candidate_order_with_the_same_arguments() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let (calls, runner) = recording(Vec::new());
        let mut written = Vec::new();

        let summary = fetch_each(messages(), &targets, PruneMode::Prune, &mut written, runner)
            .expect("a successful run should not fail");

        assert_eq!(summary.succeeded, 3);
        assert!(summary.failed.is_empty());
        assert_eq!(
            calls
                .borrow()
                .iter()
                .map(|(directory, _)| directory.clone())
                .collect::<Vec<_>>(),
            candidates
                .iter()
                .map(|candidate| candidate.workdir.clone())
                .collect::<Vec<_>>()
        );
        for (_, arguments) in calls.borrow().iter() {
            assert_eq!(arguments, &sibling_fetch_args(PruneMode::Prune));
        }
    }

    #[test]
    fn the_progress_of_every_repository_is_written_before_it_runs() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let mut written = Vec::new();

        fetch_each(messages(), &targets, PruneMode::Keep, &mut written, succeed)
            .expect("the run should succeed");

        let text = String::from_utf8(written).expect("the progress should be utf-8");
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            ["[1/3] mike", "[2/3] alpha", "[3/3] zulu"]
        );
    }

    #[test]
    fn a_failing_repository_does_not_stop_the_remaining_ones() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let (calls, runner) = recording(vec![Ok(()), run_failure(), Ok(())]);
        let mut written = Vec::new();

        let summary = fetch_each(messages(), &targets, PruneMode::Keep, &mut written, runner)
            .expect("a repository failure is recorded, not propagated");

        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.failed, ["alpha"]);
        assert_eq!(calls.borrow().len(), 3, "every repository must be visited");
    }

    #[test]
    fn a_failure_to_start_git_stops_the_remaining_repositories() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let (calls, runner) = recording(vec![Ok(()), Err(Error::GitNotFound), Ok(())]);
        let mut written = Vec::new();

        let err = fetch_each(messages(), &targets, PruneMode::Keep, &mut written, runner)
            .expect_err("a broken environment must stop the run");

        assert!(
            err.chain()
                .any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::GitNotFound))),
            "the reason should be kept: {err:#}"
        );
        assert_eq!(
            calls.borrow().len(),
            2,
            "the repositories after the failure must not be visited"
        );
    }

    #[test]
    fn a_spawn_failure_stops_the_run_as_well() {
        let candidates = siblings();
        let targets: Vec<&SiblingRepository> = candidates.iter().collect();
        let (calls, runner) = recording(vec![Err(Error::GitSpawnFailed {
            args: "fetch --all".to_owned(),
            source: std::io::Error::other("denied"),
        })]);
        let mut written = Vec::new();

        let err = fetch_each(messages(), &targets, PruneMode::Keep, &mut written, runner)
            .expect_err("a broken environment must stop the run");

        assert!(
            err.to_string().contains("mike"),
            "the repository should be named: {err:#}"
        );
        assert_eq!(calls.borrow().len(), 1);
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
