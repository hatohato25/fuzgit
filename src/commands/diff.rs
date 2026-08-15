//! `gz diff` — 比較対象を選択して差分を表示する（FR-17）。
//!
//! 比較モードは clap の排他フラグで指定し（git 本体の語彙に合わせる）、fuzzy finder で
//! 選ぶのは「比較するリビジョン」（`--branch` / `--commit`）と「表示するファイル」である。
//!
//! `--branch` / `--commit` は比較元・比較先の 2 回、[`select_one_with`] を逐次起動する。
//! どちらを選んでいるのかが分かるよう、ヘッダーに `1/2` / `2/2` を表示する。
//! 1 回目・2 回目のどちらで中断しても git は実行しない。
//!
//! 最終的な差分表示は `git diff <範囲> -- <pathspec>...` を継承 stdio で実行し、
//! ページャ・色付けは git に委ねる。

use anyhow::{Context as _, Result, anyhow, bail};

use crate::cli::DEFAULT_COMMIT_LIMIT;
use crate::commands::file_selection::{FileCandidate, resolve, target_paths};
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with, select_one_with,
};
use crate::git::exec::{pathspec, run_git};
use crate::git::read::{
    BranchInfo, BranchScope, CommitInfo, CommitScope, branches, changed_files, commits,
    current_branch, upstream,
};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};

/// 現在の位置を指すリビジョン。
const HEAD_REVISION: &str = "HEAD";

/// ステージ済みの変更を対象にする `git diff` のオプション。
const STAGED_OPTION: &str = "--staged";

/// キャプチャ実行でも色付けを維持させる `git diff` のオプション（プレビュー用）。
const COLOR_OPTION: &str = "--color=always";

/// プレビューに表示する直近コミット数（ブランチ候補）。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// 比較のモード。
///
/// clap の 5 つの排他フラグをそのまま `commands` 層へ持ち回さず、取り得る 6 通りを型で表す
/// （[`crate::commands::merge::MergeMode`] と同方針）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// index と作業ツリーを比較する（フラグ指定なし）。
    ///
    /// これは `git diff` を引数なしで実行したときの比較対象と同じである。
    /// 「指定が無いので何かへ倒す」暗黙のフォールバックではなく、`gz diff` が
    /// git 本体の既定に準拠していることを意味する（requirements.md FR-17。
    /// 語彙を git と揃えることで暗記負担を減らす設計）。
    Unstaged,
    /// HEAD と index を比較する（`--staged`）。
    Staged,
    /// HEAD と作業ツリーを比較する（`--head`）。ステージ済みの変更も含む。
    Head,
    /// HEAD と現在ブランチの upstream を比較する（`--upstream`）。
    Upstream,
    /// ブランチを 2 回選んで比較する（`--branch`）。
    BranchToBranch,
    /// コミットを 2 回選んで比較する（`--commit`）。
    CommitToCommit,
}

impl DiffMode {
    /// `--staged` / `--head` / `--upstream` / `--branch` / `--commit` の指定からモードを決める。
    ///
    /// 排他性は `clap` の `conflicts_with_all` でも担保しているが、複数が立った状態を
    /// 暗黙にどれか 1 つへ倒すことがないよう、ここでも明示的に拒否する
    /// （[`crate::commands::merge::MergeMode::from_flags`] と同方針）。
    ///
    /// # Errors
    ///
    /// 2 つ以上が同時に指定された場合にエラーを返す。
    pub fn from_flags(
        messages: &dyn Messages,
        staged: bool,
        head: bool,
        upstream: bool,
        branch: bool,
        commit: bool,
    ) -> Result<Self> {
        match (staged, head, upstream, branch, commit) {
            (false, false, false, false, false) => Ok(Self::Unstaged),
            (true, false, false, false, false) => Ok(Self::Staged),
            (false, true, false, false, false) => Ok(Self::Head),
            (false, false, true, false, false) => Ok(Self::Upstream),
            (false, false, false, true, false) => Ok(Self::BranchToBranch),
            (false, false, false, false, true) => Ok(Self::CommitToCommit),
            _ => bail!(messages.diff().conflicting_modes()),
        }
    }
}

