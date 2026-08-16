//! merge / rebase 進行中の状態確認・復帰メニュー（FR-14）。
//!
//! `gz merge` / `gz rebase` が進行中状態（`git::read::operation_in_progress`）を
//! 検出したときに委譲される、両コマンド共用のフロー。
//!
//! コンフリクトファイルを「エディタで開く」操作は提供しない
//! （requirements.md「スコープ外」。解決は各自のエディタで行い、fuzgit は
//! 差分の確認・stage・continue / skip / abort に責務を絞る）。

use anyhow::{Context as _, Result, anyhow, bail};

use crate::commands::confirmation::confirm;
use crate::commands::file_selection::{
    FileCandidate, RenameOrigin, resolve as resolve_files, target_paths,
};
use crate::commands::{command_display, status_preview_args};
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with, select_one,
};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{ChangeScope, FileChange, Operation, changes};
use crate::i18n::{Language, Messages};

/// 「コンフリクトファイルの確認」項目の finder キー。
const CONFLICTS_KEY: &str = "conflicts";

/// 「continue」項目の finder キー。
const CONTINUE_KEY: &str = "continue";

/// 「skip」項目の finder キー（rebase のみ）。
const SKIP_KEY: &str = "skip";

/// 「abort」項目の finder キー。
const ABORT_KEY: &str = "abort";

/// `git merge --continue` の引数。
const MERGE_CONTINUE_ARGS: [&str; 2] = ["merge", "--continue"];

/// `git merge --abort` の引数。
const MERGE_ABORT_ARGS: [&str; 2] = ["merge", "--abort"];

/// `git rebase --continue` の引数。
const REBASE_CONTINUE_ARGS: [&str; 2] = ["rebase", "--continue"];

/// `git rebase --skip` の引数。
const REBASE_SKIP_ARGS: [&str; 2] = ["rebase", "--skip"];

/// `git rebase --abort` の引数。
const REBASE_ABORT_ARGS: [&str; 2] = ["rebase", "--abort"];

/// 復帰メニューで選べる操作。
///
/// continue / skip / abort は「どの git コマンドを実行するか」だけが異なるため、
/// 実行する引数を項目の生成時に確定させて持たせる。これにより、rebase にしか無い
/// `--skip` を merge のメニューへ紛れ込ませることが構造的にできない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    /// コンフリクト中のファイルを確認し、選んだものを解決済みとして stage する。
    ShowConflicts,
    /// 確認なしで継承 stdio 実行する（continue / skip）。
    ///
    /// どちらも解決作業を失わせない操作であり、メッセージ入力のエディタ起動は git に委ねる。
    Proceed(&'static [&'static str]),
    /// 確認プロンプトを挟んでから継承 stdio 実行する（abort）。
    Abort(&'static [&'static str]),
}

/// 復帰メニューの 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuEntry {
    /// finder のキー。選択結果の照合はこのキーで行い、表示文字列では行わない。
    key: &'static str,
    /// 一覧表示および絞り込み対象の文字列。
    display: String,
    /// 決定時に行う操作。
    action: MenuAction,
}

/// 進行中の操作の復帰メニューを表示し、選ばれた操作を実行する。
///
/// `operation` は呼び出し側（`gz merge` / `gz rebase`）が
/// [`crate::git::read::operation_in_progress`] で判定した結果を渡す。
///
/// # Errors
///
/// 選択（中断を含む）、コンフリクトファイル一覧の取得、`git add` および
/// `git merge` / `git rebase` の実行に失敗した場合にエラーを返す。
/// abort の確認プロンプトで承認が得られなかった場合は [`crate::error::Error::Cancelled`]。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    operation: Operation,
) -> Result<()> {
    let entries = menu(messages, operation);
    let items = entries
        .iter()
        .map(|entry| to_item(language, entry))
        .collect();

    let selected = select_one(items)?;
    let entry = resolve_entry(messages, &entries, &selected)?;

    match entry.action {
        MenuAction::ShowConflicts => stage_resolved(language, messages, repository),
        MenuAction::Proceed(args) => execute(language, messages, args),
        MenuAction::Abort(args) => abort(language, messages, operation, args),
    }
}

/// 進行中の操作の呼称。メニューの表示と確認プロンプトで用いる。
fn operation_name(operation: Operation) -> &'static str {
    match operation {
        Operation::Merge => "merge",
        Operation::Rebase => "rebase",
    }
}

