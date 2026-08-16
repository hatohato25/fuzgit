//! `gz commit` — 変更ファイルを選択してコミットする（FR-9）。
//!
//! `git commit -- <pathspec>...` のパス指定コミットを行うため、選ばなかった変更は
//! ステージ済みであってもコミットされず、ステージ状態のまま残る。

use std::io::Write as _;

use anyhow::{Context as _, Result};

use crate::commands::file_selection::{FileCandidate, RenameOrigin, resolve_changes, target_paths};
use crate::error::Error;
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{ChangeScope, FileChange, changes};
use crate::i18n::{Language, Messages};

/// 失敗を伝える文言に用いる、実行する git のサブコマンド名。
const COMMIT_COMMAND: &str = "git commit";

/// 変更ファイルを複数選択し、選んだファイルの変更だけをコミットする。
///
/// `message` が `None` の場合はコミットメッセージの入力を git に委ねる
/// （継承 stdio で `git commit` を実行するため、git がユーザーのエディタを起動する）。
///
/// # Errors
///
/// 変更ファイル一覧の取得、選択（中断を含む）、`git add` / `git commit` の実行に失敗した場合に
/// エラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    message: Option<&str>,
) -> Result<()> {
    let changes = changes(repository, ChangeScope::TrackedOrUntracked)
        .context(messages.common().changed_files_read_failed())?;

    let mut items = Vec::with_capacity(changes.len());
    for change in &changes {
        let candidate = to_candidate(change);
        items.push(to_item(language, repository, change, &candidate)?);
    }

    let options = FinderOptions::new(SelectionMode::Multi)
        .with_header(messages.commit().header().to_owned())
        .with_preselect(preselected(&changes));
    let selected = select_many_with(items, &options)?;
    let selected = resolve_changes(messages, &changes, &selected)?;

    run_on_changes(language, messages, message, &selected)
}

/// 選択済みの変更ファイルの内容だけをコミットする。
///
/// `gz status` のアクションメニュー（FR-16）からも呼ばれる。未追跡ファイルの事前ステージと
/// 継承 stdio での実行（エディタ起動を git に委ねる）はこの関数の中に閉じており、
/// 呼び出し経路によらず同じ挙動になる。
///
/// # Errors
///
/// `git add` / `git commit` の実行に失敗した場合にエラーを返す。
pub fn run_on_changes(
    language: Language,
    messages: &dyn Messages,
    message: Option<&str>,
    selected: &[&FileChange],
) -> Result<()> {
    // 未追跡ファイルはパス指定コミットの対象にできず
    // 「did not match any file(s) known to git」で失敗するため、先にステージする
    let untracked = untracked_paths(selected);
    if !untracked.is_empty() {
        let arguments = add_args(&untracked);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        run_git(language, &arguments).context(messages.commit().untracked_stage_failed())?;
    }

    let candidates: Vec<FileCandidate> =
        selected.iter().map(|change| to_candidate(change)).collect();
    let paths = target_paths(&candidates.iter().collect::<Vec<&FileCandidate>>());

    let arguments = commit_args(message, &paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    if let Err(error) = run_git(language, &arguments) {
        if let Some(hint) = failure_hint(messages, message) {
            // 案内はあくまで補助であり、その書き込み失敗でコミット失敗そのものを
            // 置き換えてしまわないよう、ここだけは結果を破棄する
            let _ = writeln!(std::io::stderr(), "{hint}");
        }
        return Err(error).context(messages.common().command_run_failed(COMMIT_COMMAND));
    }

    Ok(())
}

/// コミットが失敗したときに添える案内を選ぶ。
///
/// `-m` でメッセージを指定した場合はエディタを起動しないため、エディタ設定の案内は
/// 的外れになる。エディタに委ねた場合だけ
/// [`crate::i18n::messages::CommitMessages::editor_hint`] を返す。
fn failure_hint(messages: &dyn Messages, message: Option<&str>) -> Option<&'static str> {
    match message {
        Some(_) => None,
        None => Some(messages.commit().editor_hint()),
    }
}