/// 確定した比較範囲。
///
/// `git diff` のサブコマンド名の後ろ・`--` の前に置く引数をそのまま保持する。
/// design.md の方針どおり、リビジョン同士の比較は `<a>..<b>` の 1 引数ではなく
/// `git diff <a> <b>` の 2 引数で表す（`..` は共通祖先からの差分など別の意味を持ち得るため、
/// 「2 つの状態を並べて比べる」という意図をそのまま引数の形にする）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffRange {
    /// `git diff` へ渡すオプション・リビジョン。
    arguments: Vec<String>,
    /// 見出し・メッセージに用いる比較範囲の呼称。
    description: String,
}

impl DiffRange {
    /// index と作業ツリーの比較（引数なしの `git diff` と同じ）。
    fn unstaged(messages: &dyn Messages) -> Self {
        Self {
            arguments: Vec::new(),
            description: messages.diff().unstaged_range().to_owned(),
        }
    }

    /// HEAD と index の比較（`git diff --staged`）。
    fn staged(messages: &dyn Messages) -> Self {
        Self {
            arguments: vec![STAGED_OPTION.to_owned()],
            description: messages.diff().staged_range().to_owned(),
        }
    }

    /// HEAD と作業ツリーの比較（`git diff HEAD`）。
    fn head(messages: &dyn Messages) -> Self {
        Self {
            arguments: vec![HEAD_REVISION.to_owned()],
            description: messages.diff().head_range().to_owned(),
        }
    }

    /// 2 つのリビジョンの比較（`git diff <base> <target>`）。
    fn revisions(base: &str, target: &str) -> Self {
        Self::labelled_revisions(base, base, target, target)
    }

    /// [`DiffRange::revisions`] と同じだが、表示にはリビジョンとは別の呼称を用いる。
    ///
    /// コミット同士の比較では git へ渡すのは取り違えの起きないフルハッシュだが、
    /// 見出しは候補リストの幅で打ち切られるため、表示には短縮ハッシュを使う。
    fn labelled_revisions(base: &str, base_label: &str, target: &str, target_label: &str) -> Self {
        Self {
            arguments: vec![base.to_owned(), target.to_owned()],
            description: format!("{base_label} → {target_label}"),
        }
    }
}

/// 比較対象を確定し、選択したファイルの差分を表示する。
///
/// # Errors
///
/// 比較範囲の確定（upstream 未設定・detached HEAD を含む）、候補の取得、
/// 選択（中断を含む）、`git diff` の実行に失敗した場合にエラーを返す。
/// 比較範囲に差分が無い場合はエラーにせず、その旨を伝えて正常終了する。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    mode: DiffMode,
) -> Result<()> {
    let range = resolve_range(language, messages, repository, mode)?;

    let files = changed_files(workdir(repository)?, &range.arguments)
        .with_context(|| messages.diff().files_read_failed(&range.description))?;

    if files.is_empty() {
        return report_no_diff(messages, &mut std::io::stderr(), &range);
    }

    let candidates: Vec<FileCandidate> = files
        .iter()
        .map(|path| FileCandidate::from_path(path))
        .collect();
    let items = candidates
        .iter()
        .map(|candidate| to_item(language, &range, candidate))
        .collect();
    let options = FinderOptions::new(SelectionMode::Multi).with_header(range.description.clone());
    let selected = select_many_with(items, &options)?;

    // skim が返す選択順は候補順と一致しないため、候補一覧の並び（git が報告した順）へ戻す
    let selected = resolve(messages, &candidates, &selected)?;
    let paths = target_paths(&selected);

    let arguments = diff_args(&range, &paths);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments)
        .with_context(|| messages.diff().diff_failed(&range.description))?;

    Ok(())
}

