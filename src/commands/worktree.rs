//! `gz worktree` — worktree の一覧・作成・削除・整理（FR-21）。
//!
//! 引数なしの `gz worktree` は一覧から 1 件選び、そのパスだけを標準出力へ出す
//! （`cd "$(gz worktree)"` のようなシェル連携用途。メッセージ類は標準エラーへ出す）。
//!
//! 一覧の読み取りは gix ではなく `git worktree list --porcelain -z` のキャプチャに倒している
//! （gix の `Repository::worktrees()` は main worktree を含まず、チェックアウト中のブランチも
//! 持たないため。design.md「追加機能の技術調査」）。
//!
//! move / lock / unlock / repair と `remove --force` は提供しない
//! （requirements.md「スコープ外」。いずれも素の git で 1 コマンドで済み、未コミット変更を
//! 破棄する強制削除は確認プロンプトだけでは担保しにくいため git に委ねる）。

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::cli::WorktreeCommand;
use crate::commands::confirmation::confirm;
use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::{capture_git_stderr_in, run_git};
use crate::git::read::{
    BranchInfo, BranchScope, WorktreeInfo, branches, checked_out_branches, worktrees,
};
use crate::git::repo::workdir;

/// 整理対象を報告させる `git worktree prune` の引数（何も削除しない）。
///
/// `--verbose` を併せると、整理される worktree とその理由が 1 行ずつ報告される。
/// この報告は標準出力ではなく標準エラーへ出る（git 2.55 で実測。
/// [`capture_git_stderr_in`] を使うのはそのため）。
const PRUNE_DRY_RUN_ARGS: [&str; 4] = ["worktree", "prune", "--dry-run", "--verbose"];

/// プレビューに表示する最大コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// main worktree（リポジトリ本体の作業ツリー）の表示。
const MAIN_LABEL: &str = "main";

/// linked worktree（`git worktree add` で追加した作業ツリー）の表示。
const LINKED_LABEL: &str = "linked";

/// 種別ラベルを揃える桁数（`linked` の 6 文字に合わせる）。
const KIND_WIDTH: usize = 6;

/// ブランチではなくコミットを直接チェックアウトしている worktree の表示。
const DETACHED_LABEL: &str = "detached";

/// 作業ツリーを持たない（bare）リポジトリ本体の表示。
const BARE_LABEL: &str = "bare";

/// ロックされている worktree に付ける印。
const LOCKED_MARK: &str = "locked";

/// `git worktree prune` の対象になり得る worktree に付ける印。
const PRUNABLE_MARK: &str = "prunable";

/// まだコミットが 1 件も無い worktree の `HEAD` 属性の値。
///
/// git はこの場合に全 0 のハッシュを報告する（man git-worktree「Porcelain Format」）。
/// 実在しないオブジェクトであるため、プレビューの `git log` は必ず失敗する。
const NULL_OBJECT_ID: &str = "0000000000000000000000000000000000000000";

/// 削除できる worktree が 1 件も無い場合の案内。
///
/// 一般の「選択できる候補がありません」では、main worktree を候補から除いた結果である
/// ことが分からないため、原因を明示する。
const NO_REMOVABLE_MESSAGE: &str = "削除できる worktree がありません\
（main worktree は `git worktree remove` の対象になりません）";

/// worktree に割り当てられるローカルブランチが 1 件も無い場合の案内。
const NO_AVAILABLE_BRANCH_MESSAGE: &str = "worktree に割り当てられるローカルブランチがありません\
（他の worktree でチェックアウト中のブランチは対象になりません）";

/// 整理対象が 1 件も無い場合に伝えること。
///
/// 対象が無いのは正常な状態であり、エラーにはしない（requirements.md FR-21）。
const NOTHING_TO_PRUNE_MESSAGE: &str = "整理する worktree はありません";

/// worktree 削除の確認プロンプトの見出し。
const REMOVE_HEADER: &str = "以下の worktree を削除します\
（作業ツリーのディレクトリと管理情報が削除されます。ブランチは残ります）";

/// worktree 整理の確認プロンプトの見出し。
const PRUNE_HEADER: &str = "以下の worktree の管理情報を整理します";

