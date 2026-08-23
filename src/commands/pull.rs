//! `gz pull` — 複数のブランチを upstream へ一括で追随させる（FR-24）。
//!
//! 選ばせるのは「どのブランチを upstream へ追随させるか」だけで、remote × branch や
//! 取り込み方式は選ばせない。取り込みは **fast-forward のみ**に固定する
//! （方式を選んで現在のブランチ 1 本だけ取り込むのは `--rebase` / `--merge`）。
//!
//! 方式を固定するのは「コンフリクトが起きたら pull 前の状態に戻す」という要望を、
//! ロールバック処理ではなく「壊し得る操作を行わない」ことで満たすため。fast-forward は
//! 既存のコミットを作り直さず、fast-forward できない場合・作業ツリーの変更と衝突する場合・
//! 他の worktree で使用中の場合はいずれも git が**何も変更せずに拒否する**
//! （man git-merge「PRE-MERGE CHECKS」、man git-fetch「<refspec>」）。
//!
//! 候補生成（[`pull_targets`]）とプレビューはネットワークを使わず、通信するのは対象が
//! 決まったあとに継承 stdio で実行する `git fetch` だけである（design.md「候補生成・
//! プレビューでネットワークアクセスを行わない」）。

use std::time::Instant;

use anyhow::{Context as _, Result, bail};

use crate::commands::confirmation::confirm;
use crate::commands::in_progress;
use crate::commands::{HEADER_SEPARATOR, aligned_candidates, command_display};
use crate::error::Error;
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with};
use crate::git::exec::run_git;
use crate::git::read::{
    PullScan, PullTarget, ahead_behind, current_branch, operation_in_progress, pull_targets,
    remotes, upstream as read_upstream,
};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};
use crate::notify::{notify, notify_setting, should_notify};

/// 自分自身のリポジトリを指す `git fetch` の取得元。
///
/// チェックアウトしていないブランチの更新は、取得済みの追跡参照からローカルブランチへ
/// 参照をコピーするだけで足りる。ネットワークへ出るのは対象のリモートを取得する
/// [`remote_fetch_args`] の 1 回だけに保つため、取得元にはリモート名ではなく `.` を渡す。
const CURRENT_REPOSITORY: &str = ".";

/// ローカルブランチの参照の接頭辞。refspec の更新先を完全な参照名で指定するために付ける。
const LOCAL_BRANCH_PREFIX: &str = "refs/heads/";

/// 現在のブランチを示すマーク（`gz branch` と同じ `*`）。
const CURRENT_MARK: &str = "* ";

/// 現在のブランチ以外の行頭。マークの有無でブランチ名の桁がずれないよう空白で揃える。
const OTHER_MARK: &str = "  ";

/// 候補行でブランチと upstream をつなぐ矢印（`git branch -vv` と同じ向き）。
///
/// 前後の空白は列の区切り（`crate::commands::COLUMN_SEPARATOR`）が担うため含めない。
const UPSTREAM_ARROW: &str = "→";

/// リモート追跡参照の接頭辞。表示用の短縮名（`origin/main`）を得るために取り除く。
const TRACKING_PREFIX: &str = "refs/remotes/";

/// プレビューに表示する最大コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// 取り込み対象の決め方。
///
/// 「候補が 1 つなら finder を出さない」という判断を、端末を占有する処理から切り離して
/// 単体テストできるようにするための型（[`crate::commands::fetch`] の `FetchDecision` /
/// `SiblingsDecision` と同じ役割）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullDecision {
    /// 取り込めるブランチが 1 件も無い。
    NoCandidate,
    /// 候補が 1 件だけであり、選択の余地が無い。
    Fixed,
    /// 候補一覧から複数選択する。
    Choose,
}

impl PullDecision {
    /// 候補一覧から対象の決め方を求める。
    ///
    /// 候補が 1 件のときに finder を省略できるのは、選んでも選ばなくても対象が同じ
    /// 1 本だけであり、選択が判断を増やさないため（`gz fetch` と同じ原則）。
    fn from_targets(targets: &[PullTarget]) -> Self {
        match targets {
            [] => PullDecision::NoCandidate,
            [_only] => PullDecision::Fixed,
            _ => PullDecision::Choose,
        }
    }
}

/// 取り込むブランチを決めて fast-forward で追随させる。
///
/// merge / rebase が進行中の場合は取り込みを始めず、復帰メニュー（FR-14）を表示する。
///
/// # Errors
///
/// 候補の生成に失敗した場合、取り込めるブランチが 1 件も無い場合、選択に失敗した場合
/// （中断を含む）にエラーを返す。取り込みが 1 件でも失敗した場合も、集計を表示したうえで
/// エラーを返す（成功した分は巻き戻さない。design.md「abort によるロールバックを採らない理由」）。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    mode: PullMode,
) -> Result<()> {
    // 進行中の merge / rebase を残したまま新しい取り込みは開始できないため、
    // 候補を出す前に復帰メニューへ委譲する（`gz merge` / `gz rebase` と同じ）
    if let Some(operation) = operation_in_progress(repository) {
        return in_progress::run(language, messages, repository, operation);
    }

    // fast-forward 以外の方式は**現在のブランチ 1 本**にしか適用できない。
    // チェックアウトしていないブランチは追跡参照からのコピーでしか追随させられず
    // （`git fetch . <tracking>:refs/heads/<branch>`）、rebase / merge には作業ツリーの
    // 切り替えか一時 worktree が要る。どちらも「ユーザーの作業を失わせない」方針に反する
    // （requirements.md「スコープ外」）。したがって候補選択そのものを行わない
    if mode != PullMode::FfOnly {
        return integrate_current_branch(language, messages, repository, mode);
    }

    let scan = pull_targets(repository).context(messages.pull().targets_read_failed())?;

    let targets = match PullDecision::from_targets(&scan.targets) {
        PullDecision::NoCandidate => bail!(messages.pull().no_candidates()),
        PullDecision::Fixed => {
            // 候補は 1 件だけ（[`PullDecision::Fixed`]）。finder を出さない以上、
            // どのブランチを取り込むのかはここでしか示せない
            let targets: Vec<&PullTarget> = scan.targets.iter().collect();
            for target in &targets {
                report_target(messages, &mut std::io::stderr(), target)?;
            }
            targets
        }
        PullDecision::Choose => {
            // 候補行と事前選択は同じ組み立て結果から作る。事前選択は表示文字列の完全一致で
            // 判定される（`crate::finder::FinderOptions`）一方、列の幅は候補一覧全体で決まるため
            let lines = aligned_candidates(&scan.targets, cells);
            let options = FinderOptions::new(SelectionMode::Multi)
                .with_header(pull_header(messages, &scan))
                .with_preselect(preselect(&lines));
            let selected = select_many_with(items(language, messages, &lines), &options)?;

            // skim は選択した順に返すため、候補一覧の順序（現在のブランチが先頭、
            // 以降は名前順）へ揃え直したうえで、キーが候補に含まれることを検証する
            in_candidate_order(messages, &scan.targets, &selected)?
        }
    };

    // 通知の設定は 1 件も実行しないうちに解決する（不正な設定のまま通信を始めない）
    let notification = notify_setting(repository)?;

    // 通知の閾値と比べるのは取り込みに掛かった時間だけであり、finder で候補を選んで
    // いる間は含めない（ユーザーが端末の前にいる時間であるため）
    let started = Instant::now();
    let summary = pull_each(messages, &targets, &mut std::io::stderr(), |arguments| {
        run_git(language, arguments)
    })?;
    let elapsed = started.elapsed();
    report_summary(messages, &mut std::io::stderr(), &summary)?;

    // 集計を書き出した**後**に通知する。通知が出ない環境でも集計は必ず出ることを
    // 構造で担保するためである（FR-29）。本文に [`summary_line`] を使わないのは、
    // それが失敗したブランチ名（ユーザー由来の文字列）を含むためで、通知へ載せるのは
    // 件数だけに限る（design.md「並列 fetch と完了通知のセキュリティ上の考慮」）
    if should_notify(notification, elapsed) {
        notify(
            messages,
            messages.pull().notification_title(),
            &messages
                .common()
                .run_summary(summary.succeeded, summary.failed.len()),
        );
    }

    if summary.has_failure() {
        // 集計は表示済み。ここでは終了コードを 1 にするためにエラーを返す
        bail!(messages.pull().partial_failure());
    }

    Ok(())
}

