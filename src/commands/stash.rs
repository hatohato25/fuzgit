//! `gz stash` — 変更ファイルを stash へ退避し、既存の stash を適用・破棄する（FR-6）。
//!
//! `push` が選ぶのは作業ツリーの「ファイル」、`apply` / `pop` / `drop` が選ぶのは既存の「stash」で、
//! 選択対象が異なるため入口を分けている（[`push`] と [`run`]）。

use anyhow::{Context as _, Result, anyhow};

use crate::commands::confirmation::confirm;
use crate::commands::file_selection::{FileCandidate, RenameOrigin, resolve, target_paths};
use crate::error::Error;
use crate::finder::{FinderItem, PreviewSource, select_many, select_one};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{ChangeScope, FileChange, StashEntry, changes, stashes};

/// `drop` の確認プロンプトの見出し。
///
/// 破棄した stash は元に戻せないため、対象を示したうえで同意を求める。
const DROP_CONFIRMATION_HEADER: &str = "以下の stash を破棄します（元に戻せません）:";

/// `git stash push` に未追跡ファイルも含めさせるオプション（`-u` の長い綴り）。
const INCLUDE_UNTRACKED_OPTION: &str = "--include-untracked";

/// 選択した stash に対して行う操作。
///
/// `apply` / `pop` / `drop` のどれとして実行するかを型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashAction {
    /// stash を作業ツリーへ適用し、stash も残す（`gz stash apply`）。
    Apply,
    /// stash を作業ツリーへ適用し、成功したら stash を取り除く（`gz stash pop`）。
    Pop,
    /// stash を適用せずに破棄する（`gz stash drop`）。元に戻せないため実行前に確認する。
    Drop,
}

impl StashAction {
    /// `git stash` に続けるサブコマンド名。
    fn subcommand(self) -> &'static str {
        match self {
            StashAction::Apply => "apply",
            StashAction::Pop => "pop",
            StashAction::Drop => "drop",
        }
    }

    /// 実行前に確認を求める操作かどうか。
    fn needs_confirmation(self) -> bool {
        // apply / pop は作業ツリーへ復元する操作であり、失われるものがない。
        // drop は復元せずに捨てるため確認する
        matches!(self, StashAction::Drop)
    }
}

/// `gz stash push` で未追跡ファイルを対象に含めるかどうか。
///
/// 候補の範囲と `git stash push` へ渡すオプションの両方がこれで決まる。真偽値を持ち回すと
/// 片方だけ反映して「候補には出るが git 側が対象にできない」不整合を招くため、型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UntrackedFiles {
    /// 追跡済みファイルの変更のみを対象にする（既定）。
    Exclude,
    /// 未追跡ファイルも対象に含める（`-u` / `--include-untracked`）。
    Include,
}

impl UntrackedFiles {
    /// 候補として並べる変更の範囲。
    fn change_scope(self) -> ChangeScope {
        match self {
            UntrackedFiles::Exclude => ChangeScope::Tracked,
            UntrackedFiles::Include => ChangeScope::TrackedOrUntracked,
        }
    }

    /// `git stash push` に付けるオプション。
    fn option(self) -> Option<&'static str> {
        match self {
            UntrackedFiles::Exclude => None,
            UntrackedFiles::Include => Some(INCLUDE_UNTRACKED_OPTION),
        }
    }
}