/// モードに応じて比較範囲を確定する。
///
/// `--branch` / `--commit` はここで選択 UI を 2 回起動する。
///
/// # Errors
///
/// upstream が無い場合、候補の取得・選択に失敗した場合にエラーを返す。
fn resolve_range(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    mode: DiffMode,
) -> Result<DiffRange> {
    match mode {
        DiffMode::Unstaged => Ok(DiffRange::unstaged(messages)),
        DiffMode::Staged => Ok(DiffRange::staged(messages)),
        DiffMode::Head => Ok(DiffRange::head(messages)),
        DiffMode::Upstream => upstream_range(messages, repository),
        DiffMode::BranchToBranch => branch_range(language, messages, repository),
        DiffMode::CommitToCommit => commit_range(language, messages, repository),
    }
}

/// HEAD と現在ブランチの upstream の比較範囲を組み立てる。
///
/// 比較先はネットワークを使わずに読めるローカルのリモート追跡参照
/// （`refs/remotes/<remote>/<branch>`）であり、`gz status` の ahead/behind と同じ基準を使う。
///
/// # Errors
///
/// detached HEAD の場合、upstream が設定されていない場合、追跡参照を組み立てられない
/// 設定の場合に、それぞれの原因を示すエラーを返す（推測して別の対象へ倒さない）。
fn upstream_range(messages: &dyn Messages, repository: &gix::Repository) -> Result<DiffRange> {
    let Some(branch) =
        current_branch(repository).context(messages.common().current_branch_read_failed())?
    else {
        bail!(messages.common().detached_head_without_upstream());
    };

    let Some(upstream) = upstream(repository, &branch)
        .with_context(|| messages.common().upstream_read_failed(&branch))?
    else {
        bail!(messages.common().upstream_not_configured(&branch));
    };

    let Some(reference) = upstream.tracking_ref() else {
        bail!(messages.diff().tracking_ref_unavailable(
            &branch,
            &upstream.remote,
            &upstream.merge_ref
        ));
    };

    Ok(DiffRange::revisions(HEAD_REVISION, &reference))
}

/// ブランチを 2 回選んで比較範囲を組み立てる。
///
/// # Errors
///
/// ブランチ一覧の取得、選択（中断を含む）に失敗した場合にエラーを返す。
fn branch_range(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<DiffRange> {
    // リモート追跡ブランチも候補に含める。`main` と `origin/main` の比較は
    // 「push していない変更の確認」という日常的な用途であるため
    let candidates = branches(repository, BranchScope::All)
        .context(messages.common().branch_list_read_failed())?;

    let base = choose_branch(
        language,
        messages,
        &candidates,
        messages.diff().base_branch_header(),
    )?;
    let target = choose_branch(
        language,
        messages,
        &candidates,
        messages.diff().target_branch_header(),
    )?;

    Ok(DiffRange::revisions(&base, &target))
}

/// コミットを 2 回選んで比較範囲を組み立てる。
///
/// # Errors
///
/// コミット履歴の取得、選択（中断を含む）に失敗した場合にエラーを返す。
fn commit_range(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<DiffRange> {
    // 候補・プレビューは `gz log` と共通（requirements.md FR-17）
    let candidates = commits(repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT)
        .context(messages.common().commit_history_read_failed())?;

    let base = choose_commit(
        language,
        messages,
        &candidates,
        messages.diff().base_commit_header(),
    )?;
    let target = choose_commit(
        language,
        messages,
        &candidates,
        messages.diff().target_commit_header(),
    )?;

    Ok(DiffRange::labelled_revisions(
        &base.id,
        &base.short_id,
        &target.id,
        &target.short_id,
    ))
}

/// ブランチを 1 件選び、候補一覧との照合を経た名前を返す。
///
/// # Errors
///
/// 選択（中断を含む）に失敗した場合、選択結果が候補一覧に無い場合にエラーを返す。
fn choose_branch(
    language: Language,
    messages: &dyn Messages,
    candidates: &[BranchInfo],
    header: &str,
) -> Result<String> {
    let items = candidates
        .iter()
        .map(|branch| branch_item(language, branch))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(header.to_owned());
    let selected = select_one_with(items, &options)?;

    // `git diff` はリビジョンを位置引数に取り `--` で保護できないため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .map(|candidate| candidate.name.clone())
        .ok_or_else(|| anyhow!(messages.diff().branch_selection_not_found(&selected)))
}

/// コミットを 1 件選び、候補一覧との照合を経たコミットを返す。
///
/// git へ渡すのはフルハッシュ、見出しに使うのは短縮ハッシュであるため、
/// どちらも取れるよう候補そのものを返す。
///
/// # Errors
///
/// [`choose_branch`] と同じ。
fn choose_commit<'a>(
    language: Language,
    messages: &dyn Messages,
    candidates: &'a [CommitInfo],
    header: &str,
) -> Result<&'a CommitInfo> {
    let items = candidates
        .iter()
        .map(|commit| commit_item(language, commit))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(header.to_owned());
    let selected = select_one_with(items, &options)?;

    candidates
        .iter()
        .find(|candidate| candidate.id == selected)
        .ok_or_else(|| anyhow!(messages.diff().commit_selection_not_found(&selected)))
}

