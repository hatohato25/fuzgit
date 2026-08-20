//! `gz status` — 変更ファイルの状態を一覧し、選んだファイルに操作を行う（FR-16）。
//!
//! 候補生成は `gz commit` と同じ（staged / unstaged / 未追跡を 1 リスト化）で、
//! 決定後にアクションメニュー（`select_one`）を挟む 2 段選択になる。
//! 各アクションの実体は対応する既存コマンドの公開関数であり、安全策
//! （restore の確認プロンプト、commit の未追跡の事前 add 等）もそのまま働く。

use anyhow::{Context as _, Result, anyhow};

use crate::commands::file_selection::{FileCandidate, RenameOrigin, resolve_changes};
use crate::commands::restore::RestoreTarget;
use crate::commands::stash::UntrackedFiles;
use crate::commands::{HEADER_SEPARATOR, add, commit, restore, stash, status_preview_args};
use crate::error::Error;
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with, select_one,
};
use crate::git::exec::pathspec;
use crate::git::read::{
    ChangeScope, FileChange, ahead_behind, changes, current_branch, stashes, upstream,
};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};

/// detached HEAD（ブランチを指していない状態）のヘッダー表示。
const DETACHED_LABEL: &str = "detached HEAD";

/// staged の変更を見せるプレビューセクションの見出し。
const STAGED_LABEL: &str = "staged";

/// unstaged の変更を見せるプレビューセクションの見出し。
const UNSTAGED_LABEL: &str = "unstaged";

/// 「ステージする」項目の finder キー。
const ADD_KEY: &str = "add";

/// 「変更を破棄する」項目の finder キー。
const RESTORE_KEY: &str = "restore";

/// 「stash へ退避する」項目の finder キー。
const STASH_KEY: &str = "stash";

/// 「コミットする」項目の finder キー。
const COMMIT_KEY: &str = "commit";

/// 「パスを標準出力へ出力する」項目の finder キー。
const PRINT_KEY: &str = "print";

/// ヘッダーに 1 行で示す現在の状態。
///
/// ヘッダーは候補リストの幅で打ち切られるため、区画を増やさず 1 行に収める。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Summary {
    /// 現在のブランチ名。detached HEAD では `None`。
    branch: Option<String>,
    /// upstream に対する (ahead, behind)。upstream が無い場合は `None`。
    ahead_behind: Option<(usize, usize)>,
    /// ステージ済みの変更を持つファイル数。
    staged: usize,
    /// 未ステージの変更を持つファイル数。
    unstaged: usize,
    /// 未追跡ファイル数。
    untracked: usize,
    /// stash の件数。
    stashes: usize,
}

/// アクションメニューで選べる操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    /// 選択したファイルをステージする（`gz add` と同じ）。
    Add,
    /// 選択したファイルの作業ツリーの変更を破棄する（`gz restore` と同じく確認プロンプトを伴う）。
    Restore,
    /// 選択したファイルを stash へ退避する（`gz stash push` と同じ）。
    StashPush,
    /// 選択したファイルの変更だけをコミットする（`gz commit` と同じ）。
    Commit,
    /// 選択したファイルのパスを標準出力へ出力する（パイプ用途）。
    PrintPaths,
}

/// アクションメニューの 1 項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MenuEntry {
    /// finder のキー。選択結果の照合はこのキーで行い、表示文字列では行わない。
    key: &'static str,
    /// 一覧表示および絞り込み対象の文字列。
    display: &'static str,
    /// 決定時に行う操作。
    action: MenuAction,
}