/// 一覧に表示する 1 行を列へ分解する。連結した文字列がそのまま絞り込みの対象になる。
///
/// ブランチ名の長さは候補ごとにまちまちであるため、列として返して
/// [`aligned_candidates`] に幅を揃えさせる（矢印と upstream の桁が候補ごとにずれないように）。
///
/// ahead / behind は**載せない**。候補生成の時点で分かるのは前回の fetch までに取得済みの
/// 追跡参照との差であり、これから fetch して取り込む本数とは一致しない。古い件数を
/// 添えると「2 件だけ入る」と読めてしまうため、件数は取り込み後に git 自身が示すものに委ねる。
fn cells(target: &PullTarget) -> Vec<String> {
    let mark = if target.is_current {
        CURRENT_MARK
    } else {
        OTHER_MARK
    };

    vec![
        format!("{mark}{branch}", branch = target.branch),
        UPSTREAM_ARROW.to_owned(),
        tracking_name(target).to_owned(),
    ]
}

/// 候補行に示す upstream の短縮名（`origin/main`）を返す。
///
/// `tracking_ref` は [`crate::git::read::Upstream::tracking_ref`] が
/// `refs/remotes/<remote>/<branch>` の形で組み立てた値であり、接頭辞は必ず付く。
/// 万一付いていない場合は参照名をそのまま示す（短縮は表示のための整形にすぎず、
/// git へ渡すのは常に完全な参照名の方であるため、別の参照を指すことはない）。
fn tracking_name(target: &PullTarget) -> &str {
    target
        .tracking_ref
        .strip_prefix(TRACKING_PREFIX)
        .unwrap_or(&target.tracking_ref)
}

/// 候補一覧のヘッダーを組み立てる。
///
/// 取り込み方式（fast-forward のみ）は選ばせないため操作説明として常に示し、
/// 除外件数は「黙って消さない」ために除外があるときだけ添える
/// （`gz fetch --siblings` の `sibling_header` と同じ体裁）。
fn pull_header(messages: &dyn Messages, scan: &PullScan) -> String {
    let mut sections = vec![messages.pull().header().to_owned()];

    if scan.excluded > 0 {
        sections.push(messages.pull().excluded_count(scan.excluded));
    }

    sections.join(HEADER_SEPARATOR)
}

/// 起動時に選択済みにする候補の表示文字列を集める。
///
/// 現在のブランチは最も取り込みたい対象である一方、他のブランチと違って作業ツリーごと
/// 更新される。選択済みで始めつつ、Tab で外せる形にする。
///
/// 受け取るのは [`aligned_candidates`] が組み立てた候補と表示行の対であり、
/// finder へ渡す候補行と同じ文字列がそのまま事前選択になる。
fn preselect(targets: &[(&PullTarget, String)]) -> Vec<String> {
    targets
        .iter()
        .filter(|(target, _)| target.is_current)
        .map(|(_, line)| line.clone())
        .collect()
}

/// 取り込み対象を finder の候補へ変換する。
///
/// 照合キーはブランチ名（[`pull_targets`] が列挙した値であり、ユーザーの自由入力ではない）。
fn items(
    language: Language,
    messages: &dyn Messages,
    targets: &[(&PullTarget, String)],
) -> Vec<FinderItem> {
    targets
        .iter()
        .map(|(target, line)| {
            FinderItem::new(
                line.clone(),
                target.branch.clone(),
                preview_source(messages, target),
                language.messages(),
            )
        })
        .collect()
}

/// 候補 1 件のプレビュー内容を組み立てる。
///
/// 読むのはローカルに保存済みの追跡参照だけで、ネットワークへは接続しない
/// （design.md「候補生成・プレビューでネットワークアクセスを行わない」）。
/// プレビューは選択項目ごとに同期実行されるため、ここで往復遅延や認証プロンプトを
/// 挟むと描画がそのままブロックされる。
fn preview_source(messages: &dyn Messages, target: &PullTarget) -> PreviewSource {
    PreviewSource::Composite(vec![(
        messages.pull().unmerged_section().to_owned(),
        PreviewSource::Git(preview_args(target)),
    )])
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
///
/// 範囲はブランチから見て追跡参照側にしか無いコミット（= 取り込まれる予定のコミット）。
/// 件数を絞るのは、プレビューがカーソル移動のたびに同期実行されるためで、
/// 長く放置されたブランチでも一定の速さを保つ（design.md のプレビュー設計制約）。
fn preview_args(target: &PullTarget) -> Vec<String> {
    let range = format!(
        "{branch}..{reference}",
        branch = target.branch,
        reference = target.tracking_ref
    );

    // 末尾の `--` により、リビジョンがパスとして解釈されることを防ぐ
    [
        "log",
        "--color=always",
        "--oneline",
        "-n",
        PREVIEW_COMMIT_COUNT,
        &range,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// 選択されたブランチ名を候補一覧の順序へ並べ直す。
///
/// # Errors
///
/// 候補一覧に無いブランチ名が含まれていた場合にエラーを返す（対象を取り違えたまま
/// ローカルの参照を更新しないよう、暗黙に読み飛ばさない）。
fn in_candidate_order<'a>(
    messages: &dyn Messages,
    targets: &'a [PullTarget],
    selected: &[String],
) -> Result<Vec<&'a PullTarget>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|branch| !targets.iter().any(|target| &target.branch == *branch))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(messages.pull().selection_not_found(&missing.join(", ")));
    }

    Ok(targets
        .iter()
        .filter(|target| selected.iter().any(|branch| &target.branch == branch))
        .collect())
}

/// 直列実行の結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PullSummary {
    /// 取り込みに成功したブランチの件数。
    succeeded: usize,
    /// 取り込めなかったブランチ名（実行順）。upstream の取得に失敗して飛ばした分も含む。
    failed: Vec<String>,
    /// fast-forward での取り込みそのものに失敗したブランチの件数。
    ///
    /// [`FAST_FORWARD_GUIDANCE`] を添えるかの判断に使う。upstream の取得に失敗して
    /// 飛ばしたブランチは fast-forward を試していないため、ここには数えない
    /// （`gz pull --rebase` を案内しても解決しない）。
    not_fast_forwarded: usize,
}

impl PullSummary {
    /// 1 件でも失敗したかどうか。
    fn has_failure(&self) -> bool {
        !self.failed.is_empty()
    }

    /// fast-forward できなかったブランチがあったかどうか。
    fn has_fast_forward_failure(&self) -> bool {
        self.not_fast_forwarded > 0
    }
}

/// 選択されたブランチを候補順に 1 件ずつ upstream へ追随させる。
///
/// 先に upstream のリモートをまとめて取得し（リモートごとに 1 回だけ）、そのあと
/// ブランチごとに取り込む。取得に失敗したリモートを upstream に持つブランチは、
/// 古い追跡参照へ取り込んで「更新された」ように見せないよう、失敗として扱って飛ばす。
///
/// **並列化しない。**根拠は前半と後半で異なる。
///
/// - **リモートの取得**（[`fetch_remotes`]）は、対象数が実質 1 であり並列化しても効果が無い。
///   複数の upstream を運用する場面はほとんど無く、[`target_remotes`] が重複排除するため、
///   リモートが `origin` だけなら `git fetch` はそもそも 1 回しか走らない
/// - **ブランチの取り込み**（このループ）は**同一リポジトリへの書き込み**であり、
///   `.git/index.lock` や参照の lock が競合する。現在のブランチの `merge --ff-only` は
///   index と作業ツリーまで更新するため、他のブランチの更新と同時に走らせると一貫性を壊し得る。
///   加えてローカル操作であり、所要時間の主因（ネットワーク待ち）でもない
///
/// 「git 自身も既定では逐次実行する」ことは根拠にしない。それは git の**既定値**の説明で
/// あって安全性の理由ではなく、git は `git fetch --all --jobs=<n>` として並列 fetch を
/// 上流機能に持つ。別リポジトリが対象で待ち時間の主因でもある `gz fetch --siblings` は、
/// 実際に並列化している（FR-28。[`crate::commands::fetch`] の `fetch_each`）。
///
/// 実行そのものを引数で受け取るのは、ネットワークや git の有無に依存せず集計と中断の
/// 判断を単体テストできるようにするため（本番は表示言語を束ねた [`run_git`] を渡す。
/// [`crate::commands::fetch`] の `fetch_each` と同方式）。
///
/// # Errors
///
/// git の起動自体に失敗した場合（[`Error::GitNotFound`] / [`Error::GitSpawnFailed`]）は
/// 環境の問題でありブランチごとの失敗ではないため、残りを実行せずその場で返す。
/// 進捗の書き込みに失敗した場合も同様。
fn pull_each(
    messages: &dyn Messages,
    targets: &[&PullTarget],
    writer: &mut impl std::io::Write,
    mut run: impl FnMut(&[&str]) -> crate::error::Result<()>,
) -> Result<PullSummary> {
    let failed_remotes = fetch_remotes(messages, targets, &mut run)?;

    let mut summary = PullSummary::default();
    for (index, target) in targets.iter().enumerate() {
        // git 自身の出力（更新された参照の一覧・fast-forward できない旨）も標準エラーへ
        // 出るため、この区切りが無いとどのブランチの出力なのか読み取れない
        report_line(
            messages,
            writer,
            &progress_line(index, targets.len(), target),
        )?;

        if failed_remotes.iter().any(|remote| remote == &target.remote) {
            // 黙って飛ばすと成功したように見えるため、理由を示したうえで失敗に数える
            report_line(messages, writer, &skipped_line(messages, target))?;
            summary.failed.push(target.branch.clone());
            continue;
        }

        let arguments = integrate_args(target);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

        match run(&arguments) {
            Ok(()) => summary.succeeded += 1,
            // fast-forward できない・作業ツリーの変更と衝突するといった個々の失敗は
            // 記録して次へ進む。git が理由をその場で表示済みであり、ここでは再掲しない。
            // いずれの場合も git は何も変更せずに拒否するため、巻き戻す対象も無い
            Err(Error::GitRunFailed { .. }) => {
                summary.failed.push(target.branch.clone());
                summary.not_fast_forwarded += 1;
            }
            Err(error) => {
                return Err(anyhow::Error::from(error)
                    .context(messages.pull().integration_start_failed(&target.branch)));
            }
        }
    }

    Ok(summary)
}

