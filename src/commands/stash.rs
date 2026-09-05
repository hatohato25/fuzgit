//! `gz stash` — 変更ファイルを stash へ退避し、既存の stash を適用・破棄する（FR-6）。
//!
//! `push` が選ぶのは作業ツリーの「ファイル」、`apply` / `pop` / `drop` が選ぶのは既存の「stash」で、
//! 選択対象が異なるため入口を分けている（[`push`] と [`run`]）。

use anyhow::{Context as _, Result, anyhow};

use crate::commands::confirmation::confirm;
use crate::commands::file_selection::{FileCandidate, RenameOrigin, resolve_changes, target_paths};
use crate::commands::selection_header;
use crate::error::Error;
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many, select_one_with,
};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{ChangeScope, FileChange, StashEntry, changes, stashes};
use crate::i18n::{Language, Messages};

/// 失敗を伝える文言に用いる、実行する git のサブコマンド名。
const PUSH_COMMAND: &str = "git stash push";

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

    /// 候補一覧のヘッダーで示す「決定すると何が起きるのか」。
    ///
    /// apply / pop / drop は同じ stash 一覧から選ばせるため、一覧を見ただけでは結果が
    /// 分からない。網羅的な `match` にすることで、操作を増やしたときにヘッダーの
    /// 更新漏れがコンパイルエラーになる。
    fn header_outcome(self, messages: &dyn Messages) -> &'static str {
        match self {
            StashAction::Apply => messages.stash().header_outcome_apply(),
            StashAction::Pop => messages.stash().header_outcome_pop(),
            StashAction::Drop => messages.stash().header_outcome_drop(),
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
enum UntrackedFiles {
    /// 追跡済みファイルの変更のみを対象にする（既定）。
    Exclude,
    /// 未追跡ファイルも対象に含める（`-u` / `--include-untracked`）。
    Include,
}

impl UntrackedFiles {
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
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    message: Option<&str>,
) -> Result<()> {
    // **未追跡ファイルも必ず候補に載せる。**新しく作ったファイルを退避するために
    // `git add` を求めるのは、「選ぶだけで済ませる」という fuzgit の前提に反する
    // （`gz add` / `gz commit` / `gz status` はいずれも既定で未追跡を候補にしている）。
    // git へ `-u` を付けるかどうかは、選択の中身から [`untracked_files`] が決める
    let changes = changes(repository, ChangeScope::TrackedOrUntracked)
        .context(messages.common().changed_files_read_failed())?;

    let mut items = Vec::with_capacity(changes.len());
    for change in &changes {
        let candidate = to_candidate(change);
        items.push(to_file_item(language, repository, change, &candidate)?);
    }

    let selected = select_many(items)?;
    let selected = resolve_changes(messages, &changes, &selected)?;

    push_on_changes(language, messages, message, &selected)
}

/// 選択済みの変更ファイルを `git stash push` で退避する。
///
/// `gz status` のアクションメニュー（FR-16）からも呼ばれる。`untracked` は
/// 選択に未追跡ファイルを含む場合に [`UntrackedFiles::Include`] を渡すこと
/// （`--include-untracked` が無いと git 側が未追跡のパスを対象にできない）。
///
/// # Errors
///
/// `git stash push` の実行に失敗した場合にエラーを返す。
pub fn push_on_changes(
    language: Language,
    messages: &dyn Messages,
    message: Option<&str>,
    selected: &[&FileChange],
) -> Result<()> {
    let untracked = untracked_files(selected);
    let candidates: Vec<FileCandidate> =
        selected.iter().map(|change| to_candidate(change)).collect();
    let paths = target_paths(&candidates.iter().collect::<Vec<&FileCandidate>>());

    let arguments = push_args(message, untracked, &paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).context(messages.common().command_run_failed(PUSH_COMMAND))?;

    Ok(())
}

/// 変更ファイルを候補へ変換する。
///
/// リネームの変更元は index に存在せず、パススペックとして渡すと
/// 「did not match any file(s) known to git」になるため対象にしない。
fn to_candidate(change: &FileChange) -> FileCandidate {
    FileCandidate::from_change(change, RenameOrigin::Exclude)
}

/// stash を 1 件選び、`git stash apply|pop|drop` を実行する。
///
/// # Errors
///
/// stash 一覧の取得、選択（中断を含む）、確認の否認、`git stash` の実行に失敗した場合に
/// エラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    action: StashAction,
) -> Result<()> {
    let candidates = stashes(repository).context(messages.common().stash_list_read_failed())?;

    let items = candidates
        .iter()
        .map(|entry| to_stash_item(language, entry))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.stash().header_subject(),
        action.header_outcome(messages),
    ));
    let selected = select_one_with(items, &options)?;

    let entry = candidates
        .iter()
        .find(|candidate| candidate.selector() == selected)
        .ok_or_else(|| anyhow!(messages.stash().selection_not_found(&selected)))?;

    if action.needs_confirmation() {
        confirm(
            messages,
            messages.stash().drop_confirmation(),
            &[&display_line(entry)],
        )?;
    }

    let selector = entry.selector();
    run_git(language, &["stash", action.subcommand(), &selector]).with_context(|| {
        messages.common().command_run_failed(&format!(
            "git stash {subcommand} {selector}",
            subcommand = action.subcommand()
        ))
    })?;

    Ok(())
}