/// 変更ファイルを一覧し、選択したファイルに対してアクションを実行する。
///
/// # Errors
///
/// 状態の取得、選択（中断を含む）、実行したアクションの失敗時にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<()> {
    let changes = changes(repository, ChangeScope::TrackedOrUntracked)
        .context(messages.common().changed_files_read_failed())?;
    let summary = summarize(messages, repository, &changes)?;

    if changes.is_empty() {
        return report_clean(messages, &mut std::io::stderr(), &summary);
    }

    let mut items = Vec::with_capacity(changes.len());
    for change in &changes {
        items.push(to_item(language, repository, change)?);
    }

    let options = FinderOptions::new(SelectionMode::Multi).with_header(header_line(&summary));
    let selected = select_many_with(items, &options)?;
    let selected = resolve_changes(messages, &changes, &selected)?;

    let entries = menu(messages);
    let items = entries
        .iter()
        .map(|entry| to_menu_item(language, entry))
        .collect();
    let chosen = select_one(items)?;
    let action = resolve_action(messages, &entries, &chosen)?;

    run_action(language, messages, action, &selected)
}

/// ヘッダーに表示する現在の状態を集める。
///
/// # Errors
///
/// ブランチ・upstream・ahead/behind・stash 一覧の取得に失敗した場合にエラーを返す。
fn summarize(
    messages: &dyn Messages,
    repository: &gix::Repository,
    changes: &[FileChange],
) -> Result<Summary> {
    let branch =
        current_branch(repository).context(messages.common().current_branch_read_failed())?;
    let ahead_behind = match &branch {
        Some(branch) => tracking_position(messages, repository, branch)?,
        // detached HEAD には upstream の設定が無く、比較対象を決められない
        None => None,
    };
    let stashes = stashes(repository).context(messages.common().stash_list_read_failed())?;

    Ok(Summary {
        branch,
        ahead_behind,
        staged: count(changes, FileChange::has_staged_change),
        unstaged: count(changes, FileChange::has_worktree_change),
        untracked: count(changes, FileChange::is_untracked),
        stashes: stashes.len(),
    })
}

/// 条件を満たす変更ファイルの数を数える。
///
/// ステージ済みと未ステージの両方の変更を持つファイル（`MM` 等）は双方で数える。
/// `git status` が index 側・作業ツリー側を別々に報告するのと同じ見方をするため。
fn count(changes: &[FileChange], predicate: fn(&FileChange) -> bool) -> usize {
    changes.iter().filter(|change| predicate(change)).count()
}

/// 現在のブランチと upstream の (ahead, behind) を求める。upstream が無い場合は `None`。
///
/// # Errors
///
/// upstream の取得、ahead/behind の算出に失敗した場合にエラーを返す。
fn tracking_position(
    messages: &dyn Messages,
    repository: &gix::Repository,
    branch: &str,
) -> Result<Option<(usize, usize)>> {
    let Some(upstream) = upstream(repository, branch)
        .with_context(|| messages.common().upstream_read_failed(branch))?
    else {
        return Ok(None);
    };
    let Some(reference) = upstream.tracking_ref() else {
        return Ok(None);
    };

    ahead_behind(workdir(repository)?, branch, &reference)
        .with_context(|| messages.common().ahead_behind_failed(&reference))
}

/// ヘッダーの 1 行を組み立てる。
///
/// upstream が無い場合は ahead/behind の区画ごと省略する（無関係な `0` を並べない）。
fn header_line(summary: &Summary) -> String {
    let mut sections = vec![match &summary.branch {
        Some(branch) => branch.clone(),
        None => DETACHED_LABEL.to_owned(),
    }];

    if let Some((ahead, behind)) = summary.ahead_behind {
        sections.push(format!("ahead {ahead} / behind {behind}"));
    }

    sections.push(format!(
        "staged {staged} / unstaged {unstaged} / untracked {untracked} / stash {stashes}",
        staged = summary.staged,
        unstaged = summary.unstaged,
        untracked = summary.untracked,
        stashes = summary.stashes
    ));

    sections.join(HEADER_SEPARATOR)
}