/// 変更ファイルを複数選択し、選んだものだけを `git stash push` で退避する。
///
/// 選ばなかった変更は作業ツリーに残る。`untracked` が [`UntrackedFiles::Exclude`] の場合、
/// 未追跡ファイルは候補にも `git stash push` の対象にもならない。
///
/// # Errors
///
/// 変更ファイル一覧の取得、選択（中断を含む）、`git stash push` の実行に失敗した場合に
/// エラーを返す。
pub fn push(
    repository: &gix::Repository,
    message: Option<&str>,
    untracked: UntrackedFiles,
) -> Result<()> {
    let changes = changes(repository, untracked.change_scope())
        .context("変更ファイル一覧の取得に失敗しました")?;

    let mut candidates = Vec::with_capacity(changes.len());
    let mut items = Vec::with_capacity(changes.len());
    for change in &changes {
        // リネームの変更元は index に存在せず、パススペックとして渡すと
        // 「did not match any file(s) known to git」になるため対象にしない
        let candidate = FileCandidate::from_change(change, RenameOrigin::Exclude);
        items.push(to_file_item(repository, change, &candidate)?);
        candidates.push(candidate);
    }

    let selected = select_many(items)?;
    let selected = resolve(&candidates, &selected)?;

    let paths = target_paths(&selected);
    let arguments = push_args(message, untracked, &paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).context("git stash push の実行に失敗しました")?;

    Ok(())
}

/// stash を 1 件選び、`git stash apply|pop|drop` を実行する。
///
/// # Errors
///
/// stash 一覧の取得、選択（中断を含む）、確認の否認、`git stash` の実行に失敗した場合に
/// エラーを返す。
pub fn run(repository: &gix::Repository, action: StashAction) -> Result<()> {
    let candidates = stashes(repository).context("stash 一覧の取得に失敗しました")?;

    let items = candidates.iter().map(to_stash_item).collect();
    let selected = select_one(items)?;

    let entry = candidates
        .iter()
        .find(|candidate| candidate.selector() == selected)
        .ok_or_else(|| anyhow!("選択された stash `{selected}` が候補に見つかりません"))?;

    if action.needs_confirmation() {
        confirm(DROP_CONFIRMATION_HEADER, &[&display_line(entry)])?;
    }

    let selector = entry.selector();
    run_git(&["stash", action.subcommand(), &selector]).with_context(|| {
        format!(
            "git stash {subcommand} {selector} の実行に失敗しました",
            subcommand = action.subcommand()
        )
    })?;

    Ok(())
}

/// `git stash push [--message <message>] [--include-untracked] -- <pathspec>...` の引数を組み立てる。
///
/// パスは必ず `--` の後ろへ置き、オプションとして解釈される余地を排除する。
fn push_args(message: Option<&str>, untracked: UntrackedFiles, paths: &[String]) -> Vec<String> {
    let mut args = vec!["stash".to_owned(), "push".to_owned()];
    if let Some(message) = message {
        args.push("--message".to_owned());
        args.push(message.to_owned());
    }
    if let Some(option) = untracked.option() {
        args.push(option.to_owned());
    }
    args.push("--".to_owned());
    args.extend(paths.iter().map(|path| pathspec(path)));
    args
}

/// 変更ファイルの候補を finder のアイテムへ変換する。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない（作業ツリーを持たない）場合にエラーを返す。
fn to_file_item(
    repository: &gix::Repository,
    change: &FileChange,
    candidate: &FileCandidate,
) -> Result<FinderItem> {
    Ok(FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        file_preview(repository, change)?,
    ))
}

/// 変更ファイルのプレビュー内容の生成方法を決める。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない場合は [`Error::NoWorktree`] を返す。
fn file_preview(repository: &gix::Repository, change: &FileChange) -> Result<PreviewSource> {
    if change.is_untracked() {
        // 未追跡ファイルは git の管理下に無く差分を取れないため内容をそのまま表示する。
        // プレビューはカレントディレクトリに依存しないよう絶対パスで指定する
        let path = repository
            .workdir_path(change.path.as_str())
            .ok_or(Error::NoWorktree)?;
        return Ok(PreviewSource::File(path));
    }

    Ok(PreviewSource::Git(file_preview_args(&change.path)))
}