/// worktree のサブコマンドを対応する処理へ振り分ける。
///
/// サブコマンドを省略した場合は一覧からの選択（パスの出力）を行う。
///
/// # Errors
///
/// 各操作が失敗した場合にエラーを返す。
pub fn run(repository: &gix::Repository, command: Option<&WorktreeCommand>) -> Result<()> {
    match command {
        None => list(repository),
        Some(WorktreeCommand::Add { path }) => add(repository, path),
        Some(WorktreeCommand::Remove) => remove(repository),
        Some(WorktreeCommand::Prune) => prune(repository),
    }
}

/// worktree を 1 件選び、そのパスを標準出力へ書き出す。
///
/// # Errors
///
/// 一覧の取得、選択（中断を含む）、標準出力への書き込みに失敗した場合にエラーを返す。
fn list(repository: &gix::Repository) -> Result<()> {
    let worktrees = read_worktrees(repository)?;
    let candidates: Vec<&WorktreeInfo> = worktrees.iter().collect();

    let selected = choose(&candidates)?;

    // パイプ利用（`cd "$(gz worktree)"`）を想定し、stdout にはパス以外を混ぜない。
    // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
    writeln!(std::io::stdout(), "{path}", path = selected.path)
        .context("標準出力への書き込みに失敗しました")?;

    Ok(())
}

/// ブランチを 1 件選び、新しい worktree を作成する。
///
/// 候補は他の worktree で使用中でないローカルブランチに限る。git は同じブランチを
/// 複数の worktree で同時にチェックアウトできないため、使用中のブランチを選べても
/// 必ず失敗する選択肢になるだけである。
///
/// # Errors
///
/// パスが UTF-8 でない場合、候補の取得・選択（中断を含む）、`git worktree add` の実行に
/// 失敗した場合にエラーを返す。
fn add(repository: &gix::Repository, path: &Path) -> Result<()> {
    // `git` へ渡す引数は文字列である必要がある。解釈できないパスを勝手に変換すると
    // 意図と異なる場所に worktree を作ってしまうため、推測せずエラーにする
    let path = path.to_str().ok_or_else(|| {
        anyhow!(
            "worktree のパス `{path}` を UTF-8 として解釈できません",
            path = path.display()
        )
    })?;

    let worktrees = read_worktrees(repository)?;
    let in_use = checked_out_branches(&worktrees);
    let locals =
        branches(repository, BranchScope::Local).context("ブランチ一覧の取得に失敗しました")?;
    let candidates = available_branches(&locals, &in_use);
    if candidates.is_empty() {
        bail!(NO_AVAILABLE_BRANCH_MESSAGE);
    }

    let items = candidates
        .iter()
        .map(|branch| to_branch_item(branch))
        .collect();
    let selected = select_one(items)?;

    // ブランチ名は `--` の後ろに置いてもパスと区別できない位置に来るため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let branch = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!("選択されたブランチ `{selected}` が候補に見つかりません"))?;

    let arguments = add_args(path, &branch.name);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).with_context(|| format!("worktree `{path}` の作成に失敗しました"))?;

    Ok(())
}

/// linked worktree を 1 件選び、確認のうえ削除する。
///
/// main worktree は `git worktree remove` の対象外であるため候補に含めない。
/// locked / 未コミット変更ありの worktree は git が拒否するため、その理由は git の
/// メッセージのまま表示する（fuzgit 側で判定を二重に実装しない）。
///
/// # Errors
///
/// 一覧の取得、選択（中断を含む）、`git worktree remove` の実行に失敗した場合にエラーを返す。
/// 確認プロンプトで承認が得られなかった場合は [`crate::error::Error::Cancelled`]。
fn remove(repository: &gix::Repository) -> Result<()> {
    let worktrees = read_worktrees(repository)?;
    let candidates = removable(&worktrees);
    if candidates.is_empty() {
        bail!(NO_REMOVABLE_MESSAGE);
    }

    let selected = choose(&candidates)?;

    // 削除されるのは作業ツリーのディレクトリごとであるため、対象を示して同意を求める
    confirm(REMOVE_HEADER, &[&display_line(selected)])?;

    let arguments = remove_args(&selected.path);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).with_context(|| {
        format!(
            "worktree `{path}` の削除に失敗しました",
            path = selected.path
        )
    })?;

    Ok(())
}

