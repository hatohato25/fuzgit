//! `gz pull` — 複数のブランチを upstream へ一括で追随させる（FR-24）。
//!
//! 選ばせるのは「どのブランチを upstream へ追随させるか」だけで、remote × branch や
//! 取り込み方式は選ばせない。取り込みは **fast-forward のみ**に固定する
//! （方式を選んで 1 本だけ同期するのは `gz sync`（FR-19）の役割）。
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

use anyhow::{Context as _, Result, bail};

use crate::commands::in_progress;
use crate::commands::sync::{SyncMode, integrate_args as sync_integrate_args};
use crate::error::Error;
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with};
use crate::git::exec::run_git;
use crate::git::read::{PullScan, PullTarget, operation_in_progress, pull_targets};
use crate::i18n::{Language, Messages};

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
const UPSTREAM_ARROW: &str = "  →  ";

/// リモート追跡参照の接頭辞。表示用の短縮名（`origin/main`）を得るために取り除く。
const TRACKING_PREFIX: &str = "refs/remotes/";

/// ヘッダー内の区切り（`gz fetch --siblings` と同じ体裁）。
const HEADER_SEPARATOR: &str = "  |  ";

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
) -> Result<()> {
    // 進行中の merge / rebase を残したまま新しい取り込みは開始できないため、
    // 候補を出す前に復帰メニューへ委譲する（`gz sync` と同じ）
    if let Some(operation) = operation_in_progress(repository) {
        return in_progress::run(language, messages, repository, operation);
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
            let options = FinderOptions::new(SelectionMode::Multi)
                .with_header(pull_header(messages, &scan))
                // 事前選択は表示文字列の完全一致で判定される（`crate::finder::FinderOptions`）
                .with_preselect(preselect(&scan.targets));
            let selected = select_many_with(items(language, messages, &scan.targets), &options)?;

            // skim は選択した順に返すため、候補一覧の順序（現在のブランチが先頭、
            // 以降は名前順）へ揃え直したうえで、キーが候補に含まれることを検証する
            in_candidate_order(messages, &scan.targets, &selected)?
        }
    };

    let summary = pull_each(messages, &targets, &mut std::io::stderr(), |arguments| {
        run_git(language, arguments)
    })?;
    report_summary(messages, &mut std::io::stderr(), &summary)?;

    if summary.has_failure() {
        // 集計は表示済み。ここでは終了コードを 1 にするためにエラーを返す
        bail!(messages.pull().partial_failure());
    }

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// ahead / behind は**載せない**。候補生成の時点で分かるのは前回の fetch までに取得済みの
/// 追跡参照との差であり、これから fetch して取り込む本数とは一致しない。古い件数を
/// 添えると「2 件だけ入る」と読めてしまうため、件数は取り込み後に git 自身が示すものに委ねる。
fn display_line(target: &PullTarget) -> String {
    let mark = if target.is_current {
        CURRENT_MARK
    } else {
        OTHER_MARK
    };

    format!(
        "{mark}{branch}{UPSTREAM_ARROW}{upstream}",
        branch = target.branch,
        upstream = tracking_name(target)
    )
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
fn preselect(targets: &[PullTarget]) -> Vec<String> {
    targets
        .iter()
        .filter(|target| target.is_current)
        .map(display_line)
        .collect()
}

/// 取り込み対象を finder の候補へ変換する。
///
/// 照合キーはブランチ名（[`pull_targets`] が列挙した値であり、ユーザーの自由入力ではない）。
fn items(language: Language, messages: &dyn Messages, targets: &[PullTarget]) -> Vec<FinderItem> {
    targets
        .iter()
        .map(|target| {
            FinderItem::new(
                display_line(target),
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
    /// （`gz sync --rebase` を案内しても解決しない）。
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
/// 並列化しない（git 自身も複数リモートの fetch を既定で逐次実行する。man git-fetch）。
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
/// 操作であり、取り込みのついでに行うものではない（`gz sync` と同じ）。
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
/// 引数は `gz sync`（FR-19）の ff-only と同一であるため、組み立てを重複させず
/// [`sync_integrate_args`] を再利用する（design.md「`gz sync` との境界」）。
fn current_branch_args(target: &PullTarget) -> Vec<String> {
    sync_integrate_args(SyncMode::FfOnly, &target.tracking_ref)
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

#[cfg(test)]
mod tests {
    use super::*;

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
                message.contains("gz push -u"),
                "the fuzgit way of setting an upstream should be offered: {message}"
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
        assert_eq!(
            preselect(&targets()),
            vec![display_line(&target("main", true))]
        );
    }

    #[test]
    fn nothing_is_preselected_on_a_detached_head() {
        // detached HEAD では現在のブランチが無い（`pull_targets` はエラーにしない）
        let candidates = vec![target("alpha", false), target("zulu", false)];

        assert!(preselect(&candidates).is_empty());
    }

    #[test]
    fn a_candidate_is_keyed_by_its_branch_name() {
        let items = items(Language::Japanese, messages(), &targets());

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
                crate::commands::sync::integrate_args(SyncMode::FfOnly, &candidate.tracking_ref),
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
            text.contains("gz sync --rebase") && text.contains("gz sync --merge"),
            "both ways of resolving a divergence should be offered: {text}"
        );
        assert_eq!(text.lines().count(), 2, "1 行だけ添える: {text}");
    }

    #[test]
    fn nothing_is_suggested_for_a_branch_that_was_never_attempted() {
        // upstream の取得に失敗して飛ばしたブランチは fast-forward を試していないため、
        // `gz sync --rebase` を案内しても解決しない
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
            japanese_calls.contains(&crate::commands::sync::integrate_args(
                SyncMode::FfOnly,
                &candidates[0].tracking_ref
            )),
            "the current branch keeps using the `gz sync` ff-only arguments: {japanese_calls:?}"
        );
        assert_eq!(
            japanese_progress, english_progress,
            "the progress lines carry numbers and branch names only"
        );
    }
}