/// 変更ファイルを候補へ変換する。
///
/// リネーム・コピーは変更元のパスも対象に含める。変更後のパスだけを指定してコミットすると
/// 追加だけが記録され、変更元の削除（`D  <元のパス>`）がステージに残ってしまうため
/// （実機で確認済み）。
fn to_candidate(change: &FileChange) -> FileCandidate {
    FileCandidate::from_change(change, RenameOrigin::Include)
}

/// 起動時に選択済みにする候補の表示文字列を集める。
///
/// ステージ済みの変更を持つファイルが対象。skim の事前選択は表示文字列の完全一致で
/// 判定されるため、キー（パス）ではなく [`FileCandidate::display`] を渡す。
fn preselected(changes: &[FileChange]) -> Vec<String> {
    changes
        .iter()
        .filter(|change| change.has_staged_change())
        .map(|change| to_candidate(change).display)
        .collect()
}

/// 選択された変更のうち、未追跡ファイルのパスを選択一覧の順序で集める。
fn untracked_paths(selected: &[&FileChange]) -> Vec<String> {
    selected
        .iter()
        .filter(|change| change.is_untracked())
        .map(|change| change.path.clone())
        .collect()
}

/// 候補を finder のアイテムへ変換する。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない（作業ツリーを持たない）場合にエラーを返す。
fn to_item(
    language: Language,
    repository: &gix::Repository,
    change: &FileChange,
    candidate: &FileCandidate,
) -> Result<FinderItem> {
    Ok(FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        preview(repository, change, candidate)?,
        language.messages(),
    )
    .with_highlights(candidate.highlights.clone()))
}

/// プレビュー内容の生成方法を決める。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない場合は [`Error::NoWorktree`] を返す。
fn preview(
    repository: &gix::Repository,
    change: &FileChange,
    candidate: &FileCandidate,
) -> Result<PreviewSource> {
    if change.is_untracked() {
        // 未追跡ファイルは git の管理下に無く差分を取れないため内容をそのまま表示する。
        // プレビューはカレントディレクトリに依存しないよう絶対パスで指定する
        let path = repository
            .workdir_path(change.path.as_str())
            .ok_or(Error::NoWorktree)?;
        return Ok(PreviewSource::File(path));
    }

    Ok(PreviewSource::Git(preview_args(&candidate.paths)))
}