/// 実体を失った worktree の管理情報を、確認のうえ整理する。
///
/// 整理対象の判定は fuzgit では行わず、`git worktree prune --dry-run --verbose` の報告を
/// そのまま提示する（design.md「stale 判定は git に委ねる」）。
///
/// # Errors
///
/// ドライランの実行、`git worktree prune` の実行に失敗した場合にエラーを返す。
/// 確認プロンプトで承認が得られなかった場合は [`crate::error::Error::Cancelled`]。
fn prune(repository: &gix::Repository) -> Result<()> {
    let report = capture_git_stderr_in(workdir(repository)?, &PRUNE_DRY_RUN_ARGS)
        .context("整理対象の確認に失敗しました")?;
    let targets = prune_targets(&report);

    if targets.is_empty() {
        return report_nothing_to_prune(&mut std::io::stderr());
    }

    let lines: Vec<&str> = targets.iter().map(String::as_str).collect();
    confirm(PRUNE_HEADER, &lines)?;

    let arguments = prune_args();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).context("worktree の整理に失敗しました")?;

    Ok(())
}

/// worktree 一覧を取得する。
///
/// # Errors
///
/// 作業ツリーが無い場合、`git worktree list` の実行・パースに失敗した場合にエラーを返す。
fn read_worktrees(repository: &gix::Repository) -> Result<Vec<WorktreeInfo>> {
    worktrees(workdir(repository)?).context("worktree 一覧の取得に失敗しました")
}

/// worktree を 1 件選び、候補一覧との照合を経た 1 件を返す。
///
/// # Errors
///
/// 選択（中断を含む）に失敗した場合、選択結果が候補一覧に無い場合にエラーを返す。
fn choose<'a>(candidates: &[&'a WorktreeInfo]) -> Result<&'a WorktreeInfo> {
    let items = candidates
        .iter()
        .map(|worktree| to_item(worktree))
        .collect();
    let selected = select_one(items)?;

    resolve(candidates, &selected)
}

/// 選択されたパスを候補一覧の 1 件へ解決する。
///
/// `git worktree remove` へ渡すパスは、ユーザーの自由入力ではなく fuzgit が
/// `git worktree list` の出力から取り出した値に限る（design.md セキュリティ設計）。
///
/// # Errors
///
/// パスが候補一覧に無い場合にエラーを返す（対象を取り違えたまま git を実行しないよう、
/// 暗黙に読み飛ばさない）。
fn resolve<'a>(candidates: &[&'a WorktreeInfo], selected: &str) -> Result<&'a WorktreeInfo> {
    candidates
        .iter()
        .find(|worktree| worktree.path == selected)
        .copied()
        .ok_or_else(|| anyhow!("選択された worktree `{selected}` が候補に見つかりません"))
}

/// 削除できる worktree（main を除く linked worktree）を候補順のまま抽出する。
fn removable(worktrees: &[WorktreeInfo]) -> Vec<&WorktreeInfo> {
    worktrees
        .iter()
        .filter(|worktree| !worktree.is_main)
        .collect()
}

/// 新しい worktree に割り当てられるローカルブランチを候補順のまま抽出する。
fn available_branches<'a>(
    locals: &'a [BranchInfo],
    in_use: &std::collections::HashSet<String>,
) -> Vec<&'a BranchInfo> {
    locals
        .iter()
        .filter(|branch| !in_use.contains(&branch.name))
        .collect()
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// パスを先頭に置くのは、`cd` 先を探す用途では絞り込みの手掛かりがパスになるため。
/// locked / prunable は該当する場合のみ印を付ける（該当しない旨は並べない）。
fn display_line(worktree: &WorktreeInfo) -> String {
    let mut line = format!(
        "{path}  {kind:<width$}  {state}",
        path = worktree.path,
        kind = kind_label(worktree),
        width = KIND_WIDTH,
        state = state_label(worktree)
    );

    if worktree.is_locked {
        line.push_str("  ");
        line.push_str(LOCKED_MARK);
    }
    if worktree.prunable {
        line.push_str("  ");
        line.push_str(PRUNABLE_MARK);
    }

    line
}

/// main worktree かどうかの表示。
fn kind_label(worktree: &WorktreeInfo) -> &'static str {
    if worktree.is_main {
        MAIN_LABEL
    } else {
        LINKED_LABEL
    }
}

