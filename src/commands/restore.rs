//! `gz restore` — ファイルを選択して復元・アンステージする（FR-4）。

use anyhow::{Context as _, Result};

use crate::commands::confirmation::confirm;
use crate::commands::file_selection::{FileCandidate, RenameOrigin, resolve, target_paths};
use crate::finder::{FinderItem, PreviewSource, select_many};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{ChangeScope, RevisionFiles, changes, revision_files};

/// `git restore` の適用先。
///
/// 確認プロンプトの要否がこれで決まるため、`--staged` の真偽値をそのまま持ち回らずに型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreTarget {
    /// 作業ツリーを書き換える（既定）。未コミットの変更が失われるため実行前に確認する。
    Worktree,
    /// インデックスのみを書き換える（`--staged`）。作業ツリーは変更しないため確認は不要。
    Index,
}

impl RestoreTarget {
    /// 候補として並べる変更の範囲。
    fn change_scope(self) -> ChangeScope {
        match self {
            RestoreTarget::Worktree => ChangeScope::Worktree,
            RestoreTarget::Index => ChangeScope::Staged,
        }
    }

    /// リネームの変更元も対象に含めるかどうか。
    fn rename_origin(self) -> RenameOrigin {
        match self {
            // アンステージはリネームそのものを取り消すため変更元も戻す
            RestoreTarget::Index => RenameOrigin::Include,
            RestoreTarget::Worktree => RenameOrigin::Exclude,
        }
    }
}

/// ファイルを複数選択し、`git restore` で復元・アンステージする。
///
/// `source` を指定した場合は現在の変更ではなく、そのリビジョンに含まれるファイルが候補になる。
/// 作業ツリーを書き換える場合のみ、実行前に対象を列挙して確認を求める。
///
/// # Errors
///
/// 候補の取得、選択（中断を含む）、確認の否認、`git restore` の実行に失敗した場合にエラーを返す。
pub fn run(
    repository: &gix::Repository,
    target: RestoreTarget,
    source: Option<&str>,
) -> Result<()> {
    let files = match source {
        Some(revision) => Some(
            revision_files(repository, revision)
                .with_context(|| format!("`{revision}` のファイル一覧の取得に失敗しました"))?,
        ),
        None => None,
    };

    let candidates = candidates(repository, target, files.as_ref())?;
    // git へは解決済みのハッシュを渡す。ユーザーの指定した文字列（`HEAD~1` 等）が
    // オプションとして解釈される余地を排除するため
    let revision = files.as_ref().map(|files| files.id.as_str());

    let items = candidates
        .iter()
        .map(|candidate| to_item(candidate, target, revision))
        .collect();
    let selected = select_many(items)?;
    let selected = resolve(&candidates, &selected)?;

    if target == RestoreTarget::Worktree {
        // 確認プロンプトにはユーザーが指定した文字列をそのまま示す（解決済みハッシュより読み取りやすい）
        let targets: Vec<&str> = selected
            .iter()
            .map(|candidate| candidate.key.as_str())
            .collect();
        confirm(&confirmation_header(targets.len(), source), &targets)?;
    }

    let paths = target_paths(&selected);
    let arguments = restore_args(target, revision, &paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).context("git restore の実行に失敗しました")?;

    Ok(())
}

/// 選択候補を組み立てる。
fn candidates(
    repository: &gix::Repository,
    target: RestoreTarget,
    source: Option<&RevisionFiles>,
) -> Result<Vec<FileCandidate>> {
    match source {
        // リビジョン指定時は、現在変更されているかに関わらずそのリビジョンの全ファイルが対象
        Some(files) => Ok(files
            .paths
            .iter()
            .map(|path| FileCandidate::from_path(path))
            .collect()),
        None => Ok(changes(repository, target.change_scope())
            .context("変更ファイル一覧の取得に失敗しました")?
            .iter()
            .map(|change| FileCandidate::from_change(change, target.rename_origin()))
            .collect()),
    }
}

/// 候補を finder のアイテムへ変換する。
fn to_item(candidate: &FileCandidate, target: RestoreTarget, revision: Option<&str>) -> FinderItem {
    FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        PreviewSource::Git(preview_args(target, revision, &candidate.paths)),
    )
}

/// プレビュー用の `git diff` の引数を組み立てる。
///
/// restore を実行した場合に変化する差分（復元元 → 復元先）をそのまま表示する。
fn preview_args(target: RestoreTarget, revision: Option<&str>, paths: &[String]) -> Vec<String> {
    let mut args = vec!["diff".to_owned(), "--color=always".to_owned()];
    if target == RestoreTarget::Index {
        args.push("--staged".to_owned());
    }
    if let Some(revision) = revision {
        args.push(revision.to_owned());
    }
    args.push("--".to_owned());
    args.extend(paths.iter().map(|path| pathspec(path)));
    args
}

/// `git restore [--staged] [--source <rev>] -- <pathspec>...` の引数を組み立てる。
///
/// パスは必ず `--` の後ろへ置き、オプションとして解釈される余地を排除する。
fn restore_args(target: RestoreTarget, revision: Option<&str>, paths: &[String]) -> Vec<String> {
    let mut args = vec!["restore".to_owned()];
    if target == RestoreTarget::Index {
        args.push("--staged".to_owned());
    }
    if let Some(revision) = revision {
        args.push("--source".to_owned());
        args.push(revision.to_owned());
    }
    args.push("--".to_owned());
    args.extend(paths.iter().map(|path| pathspec(path)));
    args
}