/// 対象のブランチが追随する先のリモートをまとめて取得し、失敗したリモート名を返す。
///
/// # Errors
///
/// git の起動自体に失敗した場合（[`Error::GitNotFound`] / [`Error::GitSpawnFailed`]）は
/// 残りのリモートを取得せずその場で返す。
fn fetch_remotes(
    messages: &dyn Messages,
    targets: &[&PullTarget],
    run: &mut impl FnMut(&[&str]) -> crate::error::Result<()>,
) -> Result<Vec<String>> {
    let mut failed = Vec::new();

    for remote in target_remotes(targets) {
        let arguments = remote_fetch_args(&remote);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

        match run(&arguments) {
            Ok(()) => {}
            // 到達不能・認証拒否は他のリモートと無関係なので、記録して次のリモートへ進む
            Err(Error::GitRunFailed { .. }) => failed.push(remote),
            Err(error) => {
                return Err(
                    anyhow::Error::from(error).context(messages.pull().fetch_start_failed(&remote))
                );
            }
        }
    }

    Ok(failed)
}

/// 対象のブランチが追随する先のリモート名を重複排除して名前順に並べる。
///
/// 同じリモートを upstream に持つブランチをまとめて扱うため、通信は 1 リモートにつき
/// 1 回だけにする（`git fetch <remote>` はそのリモートの追跡参照をまとめて更新する）。
fn target_remotes(targets: &[&PullTarget]) -> Vec<String> {
    let mut remotes: Vec<String> = targets.iter().map(|target| target.remote.clone()).collect();
    remotes.sort_unstable();
    remotes.dedup();

    remotes
}

/// `git fetch <remote>` の引数を組み立てる。
///
/// `--prune` は付けない。追跡参照の削除は `gz fetch --prune` としてユーザーが明示指定する
/// 操作であり、取り込みのついでに行うものではない。
fn remote_fetch_args(remote: &str) -> Vec<String> {
    vec!["fetch".to_owned(), remote.to_owned()]
}

/// 1 ブランチ分の取り込みに用いる git コマンドの引数を組み立てる。
///
/// 現在のブランチだけ経路が異なるのは、チェックアウト中のブランチへの ref 更新を
/// git が拒否するため（`refusing to fetch into branch '%s' checked out at '%s'`）。
fn integrate_args(target: &PullTarget) -> Vec<String> {
    if target.is_current {
        current_branch_args(target)
    } else {
        other_branch_args(target)
    }
}

/// 現在のブランチを追随させる `git merge --ff-only <追跡参照>` の引数を組み立てる。
///
/// 引数は `--rebase` / `--merge` の経路と同じ [`mode_args`] から組み立てる。
/// 「現在のブランチ 1 本を ff-only で取り込む」場合の引数を 2 か所に持つと
/// 片方だけが変わり得るため、組み立てを 1 か所へ集約する。
fn current_branch_args(target: &PullTarget) -> Vec<String> {
    mode_args(PullMode::FfOnly, &target.tracking_ref)
}

/// 現在のブランチ以外を追随させる `git fetch . <追跡参照>:refs/heads/<ブランチ>` の
/// 引数を組み立てる。
///
/// **refspec に `+`（強制更新）を付けない。** 付けると非 fast-forward の更新が通り、
/// そのブランチにしか無いコミットを失わせる。`+` が無い refspec の更新は fast-forward
/// でなければ git が拒否するため、失敗しても取り込み前の状態がそのまま残る
/// （man git-fetch「<refspec>」。この不変条件は単体テストで固定している）。
fn other_branch_args(target: &PullTarget) -> Vec<String> {
    let refspec = format!(
        "{tracking}:{LOCAL_BRANCH_PREFIX}{branch}",
        tracking = target.tracking_ref,
        branch = target.branch
    );

    vec!["fetch".to_owned(), CURRENT_REPOSITORY.to_owned(), refspec]
}

/// 実行前に示す進捗の 1 行を組み立てる（`gz fetch --siblings` と同形式）。
fn progress_line(index: usize, total: usize, target: &PullTarget) -> String {
    format!(
        "[{position}/{total}] {branch}",
        position = index + 1,
        branch = target.branch
    )
}

/// upstream のリモートを取得できなかったために取り込みを飛ばすことを伝える 1 行。
fn skipped_line(messages: &dyn Messages, target: &PullTarget) -> String {
    messages.pull().skipped(&target.remote, &target.branch)
}

/// 実行結果の集計を 1 行に組み立てる（`gz fetch --siblings` と同形式）。
fn summary_line(messages: &dyn Messages, summary: &PullSummary) -> String {
    let mut line = messages
        .common()
        .run_summary(summary.succeeded, summary.failed.len());

    if summary.has_failure() {
        line.push_str(&messages.common().failed_targets(&summary.failed.join(", ")));
    }

    line
}

/// 実行結果の集計を書き出す。
///
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_summary(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    summary: &PullSummary,
) -> Result<()> {
    report_line(messages, writer, &summary_line(messages, summary))?;

    // 案内は fast-forward できなかった場合にだけ添える。全件成功時や、リモートの取得に
    // 失敗しただけの場合に出すと、実行していない操作を促すことになる
    if summary.has_fast_forward_failure() {
        report_line(messages, writer, messages.pull().fast_forward_guidance())?;
    }

    Ok(())
}

/// 1 行を書き出す。
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
fn fixed_target_message(messages: &dyn Messages, target: &PullTarget) -> String {
    messages.pull().fixed_target(&target.branch)
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
    target: &PullTarget,
) -> Result<()> {
    report_line(messages, writer, &fixed_target_message(messages, target))
}

/// 現在のブランチ 1 本を、指定された方式で upstream へ取り込む（`--rebase` / `--merge`）。
///
/// 候補選択を行わない。対象は現在のブランチの upstream に固定されており、選ぶものが無い
/// （`gz pull` のフラグ無しが複数選択なのと対照的である。[`run`] の分岐にその理由を書いた）。
///
/// 元は `gz sync`（FR-19）という独立したコマンドだったが、「取り込む」操作が 2 つの
/// コマンドに分かれていて使い分けが読み取れなかったため、`gz pull` の方式フラグとして
/// 畳んだ（requirements.md「削除した機能の記録」）。挙動は `gz sync --rebase` /
/// `--merge` と同一である。
///
/// # Errors
///
/// upstream が定まらない場合（detached HEAD・upstream 未設定・追跡参照を組み立てられない
/// 設定・リモートが未登録）、`git fetch` の実行、ahead/behind の算出、取り込みの実行に
/// 失敗した場合にエラーを返す。確認プロンプトで承認が得られなかった場合は
/// [`crate::error::Error::Cancelled`]。
fn integrate_current_branch(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    mode: PullMode,
) -> Result<()> {
    let target = resolve_current_target(messages, repository)?;

    let arguments = remote_fetch_args(&target.remote);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).with_context(|| messages.pull().fetch_failed(&target.remote))?;

    // fetch で追跡参照が更新されているため、取り込む量はここで初めて確定する
    let position = ahead_behind(workdir(repository)?, &target.branch, &target.reference)
        .with_context(|| messages.common().ahead_behind_failed(&target.reference))?;
    let Some((ahead, behind)) = position else {
        bail!(missing_tracking_message(messages, &target));
    };

    if behind == 0 {
        return report_up_to_date(messages, &mut std::io::stderr(), &target, ahead);
    }

    let arguments = mode_args(mode, &target.reference);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    confirm(
        messages,
        &confirmation_header(messages, &target, behind, mode),
        &[&command_display(&arguments)],
    )?;

    run_git(language, &arguments)
        .with_context(|| messages.pull().integration_failed(&target.reference))?;

    Ok(())
}