/// 変更が 1 件も無いことと、ヘッダー相当の情報を書き出す。
///
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_clean(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    summary: &Summary,
) -> Result<()> {
    // クリーンな状態の確認も `gz status` の用途であるため、候補ゼロをエラーにしない
    // （requirements.md FR-16）
    writeln!(writer, "{clean}", clean = messages.status().clean())
        .context(messages.common().stderr_write_failed())?;
    writeln!(writer, "{header}", header = header_line(summary))
        .context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// 変更ファイルを finder のアイテムへ変換する。
///
/// # Errors
///
/// 未追跡ファイルの絶対パスを解決できない（作業ツリーを持たない）場合にエラーを返す。
fn to_item(
    language: Language,
    repository: &gix::Repository,
    change: &FileChange,
) -> Result<FinderItem> {
    // ここで作る候補は表示とプレビューのためのもの。git へ渡すパスの決め方は
    // アクションごとに異なるため、実行時に各コマンドの公開関数が組み立て直す。
    // プレビューはリネームの変更元も含めて表示する（変更前後を並べて見せるため）
    let candidate = FileCandidate::from_change(change, RenameOrigin::Include);

    Ok(FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        preview(repository, change, &candidate.paths)?,
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
    paths: &[String],
) -> Result<PreviewSource> {
    if change.is_untracked() {
        // 未追跡ファイルは git の管理下に無く差分を取れないため内容をそのまま表示する。
        // 色付けされていない内容を ANSI として解釈させないよう、連結せず単体で扱う。
        // プレビューはカレントディレクトリに依存しないよう絶対パスで指定する
        let path = repository
            .workdir_path(change.path.as_str())
            .ok_or(Error::NoWorktree)?;
        return Ok(PreviewSource::File(path));
    }

    Ok(PreviewSource::Composite(sections(change, paths)))
}

/// 状態コードに応じたプレビューのセクションを組み立てる。
///
/// 該当しない観点の git は実行しない（プレビューは選択項目ごとに都度生成されるため、
/// 1 項目あたりの git 実行を最小限に保つ）。
fn sections(change: &FileChange, paths: &[String]) -> Vec<(String, PreviewSource)> {
    let mut sections = Vec::new();

    if change.has_staged_change() {
        sections.push((
            STAGED_LABEL.to_owned(),
            PreviewSource::Git(staged_args(paths)),
        ));
    }
    if change.has_worktree_change() {
        sections.push((
            UNSTAGED_LABEL.to_owned(),
            PreviewSource::Git(unstaged_args(paths)),
        ));
    }

    sections
}

/// staged セクション用の `git diff --cached` の引数を組み立てる。
fn staged_args(paths: &[String]) -> Vec<String> {
    ["diff", "--color=always", "--cached", "--"]
        .into_iter()
        .map(str::to_owned)
        .chain(paths.iter().map(|path| pathspec(path)))
        .collect()
}

/// unstaged セクション用の `git diff` の引数を組み立てる。
fn unstaged_args(paths: &[String]) -> Vec<String> {
    ["diff", "--color=always", "--"]
        .into_iter()
        .map(str::to_owned)
        .chain(paths.iter().map(|path| pathspec(path)))
        .collect()
}

/// アクションメニューの項目を組み立てる。
///
/// 項目は固定であり、選択済みファイルの内容によって増減させない
/// （毎回同じ並びであることが、選び間違いを防ぐうえで重要なため）。
fn menu(messages: &dyn Messages) -> Vec<MenuEntry> {
    vec![
        MenuEntry {
            key: ADD_KEY,
            display: messages.status().add_action(),
            action: MenuAction::Add,
        },
        MenuEntry {
            key: RESTORE_KEY,
            display: messages.status().restore_action(),
            action: MenuAction::Restore,
        },
        MenuEntry {
            key: STASH_KEY,
            display: messages.status().stash_action(),
            action: MenuAction::StashPush,
        },
        MenuEntry {
            key: COMMIT_KEY,
            display: messages.status().commit_action(),
            action: MenuAction::Commit,
        },
        MenuEntry {
            key: PRINT_KEY,
            display: messages.status().print_action(),
            action: MenuAction::PrintPaths,
        },
    ]
}

/// メニュー項目を finder のアイテムへ変換する。
fn to_menu_item(language: Language, entry: &MenuEntry) -> FinderItem {
    FinderItem::new(
        entry.display.to_owned(),
        entry.key.to_owned(),
        PreviewSource::Git(status_preview_args()),
        language.messages(),
    )
}

/// 選択されたキーに対応する操作を返す。
///
/// # Errors
///
/// 選択されたキーがメニューに含まれない場合にエラーを返す（対象を取り違えたまま
/// git 操作を実行しないよう、暗黙に読み飛ばさない）。
fn resolve_action(
    messages: &dyn Messages,
    entries: &[MenuEntry],
    selected: &str,
) -> Result<MenuAction> {
    entries
        .iter()
        .find(|entry| entry.key == selected)
        .map(|entry| entry.action)
        .ok_or_else(|| anyhow!(messages.status().menu_selection_not_found(selected)))
}

/// 選択されたファイルに対して操作を実行する。
///
/// 実行部は対応する既存コマンドの公開関数をそのまま呼ぶ。安全策（restore の確認プロンプト、
/// commit の未追跡ファイルの事前 add 等）は関数側にあるため、`gz status` 経由でも
/// 各コマンドを直接使った場合と同じ挙動になる。
///
/// # Errors
///
/// 呼び出した操作が失敗した場合にエラーを返す。
fn run_action(
    language: Language,
    messages: &dyn Messages,
    action: MenuAction,
    selected: &[&FileChange],
) -> Result<()> {
    match action {
        MenuAction::Add => add::run_on_changes(language, messages, selected),
        // 破棄できるのは作業ツリーの変更であり、実行前に確認プロンプトが表示される
        MenuAction::Restore => {
            restore::run_on_changes(language, messages, RestoreTarget::Worktree, selected)
        }
        MenuAction::StashPush => stash::push_on_changes(
            language,
            messages,
            None,
            untracked_files(selected),
            selected,
        ),
        // メッセージの入力は `gz commit` と同じくエディタ（git）に委ねる
        MenuAction::Commit => commit::run_on_changes(language, messages, None, selected),
        MenuAction::PrintPaths => print_paths(messages, &mut std::io::stdout(), selected),
    }
}

/// 選択に未追跡ファイルが含まれるかどうかから `git stash push` の対象範囲を決める。
///
/// 未追跡ファイルは `--include-untracked` を付けないと退避できず、pathspec に含めた場合は
/// git が「did not match any file(s) known to git」で失敗する。
fn untracked_files(selected: &[&FileChange]) -> UntrackedFiles {
    if selected.iter().any(|change| change.is_untracked()) {
        UntrackedFiles::Include
    } else {
        UntrackedFiles::Exclude
    }
}

/// 選択されたファイルのパスを 1 行ずつ書き出す。
///
/// パイプ利用を想定し、標準出力にはパス以外を混ぜない。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn print_paths(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    selected: &[&FileChange],
) -> Result<()> {
    for change in selected {
        // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
        writeln!(writer, "{path}", path = change.path)
            .context(messages.common().stdout_write_failed())?;
    }

    Ok(())
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

    fn summary() -> Summary {
        Summary {
            branch: Some("main".to_owned()),
            ahead_behind: Some((2, 1)),
            staged: 3,
            unstaged: 2,
            untracked: 1,
            stashes: 4,
        }
    }

    fn labels(sections: &[(String, PreviewSource)]) -> Vec<&str> {
        sections.iter().map(|(label, _)| label.as_str()).collect()
    }

    fn selected(changes: &[FileChange]) -> Vec<&FileChange> {
        changes.iter().collect()
    }

    #[test]
    fn the_header_fits_on_one_line() {
        // ヘッダーは候補リストの幅で打ち切られるため、改行を含めない
        let header = header_line(&summary());

        assert_eq!(header.lines().count(), 1, "unexpected header: {header}");
        assert_eq!(
            header,
            "main  |  ahead 2 / behind 1  |  staged 3 / unstaged 2 / untracked 1 / stash 4"
        );
    }

    #[test]
    fn a_branch_without_an_upstream_omits_the_counts_it_cannot_have() {
        let header = header_line(&Summary {
            ahead_behind: None,
            ..summary()
        });

        assert_eq!(
            header,
            "main  |  staged 3 / unstaged 2 / untracked 1 / stash 4"
        );
        assert!(
            !header.contains("ahead") && !header.contains("behind"),
            "an unset upstream has no counts to show: {header}"
        );
    }

    #[test]
    fn a_detached_head_is_named_instead_of_a_branch() {
        let header = header_line(&Summary {
            branch: None,
            ahead_behind: None,
            ..summary()
        });

        assert!(
            header.starts_with(DETACHED_LABEL),
            "unexpected header: {header}"
        );
    }

    #[test]
    fn a_clean_work_tree_still_shows_every_count() {
        let header = header_line(&Summary {
            ahead_behind: None,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            stashes: 1,
            ..summary()
        });

        assert_eq!(
            header,
            "main  |  staged 0 / unstaged 0 / untracked 0 / stash 1"
        );
    }

    #[test]
    fn a_file_with_both_kinds_of_change_is_counted_on_both_sides() {
        let changes = [
            change("staged.txt", "M "),
            change("unstaged.txt", " M"),
            change("both.txt", "MM"),
            change("new.txt", "??"),
        ];

        assert_eq!(count(&changes, FileChange::has_staged_change), 2);
        assert_eq!(count(&changes, FileChange::has_worktree_change), 2);
        assert_eq!(count(&changes, FileChange::is_untracked), 1);
    }

    #[test]
    fn a_staged_only_file_is_previewed_by_its_staged_section_alone() {
        let sections = sections(&change("a.txt", "M "), &paths(&["a.txt"]));

        assert_eq!(labels(&sections), [STAGED_LABEL]);
        assert_eq!(
            sections[0].1,
            PreviewSource::Git(paths(&[
                "diff",
                "--color=always",
                "--cached",
                "--",
                ":(top,literal)a.txt"
            ]))
        );
    }

    #[test]
    fn an_unstaged_only_file_is_previewed_by_its_unstaged_section_alone() {
        let sections = sections(&change("a.txt", " M"), &paths(&["a.txt"]));

        assert_eq!(labels(&sections), [UNSTAGED_LABEL]);
        assert_eq!(
            sections[0].1,
            PreviewSource::Git(paths(&[
                "diff",
                "--color=always",
                "--",
                ":(top,literal)a.txt"
            ]))
        );
    }

    #[test]
    fn a_file_changed_on_both_sides_gets_both_sections() {
        let sections = sections(&change("a.txt", "MM"), &paths(&["a.txt"]));

        assert_eq!(labels(&sections), [STAGED_LABEL, UNSTAGED_LABEL]);
    }

    #[test]
    fn a_conflicted_file_gets_both_sections() {
        // マージ未解決（`UU` 等）は index 側・作業ツリー側の双方に差分を持つ
        let sections = sections(&change("a.txt", "UU"), &paths(&["a.txt"]));

        assert_eq!(labels(&sections), [STAGED_LABEL, UNSTAGED_LABEL]);
    }

    #[test]
    fn a_rename_is_previewed_with_both_of_its_paths() {
        let change = rename("new.txt", "old.txt", "R ");
        let candidate = FileCandidate::from_change(&change, RenameOrigin::Include);

        let sections = sections(&change, &candidate.paths);

        assert_eq!(
            sections[0].1,
            PreviewSource::Git(paths(&[
                "diff",
                "--color=always",
                "--cached",
                "--",
                ":(top,literal)new.txt",
                ":(top,literal)old.txt"
            ]))
        );
    }

    #[test]
    fn an_untracked_file_is_previewed_by_its_content_only() {
        let dir = TempDir::new("status-preview-untracked");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        write_file(dir.path(), "dir/new file.txt", "untracked\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        let change = change("dir/new file.txt", "??");

        let preview = preview(&repository, &change, &paths(&["dir/new file.txt"]))
            .expect("the path resolves");

        match preview {
            PreviewSource::File(path) => assert_eq!(path, dir.path().join("dir/new file.txt")),
            other => panic!("an untracked file must be previewed by content: {other:?}"),
        }
    }

    #[test]
    fn an_item_keeps_the_path_as_its_key_and_shows_the_status_code() {
        let dir = TempDir::new("status-item");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        write_file(dir.path(), "a.txt", "modified\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let item = to_item(Language::Japanese, &repository, &change("a.txt", " M"))
            .expect("the item should build");

        assert_eq!(item.key(), "a.txt");
    }

    #[test]
    fn the_menu_offers_every_action_in_a_fixed_order() {
        let entries = menu(Language::Japanese.messages());

        assert_eq!(
            entries.iter().map(|entry| entry.key).collect::<Vec<_>>(),
            [ADD_KEY, RESTORE_KEY, STASH_KEY, COMMIT_KEY, PRINT_KEY]
        );
    }

    #[test]
    fn each_entry_resolves_to_its_own_action() {
        let entries = menu(Language::Japanese.messages());

        for (key, expected) in [
            (ADD_KEY, MenuAction::Add),
            (RESTORE_KEY, MenuAction::Restore),
            (STASH_KEY, MenuAction::StashPush),
            (COMMIT_KEY, MenuAction::Commit),
            (PRINT_KEY, MenuAction::PrintPaths),
        ] {
            assert_eq!(
                resolve_action(Language::Japanese.messages(), &entries, key)
                    .expect("the key belongs to the menu"),
                expected
            );
        }
    }

    #[test]
    fn a_key_outside_of_the_menu_is_rejected() {
        let messages = Language::Japanese.messages();
        let err = resolve_action(messages, &menu(messages), "drop")
            .expect_err("an unknown key must be rejected");

        assert!(
            err.to_string().contains("drop"),
            "the unknown key should be named: {err:#}"
        );
    }

    #[test]
    fn every_entry_names_the_command_it_runs() {
        // 括弧内の git コマンド名は、実際に何が実行されるのかを示すため訳さない
        for language in [Language::Japanese, Language::English] {
            let entries = menu(language.messages());

            for (key, command) in [
                (ADD_KEY, "git add"),
                (RESTORE_KEY, "git restore"),
                (STASH_KEY, "git stash push"),
                (COMMIT_KEY, "git commit"),
            ] {
                let entry = entries
                    .iter()
                    .find(|entry| entry.key == key)
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
    fn an_item_is_identified_by_its_key_not_by_its_display() {
        let entries = menu(Language::Japanese.messages());
        let items: Vec<FinderItem> = entries
            .iter()
            .map(|entry| to_menu_item(Language::Japanese, entry))
            .collect();

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            [ADD_KEY, RESTORE_KEY, STASH_KEY, COMMIT_KEY, PRINT_KEY]
        );
    }

    #[test]
    fn every_status_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let status = language.messages().status();

            for text in [
                status.clean(),
                status.add_action(),
                status.restore_action(),
                status.stash_action(),
                status.commit_action(),
                status.print_action(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            assert!(
                status.menu_selection_not_found("drop").contains("drop"),
                "{language:?} must name the selection"
            );
        }
    }

    #[test]
    fn the_status_wording_is_translated() {
        let japanese = Language::Japanese.messages().status();
        let english = Language::English.messages().status();

        assert_ne!(japanese.clean(), english.clean());
        assert_ne!(japanese.add_action(), english.add_action());
        assert_ne!(japanese.restore_action(), english.restore_action());
        assert_ne!(japanese.stash_action(), english.stash_action());
        assert_ne!(japanese.commit_action(), english.commit_action());
        assert_ne!(japanese.print_action(), english.print_action());
        assert_ne!(
            japanese.menu_selection_not_found("drop"),
            english.menu_selection_not_found("drop")
        );
    }

    #[test]
    fn a_menu_entry_is_matched_by_its_key_in_every_language() {
        // 照合に使うキーは表示と分かれているため、表示言語を変えても解決結果は変わらない
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();

            assert_eq!(
                resolve_action(messages, &menu(messages), ADD_KEY)
                    .expect("the key belongs to the menu"),
                MenuAction::Add
            );
        }
    }

    #[test]
    fn a_selection_with_an_untracked_file_stashes_untracked_files_too() {
        let changes = [change("a.txt", " M"), change("new.txt", "??")];

        assert_eq!(
            untracked_files(&selected(&changes)),
            UntrackedFiles::Include,
            "an untracked path cannot be stashed without the option"
        );
    }

    #[test]
    fn a_selection_of_tracked_files_stashes_them_as_they_are() {
        let changes = [change("a.txt", " M"), change("b.txt", "M ")];

        assert_eq!(
            untracked_files(&selected(&changes)),
            UntrackedFiles::Exclude
        );
    }

    #[test]
    fn only_the_paths_are_written_to_the_output() {
        let changes = [
            change("a.txt", "M "),
            rename("new.txt", "old.txt", "R "),
            change("dir/with space.txt", "??"),
        ];
        let mut output = Vec::new();

        print_paths(
            Language::Japanese.messages(),
            &mut output,
            &selected(&changes),
        )
        .expect("writing to a buffer cannot fail");

        assert_eq!(
            String::from_utf8(output).expect("the output should be utf-8"),
            "a.txt\nnew.txt\ndir/with space.txt\n",
            "the status code must not be written to stdout"
        );
    }

    #[test]
    fn the_clean_report_states_the_cause_and_the_current_state() {
        let messages = Language::Japanese.messages();
        let mut output = Vec::new();

        report_clean(messages, &mut output, &summary()).expect("writing to a buffer cannot fail");

        let text = String::from_utf8(output).expect("the output should be utf-8");
        assert!(
            text.contains(messages.status().clean()),
            "unexpected report: {text}"
        );
        assert!(
            text.contains(&header_line(&summary())),
            "the header information should be repeated: {text}"
        );
    }

    #[test]
    fn a_clean_repository_produces_no_candidates_but_a_summary() {
        let dir = TempDir::new("status-clean");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let changes =
            changes(&repository, ChangeScope::TrackedOrUntracked).expect("status should be read");
        let summary = summarize(Language::Japanese.messages(), &repository, &changes)
            .expect("the summary should be built");

        assert!(changes.is_empty(), "unexpected changes: {changes:?}");
        assert_eq!(summary.branch.as_deref(), Some("main"));
        assert_eq!(summary.ahead_behind, None, "there is no upstream");
        assert_eq!(summary.stashes, 0);
    }

    #[test]
    fn the_summary_counts_the_changes_and_the_stashes() {
        let dir = TempDir::new("status-summary");
        init_repository(dir.path());
        write_file(dir.path(), "tracked.txt", "original\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");

        write_file(dir.path(), "tracked.txt", "stashed\n");
        git_in(dir.path(), &["stash", "push", "--quiet"]);

        write_file(dir.path(), "tracked.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "tracked.txt"]);
        write_file(dir.path(), "tracked.txt", "and modified again\n");
        write_file(dir.path(), "untracked.txt", "new\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let changes =
            changes(&repository, ChangeScope::TrackedOrUntracked).expect("status should be read");
        let summary = summarize(Language::Japanese.messages(), &repository, &changes)
            .expect("the summary should be built");

        assert_eq!(summary.staged, 1);
        assert_eq!(summary.unstaged, 1);
        assert_eq!(summary.untracked, 1);
        assert_eq!(summary.stashes, 1);
        assert_eq!(
            header_line(&summary),
            "main  |  staged 1 / unstaged 1 / untracked 1 / stash 1"
        );
    }
}