/// `git stash push [--message <message>] [--include-untracked] -- <pathspec>...` の引数を組み立てる。
///
/// パスは必ず `--` の後ろへ置き、オプションとして解釈される余地を排除する。
/// 選択に未追跡ファイルが含まれるかどうかから、git へ `-u` を付けるかを決める。
///
/// 未追跡ファイルは `--include-untracked` を付けないと退避できず、pathspec に含めた場合は
/// git が「did not match any file(s) known to git」で失敗する（実測確認済み）。
///
/// 逆に、選択が追跡済みだけのときに `-u` を付けても**他の未追跡ファイルを巻き込むことは無い**
/// （pathspec が対象を限るため。実測確認済み）。それでも選択から導くのは、実行する
/// コマンドと選んだ内容を一致させ、デバッグログを読んだときに食い違わせないためである。
fn untracked_files(selected: &[&FileChange]) -> UntrackedFiles {
    if selected.iter().any(|change| change.is_untracked()) {
        UntrackedFiles::Include
    } else {
        UntrackedFiles::Exclude
    }
}

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
    language: Language,
    repository: &gix::Repository,
    change: &FileChange,
    candidate: &FileCandidate,
) -> Result<FinderItem> {
    Ok(FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        file_preview(repository, change)?,
        language.messages(),
    )
    .with_highlights(candidate.highlights.clone()))
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

/// stash に含まれるファイルを見せるプレビューセクションの見出し。
///
/// `gz status` の `staged` / `unstaged` と同じく、git の語彙をそのまま使うため翻訳しない。
const FILES_LABEL: &str = "files";

/// stash の差分を見せるプレビューセクションの見出し。
const DIFF_LABEL: &str = "diff";

/// プレビューを組み立てる。
///
/// **2 つのセクションに分ける。**先頭に「何が入っているのか」（`--stat` によるファイル名と
/// 変更量）を置き、その下に差分を置く。差分だけだと、ファイル数の多い stash で
/// 「結局どれが入っているのか」を掴むのに画面を送ることになる（利用者からの要望）。
///
/// 出力が空になったセクションは見出しごと省かれる（[`PreviewSource::Composite`]）。
fn stash_preview(entry: &StashEntry) -> PreviewSource {
    PreviewSource::Composite(vec![
        (
            FILES_LABEL.to_owned(),
            PreviewSource::Git(stash_show_args(entry, "--stat")),
        ),
        (
            DIFF_LABEL.to_owned(),
            PreviewSource::Git(stash_show_args(entry, "-p")),
        ),
    ])
}