/// プレビュー用の `git diff HEAD` の引数を組み立てる。
///
/// パス指定コミットが記録するのは作業ツリーの内容であり、index に何がステージされているかは
/// 結果に影響しない（ステージ済みと作業ツリーが食い違う `MM` のファイルでも作業ツリー側が
/// コミットされることを実機で確認済み）。コミットされる内容と一致させるため、
/// index との差分ではなく HEAD との差分を表示する。
fn preview_args(paths: &[String]) -> Vec<String> {
    ["diff", "--color=always", "HEAD", "--"]
        .into_iter()
        .map(str::to_owned)
        .chain(paths.iter().map(|path| pathspec(path)))
        .collect()
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

/// `git commit [--message <message>] -- <pathspec>...` の引数を組み立てる。
///
/// `--message` を付けない場合、継承 stdio で実行する git がエディタを起動してメッセージを尋ねる。
/// パスは必ず `--` の後ろへ置き、オプションとして解釈される余地を排除する。
fn commit_args(message: Option<&str>, paths: &[String]) -> Vec<String> {
    let mut args = vec!["commit".to_owned()];
    if let Some(message) = message {
        args.push("--message".to_owned());
        args.push(message.to_owned());
    }
    args.push("--".to_owned());
    args.extend(paths.iter().map(|path| pathspec(path)));
    args
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

    fn rename(path: &str, original: &str, code: &str) -> FileChange {
        FileChange {
            original_path: Some(original.to_owned()),
            ..change(path, code)
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
    fn a_commit_without_a_message_leaves_the_editor_to_git() {
        assert_eq!(
            commit_args(None, &paths(&["a.txt"])),
            ["commit", "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn a_message_is_passed_as_an_option_value() {
        assert_eq!(
            commit_args(Some("認証を修正"), &paths(&["a.txt"])),
            [
                "commit",
                "--message",
                "認証を修正",
                "--",
                ":(top,literal)a.txt"
            ]
        );
    }

    #[test]
    fn every_selected_path_is_passed_after_the_separator() {
        let arguments = commit_args(
            Some("--not-an-option"),
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
    fn a_failed_commit_without_a_message_explains_the_editor() {
        let hint = failure_hint(Language::Japanese.messages(), None)
            .expect("the editor case needs guidance");

        assert!(
            hint.contains("gz commit -m"),
            "the message option should be offered: {hint}"
        );
        assert!(
            hint.contains("code --wait"),
            "an editor that returns immediately should be named: {hint}"
        );
    }

    #[test]
    fn the_editor_guidance_does_not_assert_a_single_cause() {
        // 失敗理由は空メッセージだけではない（マージ中の partial commit 拒否・フック等）
        let hint = failure_hint(Language::Japanese.messages(), None)
            .expect("the editor case needs guidance");

        assert!(
            hint.contains("保存されなかった場合"),
            "the guidance must stay conditional: {hint}"
        );
    }

    #[test]
    fn a_failed_commit_with_a_message_says_nothing_about_the_editor() {
        assert_eq!(
            failure_hint(Language::Japanese.messages(), Some("認証を修正")),
            None,
            "-m does not start an editor, so the editor guidance would mislead"
        );
    }

    #[test]
    fn untracked_files_are_staged_before_the_commit() {
        assert_eq!(
            add_args(&paths(&["new.txt", "dir/a[1].txt"])),
            [
                "add",
                "--",
                ":(top,literal)new.txt",
                ":(top,literal)dir/a[1].txt"
            ]
        );
    }

    #[test]
    fn staged_files_are_preselected() {
        let changes = [
            change("staged.txt", "M "),
            change("unstaged.txt", " M"),
            change("both.txt", "MM"),
            change("new.txt", "??"),
        ];

        assert_eq!(
            preselected(&changes),
            ["M  staged.txt", "MM both.txt"],
            "only files with staged changes start out selected"
        );
    }

    #[test]
    fn a_preselected_entry_is_the_display_string_and_not_the_path() {
        // skim の事前選択は表示文字列の完全一致で判定されるため、パスだけでは選択されない
        let changes = [rename("new.txt", "old.txt", "R ")];

        assert_eq!(preselected(&changes), ["R  old.txt -> new.txt"]);
    }

    #[test]
    fn nothing_is_preselected_without_staged_changes() {
        let changes = [change("unstaged.txt", " M"), change("new.txt", "??")];

        assert!(
            preselected(&changes).is_empty(),
            "an unstaged-only work tree starts with an empty selection"
        );
    }

    #[test]
    fn only_the_selected_untracked_files_are_staged() {
        let changes = [
            change("tracked.txt", " M"),
            change("new.txt", "??"),
            change("other new.txt", "??"),
        ];
        let selected = resolve_changes(
            Language::Japanese.messages(),
            &changes,
            &paths(&["other new.txt", "tracked.txt"]),
        )
        .expect("all paths are listed");

        assert_eq!(
            untracked_paths(&selected),
            ["other new.txt"],
            "a tracked change needs no staging, an unselected untracked file is left alone"
        );
    }

    #[test]
    fn a_selection_without_untracked_files_needs_no_staging() {
        let changes = [change("tracked.txt", "M "), change("new.txt", "??")];
        let selected = resolve_changes(
            Language::Japanese.messages(),
            &changes,
            &paths(&["tracked.txt"]),
        )
        .expect("all paths are listed");

        assert!(
            untracked_paths(&selected).is_empty(),
            "git add must not run when no untracked file was selected"
        );
    }

    #[test]
    fn a_rename_is_committed_with_both_of_its_paths() {
        // 変更後のパスだけを渡すと追加だけが記録され、変更元の削除がステージに残る
        let candidate = to_candidate(&rename("new.txt", "old.txt", "R "));

        assert_eq!(candidate.key, "new.txt");
        assert_eq!(candidate.paths, ["new.txt", "old.txt"]);
    }

    #[test]
    fn the_preview_shows_the_difference_against_head() {
        assert_eq!(
            preview_args(&paths(&["dir/with space.txt"])),
            [
                "diff",
                "--color=always",
                "HEAD",
                "--",
                ":(top,literal)dir/with space.txt"
            ],
            "a path commit records the work tree, so the diff is against HEAD"
        );
    }

    #[test]
    fn the_preview_of_a_rename_covers_both_of_its_paths() {
        assert_eq!(
            preview_args(&paths(&["new.txt", "old.txt"])),
            [
                "diff",
                "--color=always",
                "HEAD",
                "--",
                ":(top,literal)new.txt",
                ":(top,literal)old.txt"
            ]
        );
    }

    #[test]
    fn a_tracked_change_is_previewed_with_git_diff() {
        let (_dir, repository) =
            repository_with_untracked_file("commit-preview-tracked", "new.txt");
        let change = change("history.txt", "M ");
        let candidate = to_candidate(&change);

        let preview = preview(&repository, &change, &candidate)
            .expect("a tracked file needs no work tree lookup");

        assert_eq!(preview, PreviewSource::Git(preview_args(&candidate.paths)));
    }

    #[test]
    fn an_untracked_file_is_previewed_by_its_absolute_path() {
        let (dir, repository) =
            repository_with_untracked_file("commit-preview-untracked", "dir/new file.txt");
        let change = change("dir/new file.txt", "??");
        let candidate = to_candidate(&change);

        let preview =
            preview(&repository, &change, &candidate).expect("the work tree path should resolve");

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
        let (_dir, repository) = repository_with_untracked_file("commit-item", "new.txt");
        let change = change("new.txt", "??");
        let candidate = to_candidate(&change);

        let item = to_item(Language::Japanese, &repository, &change, &candidate)
            .expect("the item should build");

        assert_eq!(item.key(), "new.txt");
        assert_eq!(candidate.display, "?? new.txt");
    }

    #[test]
    fn every_commit_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let commit = language.messages().commit();

            for text in [
                commit.header(),
                commit.editor_hint(),
                commit.untracked_stage_failed(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            // 端末が送るキーの名前であるため、どの言語でも同じ綴りで現れる
            assert!(
                commit.header().contains("Tab") && commit.header().contains("Enter"),
                "{language:?} must name both keys: {text}",
                text = commit.header()
            );
            // ユーザーがそのまま打ち込む・設定するものであるため、どの言語でも同じ綴りで現れる
            assert!(
                commit.editor_hint().contains("gz commit -m")
                    && commit.editor_hint().contains("code --wait")
                    && commit.editor_hint().contains("EDITOR / GIT_EDITOR"),
                "{language:?} must spell out the command and the variables: {text}",
                text = commit.editor_hint()
            );
            // 実行した git のサブコマンド名は訳さない
            assert!(
                commit.untracked_stage_failed().contains("git add"),
                "{language:?} must name the command it ran: {text}",
                text = commit.untracked_stage_failed()
            );
        }
    }

    #[test]
    fn the_commit_wording_is_translated() {
        let japanese = Language::Japanese.messages().commit();
        let english = Language::English.messages().commit();

        assert_ne!(japanese.header(), english.header());
        assert_ne!(japanese.editor_hint(), english.editor_hint());
        assert_ne!(
            japanese.untracked_stage_failed(),
            english.untracked_stage_failed()
        );
    }

    #[test]
    fn the_english_editor_guidance_does_not_assert_a_single_cause_either() {
        // 失敗理由は空メッセージだけではない（マージ中の partial commit 拒否・フック等）
        let hint = failure_hint(Language::English.messages(), None)
            .expect("the editor case needs guidance");

        assert!(
            hint.contains("if the commit message is not saved"),
            "the guidance must stay conditional: {hint}"
        );
    }

    #[test]
    fn staged_unstaged_and_untracked_changes_are_offered_in_one_list() {
        let dir = TempDir::new("commit-candidates");
        init_repository(dir.path());
        write_file(dir.path(), "tracked.txt", "original\n");
        write_file(dir.path(), "staged-only.txt", "original\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");
        write_file(dir.path(), "staged-only.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "staged-only.txt"]);
        write_file(dir.path(), "tracked.txt", "modified\n");
        write_file(dir.path(), "untracked.txt", "new\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let changes =
            changes(&repository, ChangeScope::TrackedOrUntracked).expect("status should be read");

        assert_eq!(
            changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["staged-only.txt", "tracked.txt", "untracked.txt"]
        );
        assert_eq!(preselected(&changes), ["M  staged-only.txt"]);
    }
}