/// プレビュー用の `git diff HEAD` の引数を組み立てる。
///
/// stash はステージ済み・未ステージのどちらの変更も退避するため、index との差分（`git diff`）ではなく
/// HEAD との差分を表示する。これで実際に退避される内容と一致する。
fn file_preview_args(path: &str) -> Vec<String> {
    [
        "diff".to_owned(),
        "--color=always".to_owned(),
        "HEAD".to_owned(),
        "--".to_owned(),
        pathspec(path),
    ]
    .to_vec()
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// `git stash list` と同じ `stash@{n}: <メッセージ>` 形式で表示する。
fn display_line(entry: &StashEntry) -> String {
    format!(
        "{selector}: {message}",
        selector = entry.selector(),
        message = entry.message
    )
}

/// プレビュー用の `git stash show` の引数を組み立てる。
fn stash_preview_args(entry: &StashEntry) -> Vec<String> {
    [
        "stash".to_owned(),
        "show".to_owned(),
        "-p".to_owned(),
        "--color=always".to_owned(),
        entry.selector(),
    ]
    .to_vec()
}

/// stash を finder の候補へ変換する。
fn to_stash_item(entry: &StashEntry) -> FinderItem {
    FinderItem::new(
        display_line(entry),
        entry.selector(),
        PreviewSource::Git(stash_preview_args(entry)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{TempDir, commit, git_in, init_repository, write_file};

    fn entry(index: usize, message: &str) -> StashEntry {
        StashEntry {
            index,
            message: message.to_owned(),
        }
    }

    fn change(path: &str, code: &str) -> FileChange {
        let mut codes = code.chars();
        FileChange {
            path: path.to_owned(),
            original_path: None,
            index_status: codes.next().expect("a status code has two characters"),
            worktree_status: codes.next().expect("a status code has two characters"),
        }
    }

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
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
    fn each_action_maps_to_its_git_subcommand() {
        assert_eq!(StashAction::Apply.subcommand(), "apply");
        assert_eq!(StashAction::Pop.subcommand(), "pop");
        assert_eq!(StashAction::Drop.subcommand(), "drop");
    }

    #[test]
    fn only_dropping_asks_for_confirmation() {
        assert!(StashAction::Drop.needs_confirmation());
        assert!(!StashAction::Apply.needs_confirmation());
        assert!(!StashAction::Pop.needs_confirmation());
    }

    #[test]
    fn a_line_shows_the_selector_before_the_message() {
        assert_eq!(
            display_line(&entry(2, "On main: 認証: バグ修正")),
            "stash@{2}: On main: 認証: バグ修正"
        );
    }

    #[test]
    fn the_preview_shows_the_diff_of_that_stash() {
        assert_eq!(
            stash_preview_args(&entry(1, "WIP on main: 5d21a8c first")),
            ["stash", "show", "-p", "--color=always", "stash@{1}"]
        );
    }

    #[test]
    fn an_item_keeps_the_selector_as_its_key() {
        let item = to_stash_item(&entry(10, "On main: 作業中"));

        assert_eq!(item.key(), "stash@{10}");
    }

    #[test]
    fn pushing_without_options_only_lists_the_paths() {
        assert_eq!(
            push_args(None, UntrackedFiles::Exclude, &paths(&["a.txt"])),
            ["stash", "push", "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn a_message_is_passed_as_an_option_value() {
        assert_eq!(
            push_args(Some("作業中"), UntrackedFiles::Exclude, &paths(&["a.txt"])),
            [
                "stash",
                "push",
                "--message",
                "作業中",
                "--",
                ":(top,literal)a.txt"
            ]
        );
    }

    #[test]
    fn including_untracked_files_adds_its_option() {
        assert_eq!(
            push_args(None, UntrackedFiles::Include, &paths(&["new.txt"])),
            [
                "stash",
                "push",
                INCLUDE_UNTRACKED_OPTION,
                "--",
                ":(top,literal)new.txt"
            ]
        );
    }

    #[test]
    fn a_message_and_the_untracked_option_can_be_combined() {
        assert_eq!(
            push_args(Some("wip"), UntrackedFiles::Include, &paths(&["a.txt"])),
            [
                "stash",
                "push",
                "--message",
                "wip",
                INCLUDE_UNTRACKED_OPTION,
                "--",
                ":(top,literal)a.txt"
            ]
        );
    }

    #[test]
    fn every_selected_path_is_passed_after_the_separator() {
        let arguments = push_args(
            Some("--not-an-option"),
            UntrackedFiles::Include,
            &paths(&["dir/with space.txt", "dir/a[1].txt", "--not-an-option"]),
        );

        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .expect("the separator is always present");
        assert_eq!(
            arguments[separator + 1..],
            [
                ":(top,literal)dir/with space.txt",
                ":(top,literal)dir/a[1].txt",
                ":(top,literal)--not-an-option"
            ],
            "a path must never be interpreted as an option"
        );
    }

    #[test]
    fn the_candidate_scope_matches_the_option_passed_to_git() {
        // 候補に未追跡ファイルを出しながら `--include-untracked` を渡さないと、
        // git 側が対象にできず「pathspec did not match」で失敗する
        assert_eq!(UntrackedFiles::Exclude.change_scope(), ChangeScope::Tracked);
        assert_eq!(UntrackedFiles::Exclude.option(), None);

        assert_eq!(
            UntrackedFiles::Include.change_scope(),
            ChangeScope::TrackedOrUntracked
        );
        assert_eq!(
            UntrackedFiles::Include.option(),
            Some(INCLUDE_UNTRACKED_OPTION)
        );
    }

    #[test]
    fn the_preview_of_a_tracked_file_covers_the_staged_changes_as_well() {
        assert_eq!(
            file_preview_args("dir/with space.txt"),
            [
                "diff",
                "--color=always",
                "HEAD",
                "--",
                ":(top,literal)dir/with space.txt"
            ],
            "a stash also takes what is already staged, so the diff is against HEAD"
        );
    }

    #[test]
    fn a_tracked_change_is_previewed_with_git_diff() {
        let (_dir, repository) = repository_with_untracked_file("stash-preview-tracked", "new.txt");

        let preview = file_preview(&repository, &change("history.txt", "M "))
            .expect("a tracked file needs no work tree lookup");

        assert_eq!(
            preview,
            PreviewSource::Git(file_preview_args("history.txt"))
        );
    }

    #[test]
    fn an_untracked_file_is_previewed_by_its_absolute_path() {
        let (dir, repository) =
            repository_with_untracked_file("stash-preview-untracked", "dir/new file.txt");

        let preview = file_preview(&repository, &change("dir/new file.txt", "??"))
            .expect("the work tree path should resolve");

        match preview {
            PreviewSource::File(path) => {
                assert_eq!(path, dir.path().join("dir/new file.txt"));
                assert!(path.is_file(), "the previewed file should exist: {path:?}");
            }
            other => panic!("an untracked file must be previewed by content: {other:?}"),
        }
    }

    #[test]
    fn an_item_keeps_the_path_as_its_key_and_shows_the_status_code() {
        let (_dir, repository) = repository_with_untracked_file("stash-push-item", "new.txt");
        let change = change("new.txt", "??");
        let candidate = FileCandidate::from_change(&change, RenameOrigin::Exclude);

        let item = to_file_item(&repository, &change, &candidate).expect("the item should build");

        assert_eq!(item.key(), "new.txt");
        assert_eq!(candidate.display, "?? new.txt");
    }

    #[test]
    fn a_staged_only_change_is_offered_as_a_candidate() {
        let dir = TempDir::new("stash-push-candidates");
        init_repository(dir.path());
        write_file(dir.path(), "tracked.txt", "original\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");
        write_file(dir.path(), "tracked.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "tracked.txt"]);
        write_file(dir.path(), "untracked.txt", "new\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let tracked = changes(&repository, UntrackedFiles::Exclude.change_scope())
            .expect("status should be read");
        assert_eq!(
            tracked
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["tracked.txt"],
            "a staged change is stashed too, an untracked file is not"
        );

        let with_untracked = changes(&repository, UntrackedFiles::Include.change_scope())
            .expect("status should be read");
        assert_eq!(
            with_untracked
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["tracked.txt", "untracked.txt"]
        );
    }
}