/// 進行中の操作に応じたメニュー項目を組み立てる。
///
/// merge には `--skip` が無い（merge は 1 度の操作であり「飛ばす対象のコミット」を持たない）ため、
/// 項目の有無を真偽値で切り替えず、操作ごとに項目の並びそのものを列挙する。
fn menu(messages: &dyn Messages, operation: Operation) -> Vec<MenuEntry> {
    let name = operation_name(operation);
    let in_progress = messages.in_progress();

    match operation {
        Operation::Merge => vec![
            entry(
                CONFLICTS_KEY,
                in_progress.conflicts_action().to_owned(),
                MenuAction::ShowConflicts,
            ),
            entry(
                CONTINUE_KEY,
                in_progress.continue_action(name),
                MenuAction::Proceed(&MERGE_CONTINUE_ARGS),
            ),
            entry(
                ABORT_KEY,
                in_progress.abort_action(name),
                MenuAction::Abort(&MERGE_ABORT_ARGS),
            ),
        ],
        Operation::Rebase => vec![
            entry(
                CONFLICTS_KEY,
                in_progress.conflicts_action().to_owned(),
                MenuAction::ShowConflicts,
            ),
            entry(
                CONTINUE_KEY,
                in_progress.continue_action(name),
                MenuAction::Proceed(&REBASE_CONTINUE_ARGS),
            ),
            entry(
                SKIP_KEY,
                in_progress.skip_action().to_owned(),
                MenuAction::Proceed(&REBASE_SKIP_ARGS),
            ),
            entry(
                ABORT_KEY,
                in_progress.abort_action(name),
                MenuAction::Abort(&REBASE_ABORT_ARGS),
            ),
        ],
    }
}

/// メニュー項目 1 件を組み立てる。
///
/// 表示に添えるコマンド名は実行する引数そのものから作り、説明と実際の操作が
/// 食い違わないようにする。
fn entry(key: &'static str, label: String, action: MenuAction) -> MenuEntry {
    let display = match action {
        MenuAction::ShowConflicts => label,
        MenuAction::Proceed(args) | MenuAction::Abort(args) => {
            format!("{label} ({command})", command = command_display(args))
        }
    };

    MenuEntry {
        key,
        display,
        action,
    }
}

/// メニュー項目を finder のアイテムへ変換する。
fn to_item(language: Language, entry: &MenuEntry) -> FinderItem {
    FinderItem::new(
        entry.display.clone(),
        entry.key.to_owned(),
        PreviewSource::Git(status_preview_args()),
        language.messages(),
    )
}

/// 選択されたキーに対応するメニュー項目を返す。
///
/// # Errors
///
/// 選択されたキーがメニューに含まれない場合にエラーを返す（対象を取り違えたまま
/// git 操作を実行しないよう、暗黙に読み飛ばさない）。
fn resolve_entry<'a>(
    messages: &dyn Messages,
    entries: &'a [MenuEntry],
    selected: &str,
) -> Result<&'a MenuEntry> {
    entries
        .iter()
        .find(|entry| entry.key == selected)
        .ok_or_else(|| anyhow!(messages.in_progress().menu_selection_not_found(selected)))
}

/// continue / skip を継承 stdio で実行する。
///
/// コミットメッセージの入力が必要な場合のエディタ起動は git に委ねる。
fn execute(language: Language, messages: &dyn Messages, args: &[&str]) -> Result<()> {
    run_git(language, args)
        .with_context(|| messages.common().command_run_failed(&command_display(args)))
}

/// 確認を取ってから abort を実行する。
///
/// # Errors
///
/// 承認が得られなかった場合は [`crate::error::Error::Cancelled`]、
/// git の実行に失敗した場合はそのエラーを返す。
fn abort(
    language: Language,
    messages: &dyn Messages,
    operation: Operation,
    args: &[&str],
) -> Result<()> {
    // abort はここまでの解決作業を巻き戻す取り消し不能な操作であるため、
    // 実行するコマンドを示したうえで明示的な同意を求める
    confirm(
        messages,
        &abort_header(messages, operation),
        &[&command_display(args)],
    )?;

    execute(language, messages, args)
}

/// abort の確認プロンプトに示す、失われるものの説明。
fn abort_header(messages: &dyn Messages, operation: Operation) -> String {
    messages
        .in_progress()
        .abort_confirmation(operation_name(operation))
}