/// 何をチェックアウトしているかの表示。
///
/// `git worktree list --porcelain` の `detached` / `bare` は、ブランチ・HEAD の有無で
/// 区別できるため [`WorktreeInfo`] が真偽値として持っていない（`git::read` の方針）。
/// 表示のためにここで読み替える。
fn state_label(worktree: &WorktreeInfo) -> String {
    match (&worktree.branch, &worktree.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(_)) => DETACHED_LABEL.to_owned(),
        // HEAD 属性そのものが無いのは bare なリポジトリ本体だけ
        (None, None) => BARE_LABEL.to_owned(),
    }
}

/// worktree を finder のアイテムへ変換する。
fn to_item(worktree: &WorktreeInfo) -> FinderItem {
    FinderItem::new(
        display_line(worktree),
        worktree.path.clone(),
        preview_source(worktree),
    )
}

/// worktree のプレビュー内容（チェックアウト中の HEAD のコミットログ）を組み立てる。
///
/// linked worktree もリポジトリ本体とオブジェクトデータベースを共有するため、パスへ
/// 移動せずコミットのハッシュを指定するだけでログを読める。
/// 実体の無いコミット（bare・unborn HEAD）はログを引けないため、プレビューを出さない。
fn preview_source(worktree: &WorktreeInfo) -> PreviewSource {
    match &worktree.head {
        Some(head) if head != NULL_OBJECT_ID => PreviewSource::Git(log_preview_args(head)),
        Some(_) | None => PreviewSource::None,
    }
}