/// 取り込み方式（FR-19）。
///
/// clap の排他フラグ 2 つをそのまま `commands` 層へ持ち回さず、取り得る 3 通りを型で表す
/// （[`crate::commands::merge::MergeMode`] と同方針）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    /// fast-forward できる場合のみ取り込む（既定）。
    ///
    /// 既定をこれにするのは「指定が無いので安全側へ倒す」ためではなく、履歴の統合方法
    /// （merge コミットを作るのか、作り直すのか）をユーザーの明示指定に限るため。
    /// fast-forward できない場合は git のエラーをそのまま見せて停止する。
    FfOnly,
    /// upstream の上へ rebase して取り込む（`--rebase`）。
    Rebase,
    /// upstream を merge して取り込む（`--merge`）。
    Merge,
}

impl PullMode {
    /// `--rebase` / `--merge` の指定から取り込み方式を決める。
    ///
    /// 排他性は `clap` の `conflicts_with_all` でも担保しているが、両方が立った状態を
    /// 暗黙にどちらかへ倒すことがないよう、ここでも明示的に拒否する。
    ///
    /// # Errors
    ///
    /// 両方が同時に指定された場合にエラーを返す。
    pub fn from_flags(messages: &dyn Messages, rebase: bool, merge: bool) -> Result<Self> {
        match (rebase, merge) {
            (false, false) => Ok(Self::FfOnly),
            (true, false) => Ok(Self::Rebase),
            (false, true) => Ok(Self::Merge),
            (true, true) => bail!(messages.pull().conflicting_modes()),
        }
    }

    /// 確認プロンプトに添える注記。履歴を書き換える方式の場合のみ返す。
    ///
    /// 注記は `gz rebase` と同じ内容であるため共有語彙から引く
    /// （[`crate::i18n::messages::CommonMessages::history_rewrite_note`]）。
    fn note(self, messages: &dyn Messages) -> Option<&'static str> {
        match self {
            PullMode::FfOnly | PullMode::Merge => None,
            PullMode::Rebase => Some(messages.common().history_rewrite_note()),
        }
    }
}

/// 同期の対象（現在のブランチと、その upstream）。
///
/// `remote` は [`remotes`] が列挙した名前と一致することを確認済みの値、`reference` は
/// gix が組み立てたローカルの追跡参照であり、いずれもユーザーの自由入力ではない
/// （`git fetch` / `git merge` / `git rebase` の位置引数は `--` で保護できないため、
/// 値の由来で担保する。design.md セキュリティ設計）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CurrentTarget {
    /// 現在のブランチの短縮名。
    branch: String,
    /// fetch するリモート名。
    remote: String,
    /// upstream のローカル追跡参照（`refs/remotes/<remote>/<branch>`）。
    reference: String,
}