/// コンフリクト中のファイルを複数選択し、解決済みとして stage する。
///
/// # Errors
///
/// 一覧の取得、選択（中断を含む）、`git add` の実行に失敗した場合にエラーを返す。
/// 未解決のファイルが 1 件も無い場合も、その旨を示して失敗として返す。
fn stage_resolved(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<()> {
    let conflicts = changes(repository, ChangeScope::Unmerged)
        .context(messages.in_progress().conflicts_read_failed())?;
    if conflicts.is_empty() {
        bail!(messages.in_progress().no_conflicts());
    }

    let mut candidates = Vec::with_capacity(conflicts.len());
    let mut items = Vec::with_capacity(conflicts.len());
    for change in &conflicts {
        let candidate = to_candidate(change);
        items.push(to_conflict_item(language, &candidate));
        candidates.push(candidate);
    }

    let options = FinderOptions::new(SelectionMode::Multi)
        .with_header(messages.in_progress().conflicts_header().to_owned());
    let selected = select_many_with(items, &options)?;
    let selected = resolve_files(messages, &candidates, &selected)?;

    let paths = target_paths(&selected);
    let arguments = add_args(&paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).context(messages.in_progress().stage_failed())?;

    Ok(())
}

/// コンフリクト中のファイルを候補へ変換する。
///
/// `git status` はマージ未解決のエントリをリネームとして報告しないため、
/// 対象は常に自身のパスだけになる（`gz add` と同じ扱い）。
fn to_candidate(change: &FileChange) -> FileCandidate {
    FileCandidate::from_change(change, RenameOrigin::Exclude)
}

/// コンフリクト中のファイルを finder のアイテムへ変換する。
fn to_conflict_item(language: Language, candidate: &FileCandidate) -> FinderItem {
    FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        PreviewSource::Git(preview_args(&candidate.key)),
        language.messages(),
    )
    .with_highlights(candidate.highlights.clone())
}