/// ブランチ候補の一覧に表示する 1 行を組み立てる。
fn branch_display_line(branch: &BranchInfo) -> String {
    branch.name.clone()
}

/// ブランチ候補のプレビュー（直近のコミットログ）の引数を組み立てる。
fn branch_preview_args(branch: &BranchInfo) -> Vec<String> {
    // 末尾の `--` により、ブランチ名がパスではなくリビジョンとして解釈されることを保証する
    [
        "log",
        COLOR_OPTION,
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        &branch.name,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// ブランチを finder の候補へ変換する。
fn branch_item(language: Language, branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        branch_display_line(branch),
        branch.name.clone(),
        PreviewSource::Git(branch_preview_args(branch)),
        language.messages(),
    )
}

/// コミット候補の一覧に表示する 1 行を組み立てる（`gz log` と同じ並び）。
fn commit_display_line(commit: &CommitInfo) -> String {
    format!(
        "{short_id} {time} {summary} ({author})",
        short_id = commit.short_id,
        time = commit.time,
        summary = commit.summary,
        author = commit.author
    )
}

/// コミット候補のプレビュー（`git show`）の引数を組み立てる。
fn commit_preview_args(commit: &CommitInfo) -> Vec<String> {
    // 末尾の `--` により、ハッシュがパスではなくリビジョンとして解釈されることを保証する
    ["show", COLOR_OPTION, &commit.id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// コミットを finder の候補へ変換する。
fn commit_item(language: Language, commit: &CommitInfo) -> FinderItem {
    FinderItem::new(
        commit_display_line(commit),
        commit.id.clone(),
        PreviewSource::Git(commit_preview_args(commit)),
        language.messages(),
    )
}

/// 変更ファイルのプレビュー（選択中ファイルに限定した差分）の引数を組み立てる。
fn file_preview_args(range: &DiffRange, path: &str) -> Vec<String> {
    let mut arguments = vec!["diff".to_owned(), COLOR_OPTION.to_owned()];
    arguments.extend(range.arguments.iter().cloned());
    arguments.push("--".to_owned());
    arguments.push(pathspec(path));
    arguments
}

/// 変更ファイルを finder の候補へ変換する。
fn to_item(language: Language, range: &DiffRange, candidate: &FileCandidate) -> FinderItem {
    FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        PreviewSource::Git(file_preview_args(range, &candidate.key)),
        language.messages(),
    )
}

/// `git diff <範囲> -- <pathspec>...` の引数を組み立てる。
///
/// パスは必ず `--` の後ろに [`pathspec`] として置き、リビジョンやオプションとして
/// 解釈される余地を無くす。
fn diff_args(range: &DiffRange, paths: &[String]) -> Vec<String> {
    let mut arguments = vec!["diff".to_owned()];
    arguments.extend(range.arguments.iter().cloned());
    arguments.push("--".to_owned());
    arguments.extend(paths.iter().map(|path| pathspec(path)));
    arguments
}

/// 差分が無いことを伝える 1 行を組み立てる。
fn no_diff_message(messages: &dyn Messages, range: &DiffRange) -> String {
    messages.diff().no_diff(&range.description)
}

/// 差分が無いことを伝えて正常終了する。
///
/// 標準出力は git 自身の差分出力のために空けておくため、案内は標準エラーへ出す。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_no_diff(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    range: &DiffRange,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = no_diff_message(messages, range)
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 既定（日本語）の文言一式。文言そのものを固定するテスト以外はこれを使う。
    fn messages() -> &'static dyn Messages {
        Language::Japanese.messages()
    }

    fn branch(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    fn commit() -> CommitInfo {
        CommitInfo {
            id: "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345".to_owned(),
            short_id: "1f0c9a4".to_owned(),
            summary: "ブランチ切替を実装する".to_owned(),
            author: "fuzgit test".to_owned(),
            time: "2024-01-02".to_owned(),
        }
    }

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn no_flag_compares_the_index_with_the_work_tree_like_plain_git_diff() {
        assert_eq!(
            DiffMode::from_flags(messages(), false, false, false, false, false)
                .expect("no flag is a valid combination"),
            DiffMode::Unstaged
        );
    }

    #[test]
    fn each_flag_selects_its_own_mode() {
        let combinations = [
            ((true, false, false, false, false), DiffMode::Staged),
            ((false, true, false, false, false), DiffMode::Head),
            ((false, false, true, false, false), DiffMode::Upstream),
            ((false, false, false, true, false), DiffMode::BranchToBranch),
            ((false, false, false, false, true), DiffMode::CommitToCommit),
        ];

        for ((staged, head, upstream, branch, commit), expected) in combinations {
            let mode = DiffMode::from_flags(messages(), staged, head, upstream, branch, commit)
                .unwrap_or_else(|err| panic!("a single flag should be accepted: {err:#}"));

            assert_eq!(mode, expected);
        }
    }

    #[test]
    fn two_flags_are_rejected_instead_of_falling_back_to_one_of_them() {
        let combinations = [
            (true, true, false, false, false),
            (true, false, false, false, true),
            (false, true, true, false, false),
            (false, false, false, true, true),
            (true, true, true, true, true),
        ];

        for (staged, head, upstream, branch, commit) in combinations {
            let err = DiffMode::from_flags(messages(), staged, head, upstream, branch, commit)
                .expect_err("combined flags must be rejected");

            assert!(
                err.to_string().contains("同時に指定できません"),
                "the conflict should be explained: {err:#}"
            );
        }
    }

    #[test]
    fn the_unstaged_range_adds_no_argument_of_its_own() {
        assert!(
            DiffRange::unstaged(messages()).arguments.is_empty(),
            "plain `git diff` already compares the index with the work tree"
        );
    }

    #[test]
    fn the_staged_range_uses_the_git_option_of_the_same_name() {
        assert_eq!(DiffRange::staged(messages()).arguments, [STAGED_OPTION]);
    }

    #[test]
    fn the_head_range_names_the_revision_to_compare_against() {
        assert_eq!(DiffRange::head(messages()).arguments, [HEAD_REVISION]);
    }

    #[test]
    fn two_revisions_are_passed_as_two_arguments_instead_of_a_range_expression() {
        let range = DiffRange::revisions("main", "origin/main");

        assert_eq!(range.arguments, ["main", "origin/main"]);
        assert!(
            !range
                .arguments
                .iter()
                .any(|argument| argument.contains("..")),
            "`<a>..<b>` has its own meaning and must not be used here: {range:?}"
        );
    }

    #[test]
    fn commits_are_compared_by_their_full_hash_but_shown_by_the_short_one() {
        let commit = commit();
        let range = DiffRange::labelled_revisions(
            &commit.id,
            &commit.short_id,
            HEAD_REVISION,
            HEAD_REVISION,
        );

        assert_eq!(
            range.arguments,
            ["1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345", "HEAD"],
            "git must receive the unambiguous full hash"
        );
        assert_eq!(
            range.description, "1f0c9a4 → HEAD",
            "the header is truncated to the width of the candidate list"
        );
    }

    #[test]
    fn the_upstream_range_compares_head_with_the_local_tracking_reference() {
        let range = DiffRange::revisions(HEAD_REVISION, "refs/remotes/origin/main");

        assert_eq!(range.arguments, ["HEAD", "refs/remotes/origin/main"]);
    }

    #[test]
    fn the_paths_are_placed_after_the_separator_as_pathspecs() {
        let arguments = diff_args(
            &DiffRange::unstaged(messages()),
            &paths(&["src/main.rs", "a b.txt"]),
        );

        assert_eq!(
            arguments,
            [
                "diff",
                "--",
                ":(top,literal)src/main.rs",
                ":(top,literal)a b.txt"
            ]
        );
    }

    #[test]
    fn the_range_is_kept_between_the_subcommand_and_the_separator() {
        assert_eq!(
            diff_args(&DiffRange::staged(messages()), &paths(&["a.txt"])),
            ["diff", STAGED_OPTION, "--", ":(top,literal)a.txt"]
        );
        assert_eq!(
            diff_args(&DiffRange::head(messages()), &paths(&["a.txt"])),
            ["diff", "HEAD", "--", ":(top,literal)a.txt"]
        );
        assert_eq!(
            diff_args(
                &DiffRange::revisions("main", "origin/main"),
                &paths(&["a.txt"])
            ),
            ["diff", "main", "origin/main", "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn a_path_starting_with_a_dash_is_never_taken_for_an_option() {
        let arguments = diff_args(&DiffRange::unstaged(messages()), &paths(&["-weird.txt"]));

        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .expect("the separator should be present");
        assert!(
            arguments[separator + 1..]
                .iter()
                .all(|argument| argument.starts_with(":(top,literal)")),
            "every path must be a pathspec behind the separator: {arguments:?}"
        );
    }

    #[test]
    fn a_preview_is_limited_to_the_selected_file_and_keeps_the_range() {
        let range = DiffRange::revisions("main", "feature");

        assert_eq!(
            file_preview_args(&range, "src/main.rs"),
            [
                "diff",
                COLOR_OPTION,
                "main",
                "feature",
                "--",
                ":(top,literal)src/main.rs"
            ]
        );
    }

    #[test]
    fn a_preview_of_the_unstaged_range_carries_the_colour_option_alone() {
        assert_eq!(
            file_preview_args(&DiffRange::unstaged(messages()), "a.txt"),
            ["diff", COLOR_OPTION, "--", ":(top,literal)a.txt"]
        );
    }

    #[test]
    fn a_file_item_is_keyed_by_its_path_and_previews_that_path_only() {
        let range = DiffRange::head(messages());
        let candidate = FileCandidate::from_path("dir/with space.txt");

        let item = to_item(Language::Japanese, &range, &candidate);

        assert_eq!(item.key(), "dir/with space.txt");
    }

    #[test]
    fn a_branch_item_keeps_the_branch_name_as_its_key() {
        let item = branch_item(Language::Japanese, &branch("origin/main"));

        assert_eq!(item.key(), "origin/main");
        assert_eq!(
            branch_preview_args(&branch("origin/main")),
            [
                "log",
                COLOR_OPTION,
                "--oneline",
                "--decorate",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "origin/main",
                "--"
            ]
        );
    }

    #[test]
    fn a_commit_item_keeps_the_full_hash_as_its_key_and_shows_the_same_line_as_the_log() {
        let item = commit_item(Language::Japanese, &commit());

        assert_eq!(item.key(), "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345");
        assert_eq!(
            commit_display_line(&commit()),
            "1f0c9a4 2024-01-02 ブランチ切替を実装する (fuzgit test)"
        );
        assert_eq!(
            commit_preview_args(&commit()),
            [
                "show",
                COLOR_OPTION,
                "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345",
                "--"
            ]
        );
    }

    #[test]
    fn the_message_of_an_empty_diff_names_the_range_that_was_compared() {
        assert_eq!(
            no_diff_message(messages(), &DiffRange::unstaged(messages())),
            "差分はありません（未ステージの変更: index と作業ツリー）"
        );
        assert_eq!(
            no_diff_message(messages(), &DiffRange::revisions("main", "origin/main")),
            "差分はありません（main → origin/main）"
        );
    }

    #[test]
    fn every_diff_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let diff = language.messages().diff();

            for text in [
                diff.conflicting_modes(),
                diff.unstaged_range(),
                diff.staged_range(),
                diff.head_range(),
                diff.base_branch_header(),
                diff.target_branch_header(),
                diff.base_commit_header(),
                diff.target_commit_header(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            for (text, argument) in [
                (
                    diff.tracking_ref_unavailable("main", "origin", "refs/heads/main"),
                    "refs/heads/main",
                ),
                (diff.files_read_failed("HEAD → main"), "HEAD → main"),
                (diff.branch_selection_not_found("main"), "main"),
                (diff.commit_selection_not_found("1f0c9a4"), "1f0c9a4"),
                (diff.no_diff("HEAD → main"), "HEAD → main"),
                (diff.diff_failed("HEAD → main"), "HEAD → main"),
            ] {
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_two_step_headers_keep_their_progress_in_both_languages() {
        // どちらを選んでいるのかを示す `1/2` / `2/2` は数値であり訳さない
        for language in [Language::Japanese, Language::English] {
            let diff = language.messages().diff();

            assert!(
                diff.base_branch_header().starts_with("1/2")
                    && diff.base_commit_header().starts_with("1/2"),
                "{language:?} must keep the progress of the first step"
            );
            assert!(
                diff.target_branch_header().starts_with("2/2")
                    && diff.target_commit_header().starts_with("2/2"),
                "{language:?} must keep the progress of the second step"
            );
        }
    }

    #[test]
    fn the_diff_wording_is_translated() {
        let japanese = Language::Japanese.messages().diff();
        let english = Language::English.messages().diff();

        assert_ne!(japanese.conflicting_modes(), english.conflicting_modes());
        assert_ne!(japanese.unstaged_range(), english.unstaged_range());
        assert_ne!(japanese.staged_range(), english.staged_range());
        assert_ne!(japanese.head_range(), english.head_range());
        assert_ne!(japanese.base_branch_header(), english.base_branch_header());
        assert_ne!(
            japanese.target_branch_header(),
            english.target_branch_header()
        );
        assert_ne!(japanese.base_commit_header(), english.base_commit_header());
        assert_ne!(
            japanese.target_commit_header(),
            english.target_commit_header()
        );
        assert_ne!(
            japanese.tracking_ref_unavailable("main", "origin", "refs/heads/main"),
            english.tracking_ref_unavailable("main", "origin", "refs/heads/main")
        );
        assert_ne!(
            japanese.files_read_failed("HEAD → main"),
            english.files_read_failed("HEAD → main")
        );
        assert_ne!(
            japanese.branch_selection_not_found("main"),
            english.branch_selection_not_found("main")
        );
        assert_ne!(
            japanese.commit_selection_not_found("1f0c9a4"),
            english.commit_selection_not_found("1f0c9a4")
        );
        assert_ne!(
            japanese.no_diff("HEAD → main"),
            english.no_diff("HEAD → main")
        );
        assert_ne!(
            japanese.diff_failed("HEAD → main"),
            english.diff_failed("HEAD → main")
        );
    }

    #[test]
    fn the_range_of_a_message_follows_the_language_of_the_range_itself() {
        // 比較範囲の呼称は見出しにもメッセージにも現れるため、同じ文言一式から引く
        let english = Language::English.messages();

        assert!(
            no_diff_message(english, &DiffRange::unstaged(english))
                .contains(english.diff().unstaged_range()),
            "the range must be described in the language of the message"
        );
    }

    #[test]
    fn an_empty_diff_is_reported_without_failing() {
        let mut written = Vec::new();

        report_no_diff(messages(), &mut written, &DiffRange::staged(messages()))
            .expect("reporting should succeed");

        let text = String::from_utf8(written).expect("the message should be utf-8");
        assert!(
            text.contains(messages().diff().staged_range()),
            "the empty diff should be reported with the range it compared: {text}"
        );
        assert!(
            text.ends_with('\n'),
            "the line should be terminated: {text}"
        );
    }
}