/// プレビュー用の `git stash show` の引数を組み立てる。
///
/// **`--include-untracked` を必ず付ける。**`git stash show` は既定で未追跡ファイルを
/// 出さないため、未追跡だけを退避した stash（`gz stash push` は候補に未追跡を含む）では
/// プレビューが**丸ごと空になる**（実測確認済み）。未追跡を含まない stash に付けても
/// 出力は変わらない（同）。
///
/// このオプションは **Git 2.32 以降**でのみ使える（2.30 では失敗する。実測確認済み）。
/// それ未満の git ではプレビューにエラーが出るが、stash の選択と適用そのものは動く。
fn stash_show_args(entry: &StashEntry, format: &str) -> Vec<String> {
    [
        "stash",
        "show",
        format,
        INCLUDE_UNTRACKED_OPTION,
        "--color=always",
        &entry.selector(),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// stash を finder の候補へ変換する。
fn to_stash_item(language: Language, entry: &StashEntry) -> FinderItem {
    FinderItem::new(
        display_line(entry),
        entry.selector(),
        stash_preview(entry),
        language.messages(),
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
    fn the_preview_shows_the_files_and_then_the_diff() {
        // 差分だけだと、ファイル数の多い stash で「何が入っているのか」を掴むのに
        // 画面を送ることになる。先に一覧を出す
        let PreviewSource::Composite(sections) = stash_preview(&entry(1, "WIP on main: 5d21a8c"))
        else {
            panic!("プレビューは 2 つのセクションで構成する");
        };

        let labels: Vec<&str> = sections.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, [FILES_LABEL, DIFF_LABEL]);
    }

    #[test]
    fn the_preview_always_asks_for_untracked_files() {
        // `git stash show` は既定で未追跡を出さない。`gz stash push` は未追跡も候補に
        // 含めるため、これが無いと未追跡だけの stash でプレビューが丸ごと空になる
        let PreviewSource::Composite(sections) = stash_preview(&entry(1, "WIP on main: 5d21a8c"))
        else {
            panic!("プレビューは 2 つのセクションで構成する");
        };

        for (label, source) in &sections {
            let PreviewSource::Git(args) = source else {
                panic!("{label} は git の実行であること");
            };
            assert!(
                args.iter().any(|arg| arg == INCLUDE_UNTRACKED_OPTION),
                "{label} に {INCLUDE_UNTRACKED_OPTION} が要る: {args:?}"
            );
            assert_eq!(args.last().map(String::as_str), Some("stash@{1}"));
        }
    }

    #[test]
    fn the_two_sections_ask_git_for_different_shapes() {
        assert_eq!(
            stash_show_args(&entry(1, "x"), "--stat"),
            [
                "stash",
                "show",
                "--stat",
                "--include-untracked",
                "--color=always",
                "stash@{1}"
            ]
        );
        assert_eq!(
            stash_show_args(&entry(1, "x"), "-p"),
            [
                "stash",
                "show",
                "-p",
                "--include-untracked",
                "--color=always",
                "stash@{1}"
            ]
        );
    }

    #[test]
    fn an_item_keeps_the_selector_as_its_key() {
        let item = to_stash_item(Language::Japanese, &entry(10, "On main: 作業中"));

        assert_eq!(item.key(), "stash@{10}");
    }

    #[test]
    fn a_selection_with_an_untracked_file_adds_the_option() {
        // 未追跡ファイルは `-u` を付けないと退避できず、pathspec に含めると
        // git が「did not match any file(s) known to git」で失敗する（実測確認済み）
        let changes = [change("a.txt", " M"), change("new.txt", "??")];
        let selected: Vec<&FileChange> = changes.iter().collect();

        assert_eq!(untracked_files(&selected), UntrackedFiles::Include);
    }

    #[test]
    fn a_selection_of_tracked_files_leaves_the_option_off() {
        // 付けても他の未追跡ファイルを巻き込むことは無いが、実行するコマンドと
        // 選んだ内容を一致させるため、選択に無ければ付けない
        let changes = [change("a.txt", " M"), change("b.txt", "M ")];
        let selected: Vec<&FileChange> = changes.iter().collect();

        assert_eq!(untracked_files(&selected), UntrackedFiles::Exclude);
    }

    #[test]
    fn an_empty_selection_leaves_the_option_off() {
        assert_eq!(untracked_files(&[]), UntrackedFiles::Exclude);
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
    fn the_option_follows_the_selection_not_a_flag() {
        // 候補には常に未追跡ファイルを出す。git へ `--include-untracked` を渡すかは
        // 選択の中身が決める——渡さずに未追跡を pathspec へ含めると、git が
        // 「pathspec did not match」で失敗する（実測確認済み）
        assert_eq!(UntrackedFiles::Exclude.option(), None);
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
        let candidate = to_candidate(&change);

        let item = to_file_item(Language::Japanese, &repository, &change, &candidate)
            .expect("the item should build");

        assert_eq!(item.key(), "new.txt");
        assert_eq!(candidate.display, "?? new.txt");
    }

    #[test]
    fn the_header_states_what_enter_does_for_each_action() {
        // apply / pop / drop は同じ一覧から選ばせて結果だけが変わる。取り違えると
        // 復元したかった stash を消してしまうため、文言が重なっていないことを確かめる
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();
            let outcomes = [
                StashAction::Apply.header_outcome(messages),
                StashAction::Pop.header_outcome(messages),
                StashAction::Drop.header_outcome(messages),
            ];

            for (index, outcome) in outcomes.iter().enumerate() {
                assert!(
                    !outcomes[index + 1..].contains(outcome),
                    "{language:?} must tell the actions apart: {outcome}"
                );
            }
        }
    }

    #[test]
    fn every_stash_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let stash = language.messages().stash();

            for text in [
                stash.header_subject(),
                stash.header_outcome_apply(),
                stash.header_outcome_pop(),
                stash.header_outcome_drop(),
                stash.drop_confirmation(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            assert!(
                stash.selection_not_found("stash@{2}").contains("stash@{2}"),
                "{language:?} must name the selection"
            );
        }
    }

    #[test]
    fn the_stash_wording_is_translated() {
        let japanese = Language::Japanese.messages().stash();
        let english = Language::English.messages().stash();

        assert_ne!(japanese.header_subject(), english.header_subject());
        assert_ne!(
            japanese.header_outcome_apply(),
            english.header_outcome_apply()
        );
        assert_ne!(japanese.header_outcome_pop(), english.header_outcome_pop());
        assert_ne!(
            japanese.header_outcome_drop(),
            english.header_outcome_drop()
        );
        assert_ne!(japanese.drop_confirmation(), english.drop_confirmation());
        assert_ne!(
            japanese.selection_not_found("stash@{2}"),
            english.selection_not_found("stash@{2}")
        );
    }

    #[test]
    fn the_candidates_always_include_untracked_files() {
        // 新しく作ったファイルを退避するために `git add` を求めるのは、
        // 「選ぶだけで済ませる」という fuzgit の前提に反する。`gz add` / `gz commit` /
        // `gz status` と同じく、未追跡ファイルは既定で候補に出す
        let dir = TempDir::new("stash-push-candidates");
        init_repository(dir.path());
        write_file(dir.path(), "tracked.txt", "original\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");
        write_file(dir.path(), "tracked.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "tracked.txt"]);
        write_file(dir.path(), "untracked.txt", "new\n");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let candidates =
            changes(&repository, ChangeScope::TrackedOrUntracked).expect("status should be read");

        assert_eq!(
            candidates
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["tracked.txt", "untracked.txt"],
            "ステージ済みの変更も未追跡ファイルも、どちらも候補に出る"
        );
    }
}
