//! `gz add` — 未ステージ・未追跡ファイルを選択してステージする（FR-5）。

use anyhow::{Context as _, Result};

use crate::commands::file_selection::{FileCandidate, RenameOrigin, resolve, target_paths};
use crate::error::Error;
use crate::finder::{FinderItem, PreviewSource, select_many};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{ChangeScope, FileChange, changes};

/// 未ステージの変更と未追跡ファイルを複数選択し、`git add` でステージする。
///
/// # Errors
///
/// 変更ファイル一覧の取得、選択（中断を含む）、`git add` の実行に失敗した場合にエラーを返す。
pub fn run(repository: &gix::Repository) -> Result<()> {
    let changes = changes(repository, ChangeScope::Stageable)
        .context("変更ファイル一覧の取得に失敗しました")?;

    let mut candidates = Vec::with_capacity(changes.len());
    let mut items = Vec::with_capacity(changes.len());
    for change in &changes {
        // ステージ対象はリネーム後のパスのみ。変更元はインデックス側の情報であり、
        // 作業ツリーの内容をステージするうえでは対象にならない
        let candidate = FileCandidate::from_change(change, RenameOrigin::Exclude);
        items.push(to_item(repository, change, &candidate)?);
        candidates.push(candidate);
    }

    let selected = select_many(items)?;
    let selected = resolve(&candidates, &selected)?;

    let paths = target_paths(&selected);
    let arguments = add_args(&paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).context("git add の実行に失敗しました")?;

    Ok(())
}

/// 候補を finder のアイテムへ変換する。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない（作業ツリーを持たない）場合にエラーを返す。
fn to_item(
    repository: &gix::Repository,
    change: &FileChange,
    candidate: &FileCandidate,
) -> Result<FinderItem> {
    Ok(FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        preview(repository, change)?,
    ))
}

/// プレビュー内容の生成方法を決める。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない場合は [`Error::NoWorktree`] を返す。
fn preview(repository: &gix::Repository, change: &FileChange) -> Result<PreviewSource> {
    if change.is_untracked() {
        // 未追跡ファイルは git の管理下に無く差分を取れないため内容をそのまま表示する。
        // プレビューはカレントディレクトリに依存しないよう絶対パスで指定する
        let path = repository
            .workdir_path(change.path.as_str())
            .ok_or(Error::NoWorktree)?;
        return Ok(PreviewSource::File(path));
    }

    Ok(PreviewSource::Git(preview_args(&change.path)))
}

/// プレビュー用の `git diff` の引数を組み立てる。
///
/// ステージされる差分（インデックス → 作業ツリー）をそのまま表示する。
fn preview_args(path: &str) -> Vec<String> {
    [
        "diff".to_owned(),
        "--color=always".to_owned(),
        "--".to_owned(),
        pathspec(path),
    ]
    .to_vec()
}

/// `git add -- <pathspec>...` の引数を組み立てる。
///
/// パスは必ず `--` の後ろへ置き、オプションとして解釈される余地を排除する。
fn add_args(paths: &[String]) -> Vec<String> {
    ["add".to_owned(), "--".to_owned()]
        .into_iter()
        .chain(paths.iter().map(|path| pathspec(path)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{TempDir, commit, git_in, init_repository, write_file};

    fn change(path: &str, code: &str) -> FileChange {
        let mut codes = code.chars();
        FileChange {
            path: path.to_owned(),
            original_path: None,
            index_status: codes.next().expect("a status code has two characters"),
            worktree_status: codes.next().expect("a status code has two characters"),
        }
    }

    /// 未追跡ファイルを 1 件持つテストリポジトリを用意する。
    fn repository_with_untracked_file(label: &str, path: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        write_file(dir.path(), path, "untracked\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn paths_are_placed_after_the_separator() {
        let paths = ["a.txt".to_owned(), "--not-an-option".to_owned()];

        assert_eq!(
            add_args(&paths),
            [
                "add",
                "--",
                ":(top,literal)a.txt",
                ":(top,literal)--not-an-option"
            ]
        );
    }

    #[test]
    fn a_single_path_produces_three_arguments() {
        assert_eq!(
            add_args(&["dir/with space.txt".to_owned()]),
            ["add", "--", ":(top,literal)dir/with space.txt"]
        );
    }

    #[test]
    fn the_preview_of_a_tracked_file_is_its_unstaged_diff() {
        assert_eq!(
            preview_args("dir/with space.txt"),
            [
                "diff",
                "--color=always",
                "--",
                ":(top,literal)dir/with space.txt"
            ]
        );
    }

    #[test]
    fn a_tracked_change_is_previewed_with_git_diff() {
        let (_dir, repository) = repository_with_untracked_file("add-preview-tracked", "new.txt");

        let preview = preview(&repository, &change("history.txt", " M"))
            .expect("a tracked file needs no work tree lookup");

        assert_eq!(
            preview,
            PreviewSource::Git(preview_args("history.txt")),
            "tracked files must be previewed as a diff"
        );
    }

    #[test]
    fn an_untracked_file_is_previewed_by_its_absolute_path() {
        let (dir, repository) =
            repository_with_untracked_file("add-preview-untracked", "dir/new file.txt");

        let preview = preview(&repository, &change("dir/new file.txt", "??"))
            .expect("the work tree path should resolve");

        match preview {
            PreviewSource::File(path) => {
                assert!(path.is_absolute(), "unexpected path: {path:?}");
                assert_eq!(path, dir.path().join("dir/new file.txt"));
                assert!(path.is_file(), "the previewed file should exist: {path:?}");
            }
            other => panic!("an untracked file must be previewed by content: {other:?}"),
        }
    }

    #[test]
    fn an_item_keeps_the_path_as_its_key_and_shows_the_status_code() {
        let (_dir, repository) = repository_with_untracked_file("add-item", "new.txt");
        let change = change("new.txt", "??");
        let candidate = FileCandidate::from_change(&change, RenameOrigin::Exclude);

        let item = to_item(&repository, &change, &candidate).expect("the item should build");

        assert_eq!(item.key(), "new.txt");
        assert_eq!(candidate.display, "?? new.txt");
    }

    #[test]
    fn the_candidates_cover_unstaged_and_untracked_files_only() {
        let dir = TempDir::new("add-candidates");
        init_repository(dir.path());
        write_file(dir.path(), "tracked.txt", "original\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");
        write_file(dir.path(), "staged.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "staged.txt"]);
        write_file(dir.path(), "tracked.txt", "modified\n");
        write_file(dir.path(), "untracked.txt", "new\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let changes = changes(&repository, ChangeScope::Stageable).expect("status should be read");

        assert_eq!(
            changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["tracked.txt", "untracked.txt"],
            "an already staged file has nothing left to stage"
        );
    }
}