/// 確認プロンプトの見出しを組み立てる。
///
/// 何が失われるのかを実行前に明示するため、件数と上書き元を必ず含める。
fn confirmation_header(count: usize, revision: Option<&str>) -> String {
    match revision {
        Some(revision) => format!(
            "以下 {count} 件のファイルを `{revision}` の内容で上書きします（作業ツリーの変更は失われます）:"
        ),
        None => format!("以下 {count} 件のファイルの変更を破棄します（元に戻せません）:"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<String> {
        ["src/main.rs", "dir/with space.txt"]
            .iter()
            .map(|path| (*path).to_owned())
            .collect()
    }

    #[test]
    fn restoring_the_work_tree_needs_neither_staged_nor_source() {
        assert_eq!(
            restore_args(RestoreTarget::Worktree, None, &paths()),
            [
                "restore",
                "--",
                ":(top,literal)src/main.rs",
                ":(top,literal)dir/with space.txt"
            ]
        );
    }

    #[test]
    fn unstaging_adds_the_staged_flag() {
        assert_eq!(
            restore_args(RestoreTarget::Index, None, &["a.txt".to_owned()]),
            ["restore", "--staged", "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn a_source_revision_is_passed_as_an_option_value() {
        assert_eq!(
            restore_args(
                RestoreTarget::Worktree,
                Some("abc123"),
                &["a.txt".to_owned()]
            ),
            ["restore", "--source", "abc123", "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn the_staged_flag_and_the_source_revision_can_be_combined() {
        assert_eq!(
            restore_args(RestoreTarget::Index, Some("abc123"), &["a.txt".to_owned()]),
            [
                "restore",
                "--staged",
                "--source",
                "abc123",
                "--",
                ":(top,literal)a.txt"
            ]
        );
    }

    #[test]
    fn every_path_is_placed_after_the_separator() {
        let arguments = restore_args(
            RestoreTarget::Worktree,
            Some("abc123"),
            &["--not-an-option".to_owned()],
        );

        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .expect("the separator is always present");
        assert_eq!(
            arguments[separator + 1..],
            [":(top,literal)--not-an-option"],
            "a path must never be interpreted as an option"
        );
    }

    #[test]
    fn the_work_tree_preview_diffs_the_index_against_the_work_tree() {
        assert_eq!(
            preview_args(RestoreTarget::Worktree, None, &["a.txt".to_owned()]),
            ["diff", "--color=always", "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn the_index_preview_diffs_head_against_the_index() {
        assert_eq!(
            preview_args(RestoreTarget::Index, None, &["a.txt".to_owned()]),
            [
                "diff",
                "--color=always",
                "--staged",
                "--",
                ":(top,literal)a.txt"
            ]
        );
    }

    #[test]
    fn the_source_preview_diffs_that_revision_against_the_restore_destination() {
        assert_eq!(
            preview_args(
                RestoreTarget::Worktree,
                Some("abc123"),
                &["a.txt".to_owned()]
            ),
            [
                "diff",
                "--color=always",
                "abc123",
                "--",
                ":(top,literal)a.txt"
            ]
        );
    }

    #[test]
    fn a_rename_preview_shows_both_of_its_paths() {
        assert_eq!(
            preview_args(
                RestoreTarget::Index,
                None,
                &["new.txt".to_owned(), "old.txt".to_owned()]
            ),
            [
                "diff",
                "--color=always",
                "--staged",
                "--",
                ":(top,literal)new.txt",
                ":(top,literal)old.txt"
            ]
        );
    }

    #[test]
    fn unstaging_a_rename_targets_the_original_path_as_well() {
        let change = crate::git::read::FileChange {
            path: "new.txt".to_owned(),
            original_path: Some("old.txt".to_owned()),
            index_status: 'R',
            worktree_status: ' ',
        };

        let candidate = FileCandidate::from_change(&change, RestoreTarget::Index.rename_origin());

        assert_eq!(candidate.paths, ["new.txt", "old.txt"]);
    }

    #[test]
    fn restoring_the_work_tree_of_a_rename_targets_the_new_path_only() {
        let change = crate::git::read::FileChange {
            path: "new.txt".to_owned(),
            original_path: Some("old.txt".to_owned()),
            index_status: 'R',
            worktree_status: 'M',
        };

        let candidate =
            FileCandidate::from_change(&change, RestoreTarget::Worktree.rename_origin());

        assert_eq!(candidate.paths, ["new.txt"]);
    }

    #[test]
    fn each_target_lists_its_own_side_of_the_changes() {
        assert_eq!(
            RestoreTarget::Worktree.change_scope(),
            ChangeScope::Worktree
        );
        assert_eq!(RestoreTarget::Index.change_scope(), ChangeScope::Staged);
    }

    #[test]
    fn the_confirmation_states_how_many_files_lose_their_changes() {
        assert_eq!(
            confirmation_header(3, None),
            "以下 3 件のファイルの変更を破棄します（元に戻せません）:"
        );
    }

    #[test]
    fn the_confirmation_names_the_revision_that_overwrites_the_work_tree() {
        let header = confirmation_header(1, Some("abc123"));

        assert!(
            header.contains("abc123") && header.contains("1 件"),
            "unexpected header: {header}"
        );
    }

    #[test]
    fn an_item_keeps_the_path_as_its_key() {
        let candidate = FileCandidate::from_path("dir/with space.txt");

        let item = to_item(&candidate, RestoreTarget::Worktree, Some("abc123"));

        assert_eq!(item.key(), "dir/with space.txt");
    }
}