/// 同期の対象を確定する。
///
/// # Errors
///
/// detached HEAD の場合、upstream が設定されていない場合、追跡参照を組み立てられない
/// 設定の場合、upstream のリモートが登録されていない場合に、それぞれの原因を示す
/// エラーを返す（対象を推測して別のリモート・ブランチへ倒さない）。
fn resolve_current_target(
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<CurrentTarget> {
    let Some(branch) =
        current_branch(repository).context(messages.common().current_branch_read_failed())?
    else {
        bail!(messages.common().detached_head_without_upstream());
    };

    let Some(upstream) = read_upstream(repository, &branch)
        .with_context(|| messages.common().upstream_read_failed(&branch))?
    else {
        bail!(messages.common().upstream_not_configured(&branch));
    };

    let Some(reference) = upstream.tracking_ref() else {
        bail!(messages.pull().tracking_ref_unavailable(
            &branch,
            &upstream.remote,
            &upstream.merge_ref
        ));
    };

    // `branch.<name>.remote` には URL を直接書くこともできる。その値をそのまま
    // `git fetch` へ渡すと、fuzgit が列挙していない対象へネットワーク接続することになるため、
    // 登録済みのリモート名であることを確かめてから使う（design.md セキュリティ設計）
    let remotes = remotes(repository).context(messages.common().remote_list_read_failed())?;
    if !remotes.contains(&upstream.remote) {
        bail!(messages.pull().unknown_remote(&branch, &upstream.remote));
    }

    Ok(CurrentTarget {
        branch,
        remote: upstream.remote,
        reference,
    })
}

/// 取り込みに用いる git コマンドの引数を組み立てる。
///
/// `gz pull`（FR-24）が現在のブランチへ fast-forward で取り込む経路からも呼ぶ。
/// 「現在のブランチ 1 本を ff-only で取り込む」場合だけが両コマンドの重なりであり、
/// 引数の組み立てを 2 か所に持つと片方だけが変わり得るため、ここへ集約する
/// （両者が一致することは `commands::pull` の単体テストでも固定している）。
fn mode_args(mode: PullMode, reference: &str) -> Vec<String> {
    let mut args = match mode {
        PullMode::FfOnly => vec!["merge".to_owned(), "--ff-only".to_owned()],
        PullMode::Merge => vec!["merge".to_owned()],
        PullMode::Rebase => vec!["rebase".to_owned()],
    };
    args.push(reference.to_owned());
    args
}

/// fetch しても追跡参照が現れなかった場合のエラーメッセージを組み立てる。
///
/// upstream の設定はあるがリモート側にブランチが無い（削除された・まだ push していない）
/// 場合に起きる。取り込む対象が無い以上、別の参照へ倒さず原因を示して停止する。
fn missing_tracking_message(messages: &dyn Messages, target: &CurrentTarget) -> String {
    messages
        .pull()
        .missing_tracking_ref(&target.reference, &target.remote, &target.branch)
}

/// 取り込むコミットが無いことを伝える文言を組み立てる。
///
/// ahead が残っている場合はその件数も添える。「最新である」だけでは、まだ push していない
/// コミットを抱えていることに気付けないため。
fn up_to_date_message(messages: &dyn Messages, target: &CurrentTarget, ahead: usize) -> String {
    let mut message = messages
        .pull()
        .up_to_date(&target.branch, &target.reference);

    if ahead > 0 {
        // 本文と注記を区切る改行は装飾であるため、文言ではなくここで付ける
        message.push('\n');
        message.push_str(&messages.pull().unpushed_commits(ahead));
    }

    message
}

/// 取り込むコミットが無いことを伝えて正常終了する。
///
/// 最新であることは正常な結果であるためエラーにしない（requirements.md FR-19）。
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_up_to_date(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    target: &CurrentTarget,
    ahead: usize,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = up_to_date_message(messages, target, ahead)
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// 確認プロンプトの見出しを組み立てる。
///
/// 取り込まれるコミット数を示し、`--rebase` の場合は履歴改変であることを添える
/// （実行するコマンドは対象行として別に提示する）。
fn confirmation_header(
    messages: &dyn Messages,
    target: &CurrentTarget,
    count: usize,
    mode: PullMode,
) -> String {
    let mut header = messages
        .pull()
        .confirmation(&target.reference, count, &target.branch);

    if let Some(note) = mode.note(messages) {
        header.push('\n');
        header.push_str(note);
    }

    header
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

    /// 候補 1 件分を組み立てる。
    fn target(branch: &str, is_current: bool) -> PullTarget {
        target_on("origin", branch, is_current)
    }

    /// upstream のリモートを指定して候補 1 件分を組み立てる。
    fn target_on(remote: &str, branch: &str, is_current: bool) -> PullTarget {
        PullTarget {
            branch: branch.to_owned(),
            remote: remote.to_owned(),
            tracking_ref: format!("refs/remotes/{remote}/{branch}"),
            is_current,
        }
    }

    /// 現在のブランチ 1 件と他 2 件の候補一覧（`pull_targets()` と同じ並び）。
    fn targets() -> Vec<PullTarget> {
        vec![
            target("main", true),
            target("alpha", false),
            target("zulu", false),
        ]
    }

    /// 候補 1 件だけの表示行（揃える相手が居ないため列を連結しただけの行）。
    fn display_line(target: &PullTarget) -> String {
        cells(target).join(COLUMN_SEPARATOR)
    }

    /// 走査結果を組み立てる。
    fn scan(targets: Vec<PullTarget>, excluded: usize) -> PullScan {
        PullScan { targets, excluded }
    }

    /// 対象のブランチ名を並び順のまま取り出す。
    fn branches(targets: &[&PullTarget]) -> Vec<String> {
        targets
            .iter()
            .map(|target| target.branch.clone())
            .collect::<Vec<_>>()
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
    fn without_any_candidate_there_is_nothing_to_decide() {
        assert_eq!(
            PullDecision::from_targets(&[]),
            PullDecision::NoCandidate,
            "a repository without an upstream must not open the finder"
        );
    }

    #[test]
    fn a_single_candidate_skips_the_finder() {
        for only in [target("main", true), target("feature/login", false)] {
            assert_eq!(
                PullDecision::from_targets(std::slice::from_ref(&only)),
                PullDecision::Fixed,
                "a lone candidate leaves nothing to choose: {only:?}"
            );
        }
    }

    #[test]
    fn two_or_more_candidates_are_chosen_in_the_finder() {
        let targets = [target("main", true), target("alpha", false)];
        assert_eq!(PullDecision::from_targets(&targets), PullDecision::Choose);

        let targets = [
            target("main", true),
            target("alpha", false),
            target("zebra", false),
        ];
        assert_eq!(PullDecision::from_targets(&targets), PullDecision::Choose);
    }

    #[test]
    fn the_empty_result_explains_how_to_set_an_upstream() {
        // コマンド列は翻訳しないため、どちらの言語でも同じ文字列が含まれる
        for language in [Language::Japanese, Language::English] {
            let message = language.messages().pull().no_candidates();

            assert!(
                message.contains("git push -u <remote> <branch>"),
                "the way to push a new branch should be offered: {message}"
            );
            assert!(
                message.contains("git branch --set-upstream-to=<remote>/<branch>"),
                "the plain git way should be offered too: {message}"
            );
        }
    }

    #[test]
    fn a_candidate_line_shows_the_branch_and_the_upstream_it_follows() {
        let line = display_line(&target("alpha", false));

        assert_eq!(line, "  alpha  →  origin/alpha");
    }

    #[test]
    fn the_current_branch_is_marked_like_git_branch() {
        let current = display_line(&target("main", true));
        let other = display_line(&target("alpha", false));

        assert!(
            current.starts_with(CURRENT_MARK),
            "the current branch should be marked: {current}"
        );
        assert!(
            other.starts_with(OTHER_MARK),
            "the other branches should keep the same column: {other}"
        );
        assert_eq!(
            current.len() - "main".len() - "origin/main".len(),
            other.len() - "alpha".len() - "origin/alpha".len(),
            "the mark must not shift the columns: {current} / {other}"
        );
    }

    #[test]
    fn a_candidate_line_shows_the_upstream_by_its_short_name() {
        let mut candidate = target("main", false);
        candidate.remote = "upstream".to_owned();
        candidate.tracking_ref = "refs/remotes/upstream/trunk".to_owned();

        let line = display_line(&candidate);

        assert!(
            line.contains("upstream/trunk"),
            "the short name should be shown: {line}"
        );
        assert!(
            !line.contains(TRACKING_PREFIX),
            "the full reference is for git, not for the list: {line}"
        );
    }

    #[test]
    fn a_candidate_line_never_shows_ahead_or_behind() {
        // 候補生成の時点で分かるのは前回の fetch までの差であり、これから取り込む本数ではない
        for candidate in targets() {
            let line = display_line(&candidate);

            assert!(
                !line.contains("ahead") && !line.contains("behind"),
                "a stale count must not be shown: {line}"
            );
            assert!(
                !line.contains("進み") && !line.contains("遅れ"),
                "a stale count must not be shown: {line}"
            );
        }
    }

    #[test]
    fn the_header_says_that_only_fast_forward_is_used() {
        let header = pull_header(messages(), &scan(targets(), 0));

        assert!(
            header.contains("fast-forward"),
            "the integration method should be shown before choosing: {header}"
        );
        assert_eq!(header.lines().count(), 1, "1 行に収める: {header}");
    }

    #[test]
    fn the_header_states_how_many_branches_were_left_out() {
        let header = pull_header(messages(), &scan(targets(), 2));

        assert!(header.contains('2'), "the count should be shown: {header}");
        assert!(
            header.contains("除外"),
            "the exclusion should be named: {header}"
        );
        assert_eq!(header.lines().count(), 1, "1 行に収める: {header}");
    }

    #[test]
    fn nothing_is_said_about_exclusions_when_there_were_none() {
        let header = pull_header(messages(), &scan(targets(), 0));

        assert!(
            !header.contains("除外"),
            "an exclusion must not be implied: {header}"
        );
    }

    #[test]
    fn only_the_current_branch_is_preselected() {
        let candidates = targets();
        let lines = aligned_candidates(&candidates, cells);

        assert_eq!(
            preselect(&lines),
            vec!["* main   →  origin/main".to_owned()],
            "選択済みにするのは現在のブランチだけ: {lines:?}"
        );
    }

    #[test]
    fn the_preselected_line_is_the_very_line_shown_in_the_list() {
        // 事前選択は表示文字列の完全一致で判定されるため、列を揃えたあとの行と
        // 一致していなければ機能しない（`crate::finder::FinderOptions::preselect`）
        let candidates = targets();
        let lines = aligned_candidates(&candidates, cells);

        let displayed: Vec<String> = items(Language::Japanese, messages(), &lines)
            .iter()
            .map(|item| item.text().into_owned())
            .collect();

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
            !preselected.contains(&display_line(&target("main", true))),
            "the padding is what makes the two ways differ: {preselected:?}"
        );
    }

    #[test]
    fn nothing_is_preselected_on_a_detached_head() {
        // detached HEAD では現在のブランチが無い（`pull_targets` はエラーにしない）
        let candidates = vec![target("alpha", false), target("zulu", false)];
        let lines = aligned_candidates(&candidates, cells);

        assert!(preselect(&lines).is_empty());
    }

    #[test]
    fn a_candidate_is_keyed_by_its_branch_name() {
        let candidates = targets();
        let lines = aligned_candidates(&candidates, cells);

        let items = items(Language::Japanese, messages(), &lines);

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            ["main", "alpha", "zulu"]
        );
    }

    #[test]
    fn the_preview_lists_the_commits_that_would_be_integrated() {
        let candidate = target("main", true);

        let sections = sections(&preview_source(messages(), &candidate));

        assert_eq!(sections.len(), 1, "unexpected preview: {sections:?}");
        assert_eq!(
            sections[0].1,
            [
                "log",
                "--color=always",
                "--oneline",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "main..refs/remotes/origin/main",
                "--",
            ]
        );
    }

    #[test]
    fn the_preview_label_says_the_information_is_as_old_as_the_last_fetch() {
        let sections = sections(&preview_source(messages(), &target("main", true)));

        assert!(
            sections[0].0.contains("前回の fetch"),
            "the age of the information should be stated: {label}",
            label = sections[0].0
        );
    }

    #[test]
    fn no_preview_reaches_the_network() {
        // プレビューが読むのは保存済みの追跡参照だけ（design.md の設計原則）
        for candidate in targets() {
            for (label, arguments) in sections(&preview_source(messages(), &candidate)) {
                assert_eq!(
                    arguments.first().map(String::as_str),
                    Some("log"),
                    "`{label}` must only read local history: {arguments:?}"
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
    fn the_selection_is_put_back_into_candidate_order() {
        let candidates = targets();
        // skim は選択した順に返す
        let selected = ["zulu", "main"].map(str::to_owned);

        let targets = in_candidate_order(messages(), &candidates, &selected)
            .expect("keys taken from the candidates should resolve");

        assert_eq!(branches(&targets), ["main", "zulu"]);
    }

    #[test]
    fn a_branch_outside_of_the_candidates_is_rejected() {
        let candidates = targets();
        let selected = ["main".to_owned(), "elsewhere".to_owned()];

        let err = in_candidate_order(messages(), &candidates, &selected)
            .expect_err("an unknown branch is rejected");

        assert!(
            err.to_string().contains("elsewhere"),
            "the unknown branch should be named: {err:#}"
        );
    }

    #[test]
    fn a_partial_prefix_of_a_candidate_name_is_not_accepted() {
        let candidates = targets();
        let selected = ["mai".to_owned()];

        assert!(
            in_candidate_order(messages(), &candidates, &selected).is_err(),
            "the key must match a candidate exactly"
        );
    }

    #[test]
    fn the_fixed_target_is_named_in_the_line_that_replaces_the_finder() {
        let message = fixed_target_message(messages(), &target("main", true));

        assert!(
            message.contains("main"),
            "the branch should be named: {message}"
        );
        assert!(
            message.contains(messages().pull().single_target_reason()),
            "the reason the finder was skipped should be given: {message}"
        );
        assert_eq!(message.lines().count(), 1, "1 行に収める: {message}");
    }

    #[test]
    fn the_fixed_target_is_reported_to_the_given_writer() {
        let only = target("main", true);
        let mut written = Vec::new();

        report_target(messages(), &mut written, &only).expect("writing to a buffer should succeed");

        assert_eq!(
            String::from_utf8(written).expect("the message should be utf-8"),
            format!(
                "{message}\n",
                message = fixed_target_message(messages(), &only)
            )
        );
    }

    /// 実行器へ渡された git の引数の記録（実行順）。
    type Calls = std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>;

    /// 引数ごとに結果を決められる実行器を作り、呼び出し履歴を記録する。
    ///
    /// ネットワークや git の有無に依存せず、集計・中断・組み立てた引数を検証するためのもの
    /// （`gz fetch --siblings` の `recording` と同方式）。
    fn recording(
        mut outcome: impl FnMut(&[String]) -> crate::error::Result<()>,
    ) -> (Calls, impl FnMut(&[&str]) -> crate::error::Result<()>) {
        let calls: Calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = std::rc::Rc::clone(&calls);

        let runner = move |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            let result = outcome(&args);
            recorded.borrow_mut().push(args);
            result
        };

        (calls, runner)
    }

    /// すべての実行が成功する実行器の結果。
    fn succeed(_args: &[String]) -> crate::error::Result<()> {
        Ok(())
    }

    /// git コマンドが非ゼロ終了した場合の結果（到達不能・fast-forward 不可など）。
    fn run_failure() -> crate::error::Result<()> {
        Err(Error::GitRunFailed {
            command: "git fetch".to_owned(),
            code: Some(1),
        })
    }

    /// 記録された呼び出しを取り出す。
    fn recorded(calls: &Calls) -> Vec<Vec<String>> {
        calls.borrow().clone()
    }

    /// 複数のリモートにまたがる候補一覧（候補順）。
    fn mixed_targets() -> Vec<PullTarget> {
        vec![
            target_on("origin", "main", true),
            target_on("origin", "alpha", false),
            target_on("upstream", "zulu", false),
        ]
    }

    #[test]
    fn every_remote_is_listed_once_in_name_order() {
        let candidates = [
            target_on("upstream", "main", true),
            target_on("origin", "alpha", false),
            target_on("upstream", "zulu", false),
        ];
        let targets: Vec<&PullTarget> = candidates.iter().collect();

        assert_eq!(
            target_remotes(&targets),
            ["origin", "upstream"],
            "one fetch per remote is enough to update every tracking reference"
        );
    }

    #[test]
    fn the_remotes_are_fetched_before_any_branch_is_integrated() {
        let candidates = mixed_targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();
        let (calls, runner) = recording(succeed);
        let mut written = Vec::new();

        let summary = pull_each(messages(), &targets, &mut written, runner)
            .expect("a successful run should not fail");

        assert_eq!(summary.succeeded, 3);
        assert!(summary.failed.is_empty());
        assert_eq!(
            recorded(&calls),
            [
                remote_fetch_args("origin"),
                remote_fetch_args("upstream"),
                current_branch_args(&candidates[0]),
                other_branch_args(&candidates[1]),
                other_branch_args(&candidates[2]),
            ]
        );
    }

    #[test]
    fn a_branch_is_not_integrated_when_its_remote_could_not_be_fetched() {
        let candidates = mixed_targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();
        let (calls, runner) = recording(|args: &[String]| {
            if args == remote_fetch_args("origin") {
                run_failure()
            } else {
                Ok(())
            }
        });
        let mut written = Vec::new();

        let summary = pull_each(messages(), &targets, &mut written, runner)
            .expect("a remote failure is recorded, not propagated");

        assert_eq!(
            summary.failed,
            ["main", "alpha"],
            "every branch following the failed remote must be counted as failed"
        );
        assert_eq!(
            summary.succeeded, 1,
            "the branches of the other remote must still be integrated"
        );
        let calls = recorded(&calls);
        assert!(
            !calls.contains(&current_branch_args(&candidates[0]))
                && !calls.contains(&other_branch_args(&candidates[1])),
            "a stale tracking reference must not be integrated: {calls:?}"
        );
        assert!(
            calls.contains(&other_branch_args(&candidates[2])),
            "the unaffected branch must still run: {calls:?}"
        );
    }

    #[test]
    fn a_skipped_branch_names_both_the_remote_and_the_branch() {
        let line = skipped_line(messages(), &target_on("upstream", "alpha", false));

        assert!(
            line.contains("upstream"),
            "the remote should be named: {line}"
        );
        assert!(line.contains("alpha"), "the branch should be named: {line}");
        assert_eq!(line.lines().count(), 1, "1 行に収める: {line}");
    }

    #[test]
    fn another_branch_is_updated_by_copying_the_tracking_reference() {
        let arguments = other_branch_args(&target("alpha", false));

        assert_eq!(
            arguments,
            ["fetch", ".", "refs/remotes/origin/alpha:refs/heads/alpha"]
        );
    }

    #[test]
    fn the_refspec_never_forces_an_update() {
        // `+` を付けると非 fast-forward の更新が通り、ローカルのコミットを失わせる
        let arguments = other_branch_args(&target("alpha", false));
        let refspec = arguments.last().expect("the refspec is the last argument");

        assert!(
            !refspec.starts_with('+'),
            "a forced refspec would drop local commits: {refspec}"
        );
    }

    #[test]
    fn no_argument_of_any_branch_carries_a_forced_refspec() {
        let candidates = [
            target("main", true),
            target("alpha", false),
            target_on("upstream", "feature/login", false),
        ];

        for candidate in candidates {
            for argument in integrate_args(&candidate) {
                assert!(
                    !argument.contains('+'),
                    "no argument may force an update: {argument}"
                );
            }
        }
    }

    #[test]
    fn the_current_branch_is_integrated_exactly_like_gz_sync_ff_only() {
        for candidate in [
            target("main", true),
            target_on("upstream", "feature/login", true),
        ] {
            assert_eq!(
                integrate_args(&candidate),
                mode_args(PullMode::FfOnly, &candidate.tracking_ref),
                "the two commands must not drift apart: {candidate:?}"
            );
        }
    }

    #[test]
    fn the_current_branch_is_integrated_with_a_fast_forward_only_merge() {
        assert_eq!(
            integrate_args(&target("main", true)),
            ["merge", "--ff-only", "refs/remotes/origin/main"]
        );
    }

    #[test]
    fn the_progress_of_every_branch_is_written_before_it_runs() {
        let candidates = targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();
        let (_calls, runner) = recording(succeed);
        let mut written = Vec::new();

        pull_each(messages(), &targets, &mut written, runner).expect("the run should succeed");

        let text = String::from_utf8(written).expect("the progress should be utf-8");
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            ["[1/3] main", "[2/3] alpha", "[3/3] zulu"]
        );
    }

    #[test]
    fn a_branch_that_cannot_fast_forward_does_not_stop_the_remaining_ones() {
        let candidates = targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();
        let failing = other_branch_args(&candidates[1]);
        let (calls, runner) = recording(move |args: &[String]| {
            if args == failing {
                run_failure()
            } else {
                Ok(())
            }
        });
        let mut written = Vec::new();

        let summary = pull_each(messages(), &targets, &mut written, runner)
            .expect("a branch failure is recorded, not propagated");

        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.failed, ["alpha"]);
        assert!(summary.has_fast_forward_failure());
        assert_eq!(
            recorded(&calls).len(),
            4,
            "every branch must be visited after one fetch of the remote"
        );
    }

    #[test]
    fn a_failure_to_start_git_stops_the_run_before_any_branch() {
        let candidates = targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();
        let (calls, runner) = recording(|_args: &[String]| Err(Error::GitNotFound));
        let mut written = Vec::new();

        let err = pull_each(messages(), &targets, &mut written, runner)
            .expect_err("a broken environment must stop the run");

        assert!(
            err.chain()
                .any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::GitNotFound))),
            "the reason should be kept: {err:#}"
        );
        assert_eq!(
            recorded(&calls).len(),
            1,
            "the branches after the failure must not be visited"
        );
        assert!(
            String::from_utf8(written)
                .expect("the progress should be utf-8")
                .is_empty(),
            "no branch should be announced once the run is stopped"
        );
    }

    #[test]
    fn a_spawn_failure_stops_the_remaining_branches() {
        let candidates = targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();
        let (calls, runner) = recording(|args: &[String]| {
            if args.first().map(String::as_str) == Some("merge") {
                Err(Error::GitSpawnFailed {
                    args: args.join(" "),
                    source: std::io::Error::other("denied"),
                })
            } else {
                Ok(())
            }
        });
        let mut written = Vec::new();

        let err = pull_each(messages(), &targets, &mut written, runner)
            .expect_err("a broken environment must stop the run");

        assert!(
            err.to_string().contains("main"),
            "the branch should be named: {err:#}"
        );
        assert_eq!(
            recorded(&calls).len(),
            2,
            "the branches after the failure must not be visited"
        );
    }

    #[test]
    fn a_run_without_failures_is_a_success() {
        let summary = PullSummary {
            succeeded: 3,
            ..PullSummary::default()
        };

        assert!(!summary.has_failure());
        assert_eq!(summary_line(messages(), &summary), "成功 3 件 / 失敗 0 件");
    }

    #[test]
    fn the_summary_names_the_branches_that_failed() {
        let summary = PullSummary {
            succeeded: 1,
            failed: vec!["alpha".to_owned(), "zulu".to_owned()],
            not_fast_forwarded: 2,
        };

        assert!(summary.has_failure());
        let line = summary_line(messages(), &summary);
        assert!(
            line.contains("成功 1 件 / 失敗 2 件"),
            "both counts should be shown: {line}"
        );
        assert!(
            line.contains("alpha") && line.contains("zulu"),
            "the failed branches should be named: {line}"
        );
        assert_eq!(line.lines().count(), 1, "1 行に収める: {line}");
    }

    #[test]
    fn nothing_is_suggested_when_every_branch_succeeded() {
        let summary = PullSummary {
            succeeded: 2,
            ..PullSummary::default()
        };
        let mut written = Vec::new();

        report_summary(messages(), &mut written, &summary)
            .expect("writing to a buffer should succeed");

        let text = String::from_utf8(written).expect("the summary should be utf-8");
        assert_eq!(
            text.lines().count(),
            1,
            "the summary is all there is: {text}"
        );
        assert!(
            !text.contains("gz sync"),
            "an unnecessary next step must not be suggested: {text}"
        );
    }

    #[test]
    fn gz_sync_is_suggested_when_a_fast_forward_failed() {
        let summary = PullSummary {
            succeeded: 1,
            failed: vec!["alpha".to_owned()],
            not_fast_forwarded: 1,
        };
        let mut written = Vec::new();

        report_summary(messages(), &mut written, &summary)
            .expect("writing to a buffer should succeed");

        let text = String::from_utf8(written).expect("the summary should be utf-8");
        assert!(
            text.contains("gz pull --rebase") && text.contains("gz pull --merge"),
            "both ways of resolving a divergence should be offered: {text}"
        );
        assert_eq!(text.lines().count(), 2, "1 行だけ添える: {text}");
    }

    #[test]
    fn nothing_is_suggested_for_a_branch_that_was_never_attempted() {
        // upstream の取得に失敗して飛ばしたブランチは fast-forward を試していないため、
        // `gz pull --rebase` を案内しても解決しない
        let summary = PullSummary {
            succeeded: 0,
            failed: vec!["main".to_owned()],
            not_fast_forwarded: 0,
        };
        let mut written = Vec::new();

        report_summary(messages(), &mut written, &summary)
            .expect("writing to a buffer should succeed");

        let text = String::from_utf8(written).expect("the summary should be utf-8");
        assert!(
            !text.contains("gz sync"),
            "the failure was not a divergence: {text}"
        );
    }

    // --- 文言（FR-27） ---

    /// 引数を取らない文言をまとめて取り出す。
    fn plain_texts(language: Language) -> Vec<&'static str> {
        let pull = language.messages().pull();

        vec![
            pull.targets_read_failed(),
            pull.no_candidates(),
            pull.header(),
            pull.single_target_reason(),
            pull.unmerged_section(),
            pull.fast_forward_guidance(),
            pull.partial_failure(),
        ]
    }

    /// 引数を取る文言と、そこへ展開されるべき引数。
    fn texts_with_arguments(language: Language) -> Vec<(String, &'static str)> {
        let pull = language.messages().pull();

        vec![
            (pull.excluded_count(2), "2"),
            (pull.fixed_target("main"), "main"),
            (pull.selection_not_found("alpha, zulu"), "zulu"),
            (pull.skipped("origin", "alpha"), "origin"),
            (pull.skipped("origin", "alpha"), "alpha"),
            (pull.fetch_start_failed("upstream"), "upstream"),
            (pull.integration_start_failed("main"), "main"),
        ]
    }

    #[test]
    fn every_pull_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            for text in plain_texts(language) {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
        }
    }

    #[test]
    fn every_pull_message_expands_its_arguments() {
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
    fn the_pull_wording_is_translated() {
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
    fn the_english_summary_is_translated_as_well() {
        // 集計は `gz fetch --siblings` と共有する語彙から引く（[`CommonMessages::run_summary`]）
        let summary = PullSummary {
            succeeded: 1,
            failed: vec!["alpha".to_owned()],
            not_fast_forwarded: 1,
        };

        let japanese = summary_line(Language::Japanese.messages(), &summary);
        let english = summary_line(Language::English.messages(), &summary);

        assert_ne!(japanese, english, "the summary must be translated");
        assert!(
            english.contains('1') && english.contains("alpha"),
            "the counts and the failed branch should be shown: {english}"
        );
        assert_eq!(english.lines().count(), 1, "1 行に収める: {english}");
    }

    #[test]
    fn the_english_header_keeps_the_sections_on_one_line() {
        let header = pull_header(Language::English.messages(), &scan(targets(), 2));

        assert_eq!(header.lines().count(), 1, "1 行に収める: {header}");
        assert!(
            header.contains(HEADER_SEPARATOR),
            "the sections should be separated: {header}"
        );
        assert!(
            header.contains("fast-forward"),
            "the integration method is git vocabulary and stays as it is: {header}"
        );
        assert!(header.contains('2'), "the count should be shown: {header}");
    }

    #[test]
    fn the_preview_label_states_the_age_of_the_information_in_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let sections = sections(&preview_source(language.messages(), &target("main", true)));

            assert!(
                sections[0].0.contains("fetch"),
                "{language:?} should state when the information was taken: {label}",
                label = sections[0].0
            );
        }
    }

    #[test]
    fn the_arguments_are_the_same_in_every_language() {
        // 引数の組み立て（[`integrate_args`] / [`remote_fetch_args`]）は文言を持たないため、
        // `gz sync` との一致（`the_current_branch_is_integrated_exactly_like_gz_sync_ff_only`）も
        // 表示言語に左右されない
        let candidates = targets();
        let targets: Vec<&PullTarget> = candidates.iter().collect();

        let mut runs = Vec::new();
        for language in [Language::Japanese, Language::English] {
            let (calls, runner) = recording(succeed);
            let mut written = Vec::new();

            pull_each(language.messages(), &targets, &mut written, runner)
                .expect("a successful run should not fail");

            runs.push((
                recorded(&calls),
                String::from_utf8(written).expect("the progress should be utf-8"),
            ));
        }

        let [
            (japanese_calls, japanese_progress),
            (english_calls, english_progress),
        ] = runs.as_slice()
        else {
            panic!("both languages should have been run: {runs:?}");
        };
        assert_eq!(
            japanese_calls, english_calls,
            "the arguments handed to git must not depend on the language"
        );
        assert!(
            japanese_calls.contains(&mode_args(PullMode::FfOnly, &candidates[0].tracking_ref)),
            "the current branch keeps using the `gz sync` ff-only arguments: {japanese_calls:?}"
        );
        assert_eq!(
            japanese_progress, english_progress,
            "the progress lines carry numbers and branch names only"
        );
    }

    // --- 現在のブランチ 1 本の取り込み（`--rebase` / `--merge`。旧 `gz sync`）---

    fn current_target() -> CurrentTarget {
        CurrentTarget {
            branch: "main".to_owned(),
            remote: "origin".to_owned(),
            reference: "refs/remotes/origin/main".to_owned(),
        }
    }

    #[test]
    fn no_flag_integrates_by_fast_forward_only() {
        assert_eq!(
            PullMode::from_flags(messages(), false, false).expect("no flag is a valid combination"),
            PullMode::FfOnly
        );
    }

    #[test]
    fn each_integration_flag_selects_its_own_mode() {
        assert_eq!(
            PullMode::from_flags(messages(), true, false).expect("--rebase is valid on its own"),
            PullMode::Rebase
        );
        assert_eq!(
            PullMode::from_flags(messages(), false, true).expect("--merge is valid on its own"),
            PullMode::Merge
        );
    }

    #[test]
    fn combining_the_integration_flags_is_rejected() {
        let err = PullMode::from_flags(messages(), true, true)
            .expect_err("the modes are mutually exclusive");

        assert!(
            err.to_string().contains("同時に指定できません"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn the_default_never_falls_back_to_merge_or_rebase() {
        // fast-forward できない場合に暗黙で別の方式へ倒さないことを、引数の形で固定する
        let arguments = mode_args(PullMode::FfOnly, "refs/remotes/origin/main");

        assert_eq!(
            arguments,
            ["merge", "--ff-only", "refs/remotes/origin/main"]
        );
    }

    #[test]
    fn merging_passes_the_tracking_reference_without_the_fast_forward_option() {
        assert_eq!(
            mode_args(PullMode::Merge, "refs/remotes/origin/main"),
            ["merge", "refs/remotes/origin/main"]
        );
    }

    #[test]
    fn rebasing_replays_onto_the_tracking_reference() {
        assert_eq!(
            mode_args(PullMode::Rebase, "refs/remotes/origin/feature/login"),
            ["rebase", "refs/remotes/origin/feature/login"]
        );
    }

    #[test]
    fn fetching_names_the_remote_and_nothing_else() {
        // `--prune` は `gz fetch --prune` の明示指定に限る操作であり、同期では付けない
        assert_eq!(remote_fetch_args("origin"), ["fetch", "origin"]);
    }

    #[test]
    fn the_confirmation_names_the_upstream_the_count_and_the_branch() {
        let header = confirmation_header(messages(), &current_target(), 3, PullMode::FfOnly);

        assert!(
            header.contains("`refs/remotes/origin/main`"),
            "unexpected header: {header}"
        );
        assert!(header.contains("3 件"), "unexpected header: {header}");
        assert!(header.contains("`main`"), "unexpected header: {header}");
    }

    #[test]
    fn only_the_rebase_confirmation_warns_about_the_rewritten_history() {
        let rebase = confirmation_header(messages(), &current_target(), 2, PullMode::Rebase);
        assert!(
            rebase.contains("コミットハッシュが変わります"),
            "a history rewrite must be spelled out: {rebase}"
        );

        for mode in [PullMode::FfOnly, PullMode::Merge] {
            let header = confirmation_header(messages(), &current_target(), 2, mode);
            assert!(
                !header.contains("コミットハッシュが変わります"),
                "{mode:?} does not rewrite history: {header}"
            );
        }
    }

    #[test]
    fn the_history_note_belongs_to_the_rebase_mode_only() {
        let messages = messages();

        assert_eq!(
            PullMode::Rebase.note(messages),
            Some(messages.common().history_rewrite_note())
        );
        assert_eq!(PullMode::FfOnly.note(messages), None);
        assert_eq!(PullMode::Merge.note(messages), None);
    }

    #[test]
    fn the_confirmation_shows_the_command_that_runs() {
        let arguments = mode_args(PullMode::Rebase, "refs/remotes/origin/main");
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

        assert_eq!(
            command_display(&arguments),
            "git rebase refs/remotes/origin/main"
        );
    }

    #[test]
    fn being_up_to_date_is_reported_without_an_error() {
        let mut output = Vec::new();

        report_up_to_date(messages(), &mut output, &current_target(), 0)
            .expect("writing to a buffer cannot fail");

        let text = String::from_utf8(output).expect("the message should be utf-8");
        assert!(text.contains("最新です"), "unexpected message: {text}");
        assert!(
            !text.contains("push していない"),
            "there is nothing to push: {text}"
        );
    }

    #[test]
    fn the_commits_that_are_not_pushed_yet_are_mentioned() {
        let message = up_to_date_message(messages(), &current_target(), 2);

        assert!(
            message.contains("最新です"),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("2 件"),
            "the local commits should be counted: {message}"
        );
    }

    #[test]
    fn every_upstream_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let upstream = language.messages().pull();

            assert!(
                !upstream.conflicting_modes().trim().is_empty(),
                "{language:?} left a message empty"
            );

            for (text, argument) in [
                (upstream.fetch_failed("origin"), "origin"),
                (
                    upstream.tracking_ref_unavailable("main", "origin", "refs/heads/main"),
                    "refs/heads/main",
                ),
                (
                    upstream.unknown_remote("main", "https://example.com/x.git"),
                    "https://example.com/x.git",
                ),
                (
                    upstream.missing_tracking_ref("refs/remotes/origin/main", "origin", "main"),
                    "refs/remotes/origin/main",
                ),
                (
                    upstream.up_to_date("main", "refs/remotes/origin/main"),
                    "refs/remotes/origin/main",
                ),
                (upstream.unpushed_commits(2), "2"),
                (
                    upstream.integration_failed("refs/remotes/origin/main"),
                    "refs/remotes/origin/main",
                ),
                (
                    upstream.confirmation("refs/remotes/origin/main", 3, "main"),
                    "3",
                ),
            ] {
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_upstream_wording_is_translated() {
        let japanese = Language::Japanese.messages().pull();
        let english = Language::English.messages().pull();

        assert_ne!(japanese.conflicting_modes(), english.conflicting_modes());
        assert_ne!(
            japanese.fetch_failed("origin"),
            english.fetch_failed("origin")
        );
        assert_ne!(
            japanese.tracking_ref_unavailable("main", "origin", "refs/heads/main"),
            english.tracking_ref_unavailable("main", "origin", "refs/heads/main")
        );
        assert_ne!(
            japanese.unknown_remote("main", "origin"),
            english.unknown_remote("main", "origin")
        );
        assert_ne!(
            japanese.missing_tracking_ref("refs/remotes/origin/main", "origin", "main"),
            english.missing_tracking_ref("refs/remotes/origin/main", "origin", "main")
        );
        assert_ne!(
            japanese.up_to_date("main", "refs/remotes/origin/main"),
            english.up_to_date("main", "refs/remotes/origin/main")
        );
        assert_ne!(japanese.unpushed_commits(2), english.unpushed_commits(2));
        assert_ne!(
            japanese.integration_failed("refs/remotes/origin/main"),
            english.integration_failed("refs/remotes/origin/main")
        );
        assert_ne!(
            japanese.confirmation("refs/remotes/origin/main", 3, "main"),
            english.confirmation("refs/remotes/origin/main", 3, "main")
        );
    }

    #[test]
    fn the_english_count_agrees_with_the_noun_it_qualifies() {
        let english = Language::English.messages().pull();

        assert!(
            english
                .confirmation("refs/remotes/origin/main", 1, "main")
                .contains("1 commit "),
            "a single commit must not be pluralised: {text}",
            text = english.confirmation("refs/remotes/origin/main", 1, "main")
        );
        assert!(
            english
                .confirmation("refs/remotes/origin/main", 2, "main")
                .contains("2 commits "),
            "several commits must be pluralised: {text}",
            text = english.confirmation("refs/remotes/origin/main", 2, "main")
        );
        assert!(
            english.unpushed_commits(1).contains("1 commit has"),
            "the verb must agree as well: {text}",
            text = english.unpushed_commits(1)
        );
        assert!(
            english.unpushed_commits(3).contains("3 commits have"),
            "the verb must agree as well: {text}",
            text = english.unpushed_commits(3)
        );
    }

    #[test]
    fn a_missing_tracking_reference_names_the_remote_and_the_branch() {
        let message = missing_tracking_message(messages(), &current_target());

        assert!(
            message.contains("refs/remotes/origin/main"),
            "unexpected message: {message}"
        );
        assert!(message.contains("origin"), "unexpected message: {message}");
        assert!(message.contains("main"), "unexpected message: {message}");
    }
}