/// ブランチを finder のアイテムへ変換する（`gz worktree add` の候補）。
fn to_branch_item(branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        branch.name.clone(),
        branch.name.clone(),
        PreviewSource::Git(log_preview_args(&branch.name)),
    )
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
fn log_preview_args(revision: &str) -> Vec<String> {
    // 末尾の `--` により、リビジョンがパスとして解釈されることを防ぐ
    [
        "log",
        "--color=always",
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        revision,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// `git worktree add -- <path> <branch>` の引数を組み立てる。
///
/// パスはユーザー入力（位置引数）であるため `--` の後ろへ置き、オプションとして
/// 解釈される余地を排除する。`git worktree add` が `--` を受け付けること、`-` で始まる
/// パスもその後ろでは通常のパスとして扱われることは git 2.55 で実機確認済み。
fn add_args(path: &str, branch: &str) -> Vec<String> {
    ["worktree", "add", "--", path, branch]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git worktree remove -- <path>` の引数を組み立てる。
///
/// パスは `git worktree list` の出力に由来する値だが、`-` で始まるディレクトリを
/// 作ることはできるため、`--` の後ろへ置く点は [`add_args`] と揃える。
fn remove_args(path: &str) -> Vec<String> {
    ["worktree", "remove", "--", path]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git worktree prune` の引数を組み立てる。
fn prune_args() -> Vec<String> {
    ["worktree", "prune"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git worktree prune --dry-run --verbose` の報告を 1 行ずつ取り出す。
///
/// 報告は `Removing worktrees/<名前>: <理由>` の形式で、対象が無ければ空になる。
/// 内容の解釈（どれが整理されるのか）は git に委ね、fuzgit は行として確認プロンプトへ
/// 転記するだけであるため、UTF-8 でない場合もロッシー変換して捨てない。
fn prune_targets(report: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(report)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 整理する worktree が無いことを伝えて正常終了する。
///
/// 標準出力はパス出力のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_nothing_to_prune(writer: &mut impl std::io::Write) -> Result<()> {
    writeln!(writer, "{NOTHING_TO_PRUNE_MESSAGE}")
        .context("標準エラー出力への書き込みに失敗しました")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn worktree(path: &str, branch: Option<&str>, is_main: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: path.to_owned(),
            head: Some("1111111111111111111111111111111111111111".to_owned()),
            branch: branch.map(str::to_owned),
            is_main,
            is_locked: false,
            prunable: false,
        }
    }

    fn local(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    fn in_use(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn adding_puts_the_path_and_the_branch_after_the_separator() {
        assert_eq!(
            add_args("../feature", "feature"),
            ["worktree", "add", "--", "../feature", "feature"]
        );
    }

    #[test]
    fn a_path_starting_with_a_dash_stays_a_path() {
        // `--` の後ろではオプションとして解釈されない（git 2.55 で実機確認済み）
        let arguments = add_args("-dashy", "feature");

        assert_eq!(arguments[2], "--");
        assert_eq!(arguments[3], "-dashy");
    }

    #[test]
    fn a_path_containing_spaces_is_passed_as_a_single_argument() {
        let arguments = add_args("../with space", "feature");

        assert_eq!(arguments[3], "../with space");
    }

    #[test]
    fn removing_puts_the_path_after_the_separator() {
        assert_eq!(
            remove_args("/repo/../feature"),
            ["worktree", "remove", "--", "/repo/../feature"]
        );
    }

    #[test]
    fn pruning_takes_no_option_when_it_actually_runs() {
        // ドライランと本実行で引数を取り違えないことを固定する
        assert_eq!(prune_args(), ["worktree", "prune"]);
        assert_eq!(
            PRUNE_DRY_RUN_ARGS,
            ["worktree", "prune", "--dry-run", "--verbose"]
        );
    }

    #[test]
    fn a_dry_run_without_a_report_means_there_is_nothing_to_prune() {
        for report in [&b""[..], &b"\n"[..], &b"  \n\n"[..]] {
            assert!(
                prune_targets(report).is_empty(),
                "unexpected targets for {report:?}"
            );
        }
    }

    #[test]
    fn every_reported_line_becomes_a_confirmation_target() {
        let report = b"Removing worktrees/gone: gitdir file points to non-existent location\n\
Removing worktrees/old: gitdir file points to non-existent location\n";

        assert_eq!(
            prune_targets(report),
            [
                "Removing worktrees/gone: gitdir file points to non-existent location",
                "Removing worktrees/old: gitdir file points to non-existent location"
            ],
            "the reasons reported by git must be shown as they are"
        );
    }

    #[test]
    fn a_report_that_is_not_utf8_is_still_shown() {
        let report = [&b"Removing worktrees/"[..], &[0xff, 0xfe], &b": gone\n"[..]].concat();

        assert_eq!(
            prune_targets(&report).len(),
            1,
            "a single undecodable byte must not hide the whole report"
        );
    }

    #[test]
    fn the_main_worktree_is_never_a_removal_candidate() {
        let worktrees = [
            worktree("/repo", Some("main"), true),
            worktree("/repo/../feature", Some("feature"), false),
        ];

        let candidates = removable(&worktrees);

        assert_eq!(
            candidates
                .iter()
                .map(|worktree| worktree.path.as_str())
                .collect::<Vec<&str>>(),
            ["/repo/../feature"]
        );
    }

    #[test]
    fn a_repository_without_a_linked_worktree_offers_nothing_to_remove() {
        let worktrees = [worktree("/repo", Some("main"), true)];

        assert!(removable(&worktrees).is_empty());
    }

    #[test]
    fn the_branches_used_by_another_worktree_are_not_offered() {
        let locals = [local("feature"), local("main"), local("wip")];

        let candidates = available_branches(&locals, &in_use(&["main", "wip"]));

        assert_eq!(
            candidates
                .iter()
                .map(|branch| branch.name.as_str())
                .collect::<Vec<&str>>(),
            ["feature"],
            "a branch checked out elsewhere can never be added"
        );
    }

    #[test]
    fn every_branch_is_offered_when_no_worktree_holds_one() {
        let locals = [local("feature"), local("main")];

        assert_eq!(available_branches(&locals, &in_use(&[])).len(), 2);
    }

    #[test]
    fn a_selected_path_is_resolved_against_the_candidates() {
        let worktrees = [
            worktree("/repo", Some("main"), true),
            worktree("/repo/../feature", Some("feature"), false),
        ];
        let candidates: Vec<&WorktreeInfo> = worktrees.iter().collect();

        let resolved = resolve(&candidates, "/repo/../feature").expect("a listed path resolves");

        assert_eq!(resolved.branch.as_deref(), Some("feature"));
    }

    #[test]
    fn a_path_outside_of_the_candidates_is_rejected() {
        let worktrees = [worktree("/repo", Some("main"), true)];
        let candidates: Vec<&WorktreeInfo> = worktrees.iter().collect();

        let err = resolve(&candidates, "/elsewhere").expect_err("an unknown path must be rejected");

        assert!(
            err.to_string().contains("/elsewhere"),
            "the unknown path should be named: {err:#}"
        );
    }

    #[test]
    fn the_main_worktree_cannot_be_resolved_from_the_removal_candidates() {
        // 候補から外した main worktree のパスが、後段の解決で拾われないことを確かめる
        let worktrees = [
            worktree("/repo", Some("main"), true),
            worktree("/repo/../feature", Some("feature"), false),
        ];

        let err =
            resolve(&removable(&worktrees), "/repo").expect_err("the main worktree is excluded");

        assert!(
            err.to_string().contains("/repo"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn a_line_shows_the_path_the_kind_and_the_checked_out_branch() {
        assert_eq!(
            display_line(&worktree("/repo", Some("main"), true)),
            "/repo  main    main"
        );
        assert_eq!(
            display_line(&worktree("/repo/../feature", Some("feature/login"), false)),
            "/repo/../feature  linked  feature/login"
        );
    }

    #[test]
    fn a_detached_worktree_is_shown_as_such() {
        let mut detached = worktree("/repo/detached", None, false);
        detached.branch = None;

        assert!(
            display_line(&detached).contains(DETACHED_LABEL),
            "unexpected line: {line}",
            line = display_line(&detached)
        );
    }

    #[test]
    fn a_bare_repository_is_distinguished_from_a_detached_worktree() {
        let mut bare = worktree("/repo.git", None, true);
        bare.head = None;

        assert_eq!(state_label(&bare), BARE_LABEL);
    }

    #[test]
    fn the_locked_and_prunable_marks_are_added_only_when_they_apply() {
        let plain = worktree("/repo/../feature", Some("feature"), false);
        assert!(!display_line(&plain).contains(LOCKED_MARK));
        assert!(!display_line(&plain).contains(PRUNABLE_MARK));

        let mut marked = plain.clone();
        marked.is_locked = true;
        marked.prunable = true;
        let line = display_line(&marked);
        assert!(line.contains(LOCKED_MARK), "unexpected line: {line}");
        assert!(line.contains(PRUNABLE_MARK), "unexpected line: {line}");
    }

    #[test]
    fn an_item_keeps_the_path_as_its_key() {
        // 決定時に標準出力へ出すのは表示行ではなくパスそのもの
        let item = to_item(&worktree("/repo/../feature", Some("feature"), false));

        assert_eq!(item.key(), "/repo/../feature");
    }

    #[test]
    fn the_preview_reads_the_log_of_the_checked_out_commit() {
        let worktree = worktree("/repo", Some("main"), true);

        assert_eq!(
            preview_source(&worktree),
            PreviewSource::Git(log_preview_args("1111111111111111111111111111111111111111"))
        );
    }

    #[test]
    fn a_worktree_without_a_reachable_commit_has_no_preview() {
        let mut bare = worktree("/repo.git", None, true);
        bare.head = None;
        assert_eq!(preview_source(&bare), PreviewSource::None);

        let mut unborn = worktree("/repo", Some("main"), true);
        unborn.head = Some(NULL_OBJECT_ID.to_owned());
        assert_eq!(
            preview_source(&unborn),
            PreviewSource::None,
            "an unborn HEAD points at no commit"
        );
    }

    #[test]
    fn the_preview_ends_with_a_path_separator() {
        assert_eq!(
            log_preview_args("main"),
            [
                "log",
                "--color=always",
                "--oneline",
                "--decorate",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "main",
                "--"
            ]
        );
    }

    #[test]
    fn a_branch_item_keeps_its_name_as_the_key() {
        assert_eq!(
            to_branch_item(&local("feature/login")).key(),
            "feature/login"
        );
    }

    #[test]
    fn nothing_to_prune_is_reported_without_an_error() {
        let mut output = Vec::new();

        report_nothing_to_prune(&mut output).expect("writing to a buffer cannot fail");

        assert_eq!(
            String::from_utf8(output).expect("the message should be utf-8"),
            format!("{NOTHING_TO_PRUNE_MESSAGE}\n")
        );
    }
}