/// プレビュー用の `git diff` の引数を組み立てる。
///
/// マージ未解決のファイルに対する `git diff` は各ステージを併合した差分を出すため、
/// 作業ツリーに書き込まれたコンフリクトマーカー（`<<<<<<<` 等）がそのまま含まれる。
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
/// コンフリクト中のファイルを `git add` するとマージ済み（解決済み）として index へ記録される。
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
    use crate::git::read::operation_in_progress;
    use crate::git::repo::discover;
    use crate::test_support::{
        TempDir, commit, create_branch, git_in, init_repository, try_git_in, write_file,
    };

    fn keys(entries: &[MenuEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.key).collect()
    }

    fn find(entries: &[MenuEntry], key: &str) -> MenuEntry {
        resolve_entry(Language::Japanese.messages(), entries, key)
            .expect("the key should belong to the menu")
            .clone()
    }

    /// 日本語のメニュー（既存のアサーションが対象とする言語）。
    fn japanese_menu(operation: Operation) -> Vec<MenuEntry> {
        menu(Language::Japanese.messages(), operation)
    }

    /// `shared.txt` の変更が衝突する 2 ブランチを持つテストリポジトリを用意する。
    fn repository_with_diverged_branches(label: &str) -> TempDir {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        write_file(dir.path(), "shared.txt", "base\n");
        git_in(dir.path(), &["add", "--", "shared.txt"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "base"]);

        create_branch(dir.path(), "other");
        write_file(dir.path(), "shared.txt", "main side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "main side"]);

        git_in(dir.path(), &["switch", "--quiet", "other"]);
        write_file(dir.path(), "shared.txt", "other side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "other side"]);

        dir
    }

    #[test]
    fn the_merge_menu_has_no_skip_entry() {
        // merge は 1 度の操作であり、飛ばす対象のコミットを持たない
        assert_eq!(
            keys(&japanese_menu(Operation::Merge)),
            [CONFLICTS_KEY, CONTINUE_KEY, ABORT_KEY]
        );
    }

    #[test]
    fn the_rebase_menu_offers_skip_before_abort() {
        assert_eq!(
            keys(&japanese_menu(Operation::Rebase)),
            [CONFLICTS_KEY, CONTINUE_KEY, SKIP_KEY, ABORT_KEY]
        );
    }

    #[test]
    fn the_merge_menu_runs_merge_subcommands_only() {
        let entries = japanese_menu(Operation::Merge);

        assert_eq!(
            find(&entries, CONTINUE_KEY).action,
            MenuAction::Proceed(&["merge", "--continue"])
        );
        assert_eq!(
            find(&entries, ABORT_KEY).action,
            MenuAction::Abort(&["merge", "--abort"])
        );
        assert_eq!(
            find(&entries, CONFLICTS_KEY).action,
            MenuAction::ShowConflicts
        );
    }

    #[test]
    fn the_rebase_menu_runs_rebase_subcommands_only() {
        let entries = japanese_menu(Operation::Rebase);

        assert_eq!(
            find(&entries, CONTINUE_KEY).action,
            MenuAction::Proceed(&["rebase", "--continue"])
        );
        assert_eq!(
            find(&entries, SKIP_KEY).action,
            MenuAction::Proceed(&["rebase", "--skip"])
        );
        assert_eq!(
            find(&entries, ABORT_KEY).action,
            MenuAction::Abort(&["rebase", "--abort"])
        );
    }

    #[test]
    fn every_entry_shows_the_command_it_runs() {
        assert_eq!(
            find(&japanese_menu(Operation::Merge), CONTINUE_KEY).display,
            "merge を再開する (git merge --continue)"
        );
        assert_eq!(
            find(&japanese_menu(Operation::Merge), ABORT_KEY).display,
            "merge を中止する (git merge --abort)"
        );
        assert_eq!(
            find(&japanese_menu(Operation::Rebase), CONTINUE_KEY).display,
            "rebase を再開する (git rebase --continue)"
        );
        assert_eq!(
            find(&japanese_menu(Operation::Rebase), SKIP_KEY).display,
            "現在のコミットを飛ばす (git rebase --skip)"
        );
        assert_eq!(
            find(&japanese_menu(Operation::Rebase), ABORT_KEY).display,
            "rebase を中止する (git rebase --abort)"
        );
    }

    #[test]
    fn the_conflicts_entry_is_the_same_for_both_operations() {
        let merge = find(&japanese_menu(Operation::Merge), CONFLICTS_KEY);
        let rebase = find(&japanese_menu(Operation::Rebase), CONFLICTS_KEY);

        assert_eq!(merge.display, rebase.display);
        assert_eq!(
            merge.display,
            Language::Japanese
                .messages()
                .in_progress()
                .conflicts_action()
        );
    }

    #[test]
    fn an_item_is_identified_by_its_key_not_by_its_display() {
        let entries = japanese_menu(Operation::Rebase);
        let items: Vec<FinderItem> = entries
            .iter()
            .map(|entry| to_item(Language::Japanese, entry))
            .collect();

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            [CONFLICTS_KEY, CONTINUE_KEY, SKIP_KEY, ABORT_KEY]
        );
    }

    #[test]
    fn the_menu_preview_shows_the_current_status_in_color() {
        assert_eq!(
            status_preview_args(),
            ["-c", "color.status=always", "status", "--short", "--branch"]
        );
    }

    #[test]
    fn a_selected_key_resolves_to_its_entry() {
        let entries = japanese_menu(Operation::Rebase);

        let entry = resolve_entry(Language::Japanese.messages(), &entries, SKIP_KEY)
            .expect("skip belongs to the rebase menu");

        assert_eq!(entry.action, MenuAction::Proceed(&["rebase", "--skip"]));
    }

    #[test]
    fn a_key_outside_of_the_menu_is_rejected() {
        // merge のメニューには skip が無いため、キーだけが渡ってきても解決できない
        let messages = Language::Japanese.messages();
        let err = resolve_entry(messages, &menu(messages, Operation::Merge), SKIP_KEY)
            .expect_err("an unknown key must be rejected");

        assert!(
            err.to_string().contains(SKIP_KEY),
            "the unknown key should be named: {err:#}"
        );
    }

    #[test]
    fn the_abort_confirmation_names_the_operation_and_what_is_lost() {
        let messages = Language::Japanese.messages();
        let header = abort_header(messages, Operation::Merge);

        assert!(header.contains("merge"), "unexpected header: {header}");
        assert!(header.contains("失われ"), "unexpected header: {header}");
        assert!(
            !header.contains("rebase"),
            "the other operation must not appear: {header}"
        );
        assert!(
            abort_header(messages, Operation::Rebase).contains("rebase"),
            "the rebase header should name rebase"
        );
    }

    #[test]
    fn a_command_line_is_shown_as_typed_by_the_user() {
        assert_eq!(command_display(&MERGE_ABORT_ARGS), "git merge --abort");
        assert_eq!(command_display(&REBASE_SKIP_ARGS), "git rebase --skip");
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
    fn a_conflict_is_previewed_with_its_own_diff() {
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
    fn a_conflicted_file_is_shown_with_its_status_code() {
        let dir = repository_with_diverged_branches("in-progress-candidates");
        assert!(
            !try_git_in(dir.path(), &["merge", "--no-edit", "main"]),
            "the merge is expected to conflict"
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let conflicts =
            changes(&repository, ChangeScope::Unmerged).expect("the status should be read");
        let candidates: Vec<FileCandidate> = conflicts.iter().map(to_candidate).collect();

        assert_eq!(operation_in_progress(&repository), Some(Operation::Merge));
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.display.as_str())
                .collect::<Vec<_>>(),
            ["UU shared.txt"]
        );
        assert_eq!(candidates[0].paths, ["shared.txt"]);
        assert_eq!(
            to_conflict_item(Language::Japanese, &candidates[0]).key(),
            "shared.txt",
            "the key must stay the path so that the selection resolves to it"
        );
    }

    #[test]
    fn nothing_left_to_resolve_is_reported_without_starting_the_finder() {
        let dir = TempDir::new("in-progress-no-conflicts");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = stage_resolved(
            Language::Japanese,
            Language::Japanese.messages(),
            &repository,
        )
        .expect_err("there is nothing to stage");

        assert!(
            err.to_string()
                .contains("コンフリクト中のファイルはありません"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn every_menu_entry_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            for operation in [Operation::Merge, Operation::Rebase] {
                for entry in menu(language.messages(), operation) {
                    assert!(
                        !entry.display.trim().is_empty(),
                        "{language:?} left the {key} entry empty",
                        key = entry.key
                    );
                }
            }
        }
    }

    #[test]
    fn every_entry_names_the_command_it_runs_in_every_language() {
        // 括弧内の git コマンド名は、実際に何が実行されるのかを示すため訳さない
        for language in [Language::Japanese, Language::English] {
            let entries = menu(language.messages(), Operation::Rebase);

            for (key, command) in [
                (CONTINUE_KEY, "git rebase --continue"),
                (SKIP_KEY, "git rebase --skip"),
                (ABORT_KEY, "git rebase --abort"),
            ] {
                let entry = resolve_entry(language.messages(), &entries, key)
                    .expect("the key belongs to the menu");

                assert!(
                    entry.display.contains(command),
                    "the {language:?} entry should name {command}: {display}",
                    display = entry.display
                );
            }
        }
    }

    #[test]
    fn a_menu_entry_is_matched_by_its_key_in_every_language() {
        // 照合に使うキーは表示と分かれているため、表示言語を変えても解決結果は変わらない
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();
            let entries = menu(messages, Operation::Merge);
            let entry =
                resolve_entry(messages, &entries, ABORT_KEY).expect("the key belongs to the menu");

            assert_eq!(entry.action, MenuAction::Abort(&MERGE_ABORT_ARGS));
        }
    }

    #[test]
    fn every_in_progress_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let in_progress = language.messages().in_progress();

            for text in [
                in_progress.conflicts_action(),
                in_progress.skip_action(),
                in_progress.conflicts_header(),
                in_progress.conflicts_read_failed(),
                in_progress.no_conflicts(),
                in_progress.stage_failed(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            for text in [
                in_progress.continue_action("merge"),
                in_progress.abort_action("merge"),
                in_progress.abort_confirmation("merge"),
            ] {
                assert!(
                    text.contains("merge"),
                    "{language:?} must name the operation: {text}"
                );
                assert!(
                    !text.contains("rebase"),
                    "the other operation must not appear: {text}"
                );
            }

            assert!(
                in_progress
                    .menu_selection_not_found("drop")
                    .contains("drop"),
                "{language:?} must name the selection"
            );
        }
    }

    #[test]
    fn the_in_progress_wording_is_translated() {
        let japanese = Language::Japanese.messages().in_progress();
        let english = Language::English.messages().in_progress();

        assert_ne!(japanese.conflicts_action(), english.conflicts_action());
        assert_ne!(
            japanese.continue_action("merge"),
            english.continue_action("merge")
        );
        assert_ne!(japanese.skip_action(), english.skip_action());
        assert_ne!(
            japanese.abort_action("rebase"),
            english.abort_action("rebase")
        );
        assert_ne!(
            japanese.menu_selection_not_found("drop"),
            english.menu_selection_not_found("drop")
        );
        assert_ne!(
            japanese.abort_confirmation("merge"),
            english.abort_confirmation("merge")
        );
        assert_ne!(japanese.conflicts_header(), english.conflicts_header());
        assert_ne!(
            japanese.conflicts_read_failed(),
            english.conflicts_read_failed()
        );
        assert_ne!(japanese.no_conflicts(), english.no_conflicts());
        assert_ne!(japanese.stage_failed(), english.stage_failed());
    }
}
