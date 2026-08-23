//! 英語の文言。
//!
//! 英語はフォールバック言語であり、解決に失敗した場合・環境から判定できない場合に
//! 選ばれる（FR-25 の層 5）。

use std::path::Path;

use super::Language;
use super::messages::{
    BranchManageMessages, BranchMessages, CherryPickMessages, CliMessages, CommitMenuMessages,
    CommitMessages, CommonMessages, ConfirmMessages, DiffMessages, ErrorMessages, FetchMessages,
    FileSelectionMessages, FinderMessages, FixupMessages, InProgressMessages, LogMessages,
    MergeMessages, Messages, PullMessages, RebaseMessages, ReflogMessages, RestoreMessages,
    RevertMessages, StashMessages, StatusMessages, SyncMessages, WorktreeMessages,
};
use crate::error::{Error, stderr_suffix};
use crate::git::read::{BRANCH_REF_PREFIX, MalformedOutput, ReadOperation, WORKTREE_LABEL};
use crate::git::siblings::FilesystemOperation;

/// 英語の文言一式。
///
/// フィールドを持たない ZST であり、[`Language::messages`] から `&'static` 参照として
/// 返せる。文言を追加するときは [`Messages`] にメソッドを足し、ここと
/// [`crate::i18n::ja`] の双方へ実装を書く（片方でも欠けるとコンパイルエラーになる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishMessages;

impl Messages for EnglishMessages {
    fn language(&self) -> Language {
        Language::English
    }

    fn common(&self) -> &dyn CommonMessages {
        &EnglishCommonMessages
    }

    fn cli(&self) -> &dyn CliMessages {
        &EnglishCliMessages
    }

    fn errors(&self) -> &dyn ErrorMessages {
        &EnglishErrorMessages
    }

    fn finder(&self) -> &dyn FinderMessages {
        &EnglishFinderMessages
    }

    fn confirm(&self) -> &dyn ConfirmMessages {
        &EnglishConfirmMessages
    }

    fn branch(&self) -> &dyn BranchMessages {
        &EnglishBranchMessages
    }

    fn branch_manage(&self) -> &dyn BranchManageMessages {
        &EnglishBranchManageMessages
    }

    fn cherry_pick(&self) -> &dyn CherryPickMessages {
        &EnglishCherryPickMessages
    }

    fn file_selection(&self) -> &dyn FileSelectionMessages {
        &EnglishFileSelectionMessages
    }

    fn reflog(&self) -> &dyn ReflogMessages {
        &EnglishReflogMessages
    }

    fn restore(&self) -> &dyn RestoreMessages {
        &EnglishRestoreMessages
    }

    fn revert(&self) -> &dyn RevertMessages {
        &EnglishRevertMessages
    }

    fn stash(&self) -> &dyn StashMessages {
        &EnglishStashMessages
    }

    fn commit(&self) -> &dyn CommitMessages {
        &EnglishCommitMessages
    }

    fn fixup(&self) -> &dyn FixupMessages {
        &EnglishFixupMessages
    }

    fn status(&self) -> &dyn StatusMessages {
        &EnglishStatusMessages
    }

    fn merge(&self) -> &dyn MergeMessages {
        &EnglishMergeMessages
    }

    fn rebase(&self) -> &dyn RebaseMessages {
        &EnglishRebaseMessages
    }

    fn in_progress(&self) -> &dyn InProgressMessages {
        &EnglishInProgressMessages
    }

    fn worktree(&self) -> &dyn WorktreeMessages {
        &EnglishWorktreeMessages
    }

    fn sync(&self) -> &dyn SyncMessages {
        &EnglishSyncMessages
    }

    fn diff(&self) -> &dyn DiffMessages {
        &EnglishDiffMessages
    }

    fn fetch(&self) -> &dyn FetchMessages {
        &EnglishFetchMessages
    }

    fn pull(&self) -> &dyn PullMessages {
        &EnglishPullMessages
    }

    fn log(&self) -> &dyn LogMessages {
        &EnglishLogMessages
    }

    fn commit_menu(&self) -> &dyn CommitMenuMessages {
        &EnglishCommitMenuMessages
    }
}

/// 複数のコマンドで共有する語彙の英語表示。
///
/// いずれも `anyhow` の `context` として連鎖の途中に現れるため、**文末に句点を置かない**
/// （[`EnglishErrorMessages`] を参照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishCommonMessages;

impl CommonMessages for EnglishCommonMessages {
    fn stdout_write_failed(&self) -> &'static str {
        "Failed to write to standard output"
    }

    fn stderr_write_failed(&self) -> &'static str {
        "Failed to write to standard error"
    }

    fn commit_history_read_failed(&self) -> &'static str {
        "Failed to read the commit history"
    }

    fn commit_hash_parse_failed(&self, id: &str) -> String {
        format!("Cannot parse the commit hash `{id}`")
    }

    fn commit_read_failed(&self, id: &str) -> String {
        format!("Failed to read the commit `{id}`")
    }

    fn changed_files_read_failed(&self) -> &'static str {
        "Failed to list the changed files"
    }

    fn branch_list_read_failed(&self) -> &'static str {
        "Failed to list the branches"
    }

    fn stash_list_read_failed(&self) -> &'static str {
        "Failed to list the stashes"
    }

    fn tag_list_read_failed(&self) -> &'static str {
        "Failed to list the tags"
    }

    fn worktree_list_read_failed(&self) -> &'static str {
        "Failed to list the worktrees"
    }

    fn remote_list_read_failed(&self) -> &'static str {
        "Failed to list the remotes"
    }

    fn current_branch_read_failed(&self) -> &'static str {
        "Failed to read the current branch"
    }

    fn upstream_read_failed(&self, branch: &str) -> String {
        format!("Failed to read the upstream of `{branch}`")
    }

    fn ahead_behind_failed(&self, reference: &str) -> String {
        format!("Failed to count the commits against `{reference}`")
    }

    /// 次に取れる操作として示す `gz branch` はユーザーが打ち込むコマンドであり訳さない。
    fn detached_head_without_upstream(&self) -> &'static str {
        "A detached HEAD has no upstream. \
Switch to a branch with `gz branch` and run the command again"
    }

    /// コマンド列は訳さない（design.md「翻訳しないもの」）。
    fn upstream_not_configured(&self, branch: &str) -> String {
        format!(
            "`{branch}` has no upstream. \
Push it with `git push -u <remote> <branch>`, or set one with `git branch --set-upstream-to=<remote>/<branch>`"
        )
    }

    fn history_rewrite_note(&self) -> &'static str {
        "A rebase recreates the commits it replays, so their hashes change \
(take extra care if any of them have already been pushed)"
    }

    /// `command` は git のサブコマンド名であり訳さない（design.md「翻訳しないもの」）。
    fn command_run_failed(&self, command: &str) -> String {
        format!("Failed to run {command}")
    }

    fn run_summary(&self, succeeded: usize, failed: usize) -> String {
        format!("{succeeded} succeeded / {failed} failed")
    }

    fn switch_failed(&self, target: &str) -> String {
        format!("Failed to switch to `{target}`")
    }

    /// 集計との区切りの空白は、英語で節を続けるための書式であり文言の一部として持つ。
    fn failed_targets(&self, names: &str) -> String {
        format!(" (failed: {names})")
    }
}

/// [`crate::error::Error`] の英語表示。
///
/// 文言 trait ごとに ZST を分けているのは、移行の単位（trait 1 つ）と型が一致し、
/// `en.rs` が育っても実装ブロックの範囲が読み取れるようにするため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishErrorMessages;

impl ErrorMessages for EnglishErrorMessages {
    fn prefix(&self) -> &'static str {
        "error: "
    }

    /// 原因と次にとるべき操作を英語で説明する。
    ///
    /// [`crate::i18n::ja`] の日本語が伝えている原因・次の操作と同じ内容を伝える
    /// （requirements.md FR-27）。
    ///
    /// **文末に句点を置かない**。`main` がエラー連鎖を `": "` で連結するため、句点があると
    /// `#[source]` を持つバリアントで `.: ` という区切りになってしまう（日本語側も同じ理由で
    /// 文末に `。` を置いていない）。
    ///
    /// `RepositoryReadFailed` / `FilesystemReadFailed` / `GitOutputMalformed` が持つ
    /// `operation`・`detail` は [`ReadOperation`] / [`FilesystemOperation`] /
    /// [`MalformedOutput`] という**言語に依存しない値**であり、英語の文へ組み立てるのは
    /// [`read_operation_text`] / [`filesystem_operation_text`] / [`malformed_output_text`] で
    /// ある（フェーズ2で残っていた「`en` を選んでもこの 2 バリアントだけ日本語が混ざる」
    /// 制約は、この enum 化で解消済み）。
    fn describe(&self, error: &Error) -> String {
        match error {
            Error::NotARepository { .. } => {
                "Not a git repository. Run this command inside a git repository".to_owned()
            }
            Error::RepositoryReadFailed { operation, .. } => {
                format!(
                    "Failed to read repository information ({operation})",
                    operation = read_operation_text(operation)
                )
            }
            Error::GitOutputMalformed { operation, detail } => format!(
                "Failed to read repository information ({operation}): {detail}",
                operation = read_operation_text(operation),
                detail = malformed_output_text(detail)
            ),
            Error::GitCommandFailed { args, stderr } => {
                format!(
                    "The git command failed: git {args}{suffix}",
                    suffix = stderr_suffix(stderr)
                )
            }
            Error::GitRunFailed { command, code } => {
                format!("{command} {status}", status = exit_status_text(*code))
            }
            Error::GitNotFound => {
                "git was not found. Install git and make sure it is on your PATH".to_owned()
            }
            Error::GitSpawnFailed { args, .. } => {
                format!("Failed to start the git command: git {args}")
            }
            Error::UnbornHead { branch } => format!(
                "The current branch `{branch}` has no commits yet. \
                 Create the first commit with `git commit`, then run this command again"
            ),
            Error::NoWorktree => {
                "No worktree is available. This operation cannot run in a bare repository"
                    .to_owned()
            }
            Error::NoSiblingScope { workdir } => format!(
                "Cannot determine where to look for sibling repositories: \
                 the worktree root `{workdir}` has no parent directory",
                workdir = workdir.display()
            ),
            Error::FilesystemReadFailed {
                operation, path, ..
            } => format!(
                "Failed to read the filesystem ({operation}): {path}",
                operation = filesystem_operation_text(*operation),
                path = path.display()
            ),
            Error::InvalidFetchJobs { value } => format!(
                "`{value}` is not a valid number of parallel jobs for the git config \
                 `fuzgit.fetchJobs`. Set an integer of 1 or more, or remove the setting \
                 to fall back to the default"
            ),
            Error::InvalidNotify { value } => format!(
                "`{value}` is not a boolean for the git config `fuzgit.notify`. \
                 Set a git boolean such as `true` or `false`, or remove the setting \
                 to leave the notification disabled"
            ),
            Error::Cancelled => "The selection was cancelled".to_owned(),
            Error::NoCandidates => "There are no candidates to select from".to_owned(),
            Error::FinderFailed { message } => {
                format!("The fuzzy finder failed: {message}")
            }
        }
    }
}

/// 読み取り操作を英語の句へ変換する。
///
/// [`EnglishErrorMessages::describe`] の `RepositoryReadFailed` は
/// `Failed to read repository information ({operation})` という書式であるため、
/// ここが返すのは文ではなく操作を表す句である（**文末に句点を置かない**）。
///
/// `git rev-list` のようなサブコマンド名・`upstream` のような git の語彙は翻訳しない。
///
/// ワイルドカードの腕（`_ =>`）を置かない。バリアントが増えたときにコンパイルエラーで
/// 翻訳漏れへ気づくためである（[`crate::git::read::ReadOperation`] を参照）。
fn read_operation_text(operation: &ReadOperation) -> String {
    match operation {
        ReadOperation::HeadRead => "reading HEAD".to_owned(),
        ReadOperation::HeadBranchNameDecode => "decoding the branch name of HEAD".to_owned(),
        ReadOperation::HeadResolve => "resolving HEAD".to_owned(),
        ReadOperation::ReferenceList => "listing the references".to_owned(),
        ReadOperation::LocalBranchList => "listing the local branches".to_owned(),
        ReadOperation::LocalBranchRead => "reading a local branch".to_owned(),
        ReadOperation::LocalBranchNameDecode => "decoding a local branch name".to_owned(),
        ReadOperation::RemoteBranchList => "listing the remote-tracking branches".to_owned(),
        ReadOperation::RemoteBranchRead => "reading a remote-tracking branch".to_owned(),
        ReadOperation::RemoteBranchNameDecode => {
            "decoding a remote-tracking branch name".to_owned()
        }
        ReadOperation::RemoteNameDecode => "decoding a remote name".to_owned(),
        ReadOperation::BranchNameParse { branch } => {
            format!("parsing the branch name `{branch}`")
        }
        ReadOperation::RemoteUrlDecode => "decoding the remote URL".to_owned(),
        ReadOperation::UpstreamResolve { branch } => {
            format!("resolving the upstream of `{branch}`")
        }
        ReadOperation::UpstreamRefNameDecode => "decoding the upstream reference name".to_owned(),
        ReadOperation::RevListOutputParse => "parsing the git rev-list output".to_owned(),
        ReadOperation::BranchRead => "reading a branch".to_owned(),
        ReadOperation::BranchTipResolve => "resolving the branch tip".to_owned(),
        ReadOperation::BranchResolve { branch } => format!("resolving the branch `{branch}`"),
        ReadOperation::CommitHistoryWalk => "walking the commit history".to_owned(),
        ReadOperation::CommitObjectFetch => "getting the commit object".to_owned(),
        ReadOperation::ShortIdCompute => "computing the short hash".to_owned(),
        ReadOperation::CommitMessageDecode => "decoding the commit message".to_owned(),
        ReadOperation::CommitAuthorDecode => "decoding the commit author".to_owned(),
        ReadOperation::CommitTimeDecode => "decoding the commit date".to_owned(),
        ReadOperation::CommitTimeFormat => "formatting the commit date".to_owned(),
        ReadOperation::CommitSummaryDecode => "decoding the commit summary".to_owned(),
        ReadOperation::CommitAuthorNameDecode => "decoding the commit author name".to_owned(),
        ReadOperation::StatusOutputParse => "parsing the git status output".to_owned(),
        ReadOperation::PathDecode => "decoding a path".to_owned(),
        ReadOperation::RenameOriginPathDecode => "decoding the rename source path".to_owned(),
        ReadOperation::RevisionResolve { revision } => {
            format!("resolving the revision `{revision}`")
        }
        ReadOperation::TagList => "listing the tags".to_owned(),
        ReadOperation::TagRead => "reading a tag".to_owned(),
        ReadOperation::TagNameDecode => "decoding a tag name".to_owned(),
        ReadOperation::TagResolve { tag } => format!("resolving the tag `{tag}`"),
        ReadOperation::TagTargetFetch { tag } => format!("getting the target of the tag `{tag}`"),
        ReadOperation::TagParse { tag } => format!("parsing the tag `{tag}`"),
        ReadOperation::TagDecode { tag } => format!("decoding the tag `{tag}`"),
        ReadOperation::TagMessageDecode => "decoding the tag message".to_owned(),
        ReadOperation::HeadReflogRead => "reading the reflog of HEAD".to_owned(),
        ReadOperation::HeadReflogParse => "parsing the reflog of HEAD".to_owned(),
        ReadOperation::ReflogMessageDecode => "decoding a reflog message".to_owned(),
        ReadOperation::StashListOutputParse => "parsing the git stash list output".to_owned(),
        ReadOperation::StashSelectorDecode => "decoding a stash reference".to_owned(),
        ReadOperation::StashMessageDecode => "decoding a stash message".to_owned(),
        ReadOperation::MergedBranchOutputParse => {
            "parsing the git branch --merged output".to_owned()
        }
        ReadOperation::ForEachRefOutputParse => "parsing the git for-each-ref output".to_owned(),
        ReadOperation::WorktreeListOutputParse => "parsing the git worktree list output".to_owned(),
    }
}

/// `git` の出力の食い違いを英語の 1 文へ変換する。
///
/// [`crate::i18n::ja`] と同じく、受け取った値は `{:?}` で囲んで示す。
/// **文末に句点を置かない**（[`EnglishErrorMessages`] を参照）。
fn malformed_output_text(detail: &MalformedOutput) -> String {
    match detail {
        MalformedOutput::AheadBehind { output } => {
            format!("the ahead/behind format is not what was expected: {output:?}")
        }
        MalformedOutput::CommitCount { output } => {
            format!("the commit count format is not what was expected: {output:?}")
        }
        MalformedOutput::StatusEntry { record } => {
            format!("the entry format is not what was expected: {record:?}")
        }
        MalformedOutput::StatusRenameOriginMissing { path } => {
            format!("the rename source path for `{path}` is missing")
        }
        MalformedOutput::StashRecordPairing { records } => {
            format!("the records are not reference and message pairs ({records} records)")
        }
        MalformedOutput::StashSelectorFormat { selector } => {
            format!("`{selector}` is not in the `stash@{{n}}` format")
        }
        MalformedOutput::BranchActivityPair { line } => {
            format!("the line is not a reference and date pair: {line:?}")
        }
        MalformedOutput::WorktreeAttributeValueMissing { label } => {
            format!("the `{label}` attribute has no value")
        }
        MalformedOutput::WorktreeBranchReference { reference } => {
            format!("`{reference}` is not a reference name starting with `{BRANCH_REF_PREFIX}`")
        }
        MalformedOutput::WorktreeRecordStart { line } => {
            format!("the record does not start with the `{WORKTREE_LABEL}` attribute: {line:?}")
        }
        MalformedOutput::WorktreeRecordUnterminated { path } => {
            format!("the record for `{path}` is not terminated")
        }
    }
}

/// ファイルシステムの操作を英語の句へ変換する。
///
/// [`EnglishErrorMessages::describe`] の `FilesystemReadFailed` は
/// `Failed to read the filesystem ({operation}): {path}` という書式であるため、
/// ここが返すのは文ではなく操作を表す句である。
fn filesystem_operation_text(operation: FilesystemOperation) -> &'static str {
    match operation {
        FilesystemOperation::PathCanonicalization => "canonicalizing the path",
        FilesystemOperation::DirectoryScan => "scanning the directory",
    }
}

/// プロセスの終了状況を英語の述部として整形する。
///
/// シグナルで終了した場合は終了コードが得られないため、コードを取り繕わず理由の方を示す。
fn exit_status_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exited with code {code}"),
        None => "was terminated by a signal".to_owned(),
    }
}

/// 選択 UI（[`crate::finder`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishFinderMessages;

impl FinderMessages for EnglishFinderMessages {
    fn truncation_notice(&self) -> &'static str {
        "… (truncated)"
    }

    /// [`crate::i18n::ja`] と同じく、読み取れなかったパスと理由の両方を示す。
    ///
    /// **文末に句点を置かない**（`error` の連鎖と同じ理由。[`EnglishErrorMessages`] を参照）。
    fn file_read_failed(&self, path: &Path, error: &std::io::Error) -> String {
        format!("Cannot read {path}: {error}", path = path.display())
    }
}

/// 確認プロンプト（[`crate::commands::confirmation`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishConfirmMessages;

impl ConfirmMessages for EnglishConfirmMessages {
    /// 承認とみなす応答は言語に依らず `y` / `yes` で固定であるため、`[y/N]` は訳さない。
    fn prompt(&self) -> &'static str {
        "Proceed? [y/N]: "
    }

    fn cancelled(&self) -> &'static str {
        "Aborted"
    }

    fn output_failed(&self) -> &'static str {
        "Failed to write the confirmation prompt"
    }

    fn input_failed(&self) -> &'static str {
        "Failed to read the confirmation input"
    }
}

/// `gz branch`（[`crate::commands::branch`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishBranchMessages;

impl BranchMessages for EnglishBranchMessages {
    fn header_subject(&self) -> &'static str {
        "Pick the branch to switch to"
    }

    fn header_outcome(&self) -> &'static str {
        "switch to it"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected branch `{selected}` is not among the candidates")
    }

    fn tracking_target_undetermined(&self, name: &str) -> String {
        format!("Cannot determine which branch to track from the remote-tracking branch `{name}`")
    }
}

/// `gz branch create` / `delete` / `cleanup`（[`crate::commands::branch_manage`]）の
/// 英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishBranchManageMessages;

impl BranchManageMessages for EnglishBranchManageMessages {
    fn base_header_subject(&self) -> &'static str {
        "Pick what the new branch starts from"
    }

    fn base_header_outcome_switch(&self) -> &'static str {
        "create the branch and switch to it"
    }

    fn base_header_outcome_stay(&self) -> &'static str {
        "create the branch without switching"
    }

    fn base_selection_not_found(&self, selected: &str) -> String {
        format!("The selected base `{selected}` is not among the candidates")
    }

    fn creation_failed(&self, name: &str) -> String {
        format!("Failed to create the branch `{name}`")
    }

    /// `base` は候補の finder キー（`branch main` / `tag v1.0`）であり訳さない。
    fn created(&self, name: &str, base: &str) -> String {
        format!("Created the branch `{name}` from {base}")
    }

    /// コマンド列（`git switch`）は訳さない（design.md「翻訳しないもの」）。
    fn switch_hint(&self, name: &str) -> String {
        format!("Run `git switch {name}` to switch to it")
    }

    fn tracking(&self, upstream: &str) -> String {
        format!("tracking: {upstream}")
    }

    fn protected(&self) -> &'static str {
        "trunk, not preselected"
    }

    fn no_tracking(&self) -> &'static str {
        "no tracking branch"
    }

    fn unknown_date(&self) -> &'static str {
        "date unknown"
    }

    fn merged_state_read_failed(&self) -> &'static str {
        "Failed to determine which branches are merged"
    }

    fn activity_read_failed(&self) -> &'static str {
        "Failed to read when the branches were last updated"
    }

    /// オプション名（`--into`）は訳さない。
    fn unknown_merge_base(&self, into: &str) -> String {
        format!("The branch `{into}` given to `--into` was not found")
    }

    /// `names` の件数に依存しない言い回しにする（[`EnglishCherryPickMessages`] と同じ理由）。
    fn selection_not_found(&self, names: &str) -> String {
        format!("Selected branches not found among the candidates: {names}")
    }

    fn deletion_failed(&self) -> &'static str {
        "Failed to delete the branches"
    }

    /// キー名（`Tab` / `Enter`）は skim のキー表記であり訳さない。
    fn delete_header(&self) -> &'static str {
        "Tab: toggle the selection / Enter: delete the selected branches"
    }

    fn no_delete_candidates(&self) -> &'static str {
        "There is no branch that can be deleted \
(the current branch and the branches checked out in a worktree are not offered)"
    }

    /// 件数に依存しない言い回しにする。コマンド列（`gz branch delete --force`）は訳さない。
    fn unmerged_rejection(&self, names: &str) -> String {
        format!(
            "The selection contains branches that are not merged (unmerged): {names}\n\
             Deleting them can lose the commits that exist only on those branches. \
             Run `gz branch delete --force` if you want to delete them anyway"
        )
    }

    fn delete_confirmation(&self) -> &'static str {
        "The following branches will be deleted (a deleted branch cannot be restored)"
    }

    fn unmerged_confirmation(&self, names: &str) -> String {
        format!(
            "{base}\n\
             Warning: branches that are not merged (unmerged) are included: {names}\n\
             The commits that exist only on those branches will be lost",
            base = self.delete_confirmation()
        )
    }

    /// キー名（`Tab` / `Enter`）は skim のキー表記であり訳さない。
    fn cleanup_header(&self) -> &'static str {
        "Every branch is preselected. Tab: unselect a branch to keep / Enter: delete"
    }

    /// `merged` は git の語彙であり訳さない。
    fn no_cleanup_candidates(&self) -> &'static str {
        "There is no merged branch"
    }
}

/// `gz cherry-pick`（[`crate::commands::cherry_pick`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishCherryPickMessages;

impl CherryPickMessages for EnglishCherryPickMessages {
    /// `hashes` は 1 件のことも複数件のこともあるため、**数に依存しない**言い回しにする
    /// （日本語側が単複を区別しないのと同じ表現範囲を保つ）。
    fn selection_not_found(&self, hashes: &str) -> String {
        format!("Selected commits not found among the candidates: {hashes}")
    }

    /// 案内に含まれるコマンド列は訳さない（design.md「翻訳しないもの」）。
    fn resolution_hint(&self) -> &'static str {
        "The cherry-pick failed. \
         Run `git cherry-pick --continue` once the conflicts are resolved, \
         or `git cherry-pick --abort` to cancel it"
    }
}

/// ファイル選択（[`crate::commands::file_selection`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishFileSelectionMessages;

impl FileSelectionMessages for EnglishFileSelectionMessages {
    /// `paths` の件数に依存しない言い回しにする（[`EnglishCherryPickMessages`] と同じ理由）。
    fn selection_not_found(&self, paths: &str) -> String {
        format!("Selected files not found among the candidates: {paths}")
    }
}

/// `gz reflog`（[`crate::commands::reflog`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishReflogMessages;

impl ReflogMessages for EnglishReflogMessages {
    /// `reflog` は git の語彙であり訳さない。
    fn header_subject(&self) -> &'static str {
        "Pick a reflog entry"
    }

    fn header_outcome_print(&self) -> &'static str {
        "print its full hash"
    }

    fn header_outcome_menu(&self) -> &'static str {
        "pick what to do next"
    }

    fn conflicting_actions(&self) -> &'static str {
        "--restore and --action cannot be combined"
    }

    fn header_outcome_restore(&self, name: &str) -> String {
        format!("create the branch `{name}` at that commit")
    }

    fn read_failed(&self) -> &'static str {
        "Failed to read the reflog"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected reflog entry `{selected}` is not among the candidates")
    }

    fn branch_creation_failed(&self, name: &str) -> String {
        format!("Failed to create the branch `{name}`")
    }

    fn branch_created(&self, name: &str, id: &str) -> String {
        format!("Created the branch `{name}` at {id}")
    }
}

/// `gz restore`（[`crate::commands::restore`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishRestoreMessages;

impl RestoreMessages for EnglishRestoreMessages {
    fn revision_files_read_failed(&self, revision: &str) -> String {
        format!("Failed to list the files in `{revision}`")
    }

    fn discard_confirmation(&self, count: usize) -> String {
        format!(
            "The changes in the following {count} {noun} will be discarded \
             (this cannot be undone):",
            noun = files(count)
        )
    }

    fn overwrite_confirmation(&self, count: usize, revision: &str) -> String {
        format!(
            "The following {count} {noun} will be overwritten with the contents of `{revision}` \
             (changes in the working tree will be lost):",
            noun = files(count)
        )
    }
}

/// 件数に合わせて `file` / `files` を選ぶ。
///
/// 日本語は単複を区別しないため文言は 1 つで足りるが、英語で `1 files` と出すと
/// 機械翻訳のような印象を与えるため、確認プロンプトのように件数が前面に出る文言では
/// 数を合わせる。
fn files(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

/// `gz revert`（[`crate::commands::revert`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishRevertMessages;

impl RevertMessages for EnglishRevertMessages {
    /// `hashes` の件数に依存しない言い回しにする（[`EnglishCherryPickMessages`] と同じ理由）。
    fn selection_not_found(&self, hashes: &str) -> String {
        format!("Selected commits not found among the candidates: {hashes}")
    }

    /// 案内に含まれるコマンド列は訳さない（design.md「翻訳しないもの」）。
    fn resolution_hint(&self) -> &'static str {
        "The revert failed. \
         Run `git revert --continue` once the conflicts are resolved, \
         or `git revert --abort` to cancel it"
    }

    /// 対象のコミットと実行するコマンド列を呼び出し側が続けて並べるため、末尾は `:` で終える。
    fn merge_commit_selected(&self) -> &'static str {
        "The selection contains a merge commit. Reverting a merge needs the number of the \
         parent to undo (`-m <parent-number>`), which fuzgit does not support. \
         Run plain git yourself:"
    }
}

/// `gz stash`（[`crate::commands::stash`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishStashMessages;

impl StashMessages for EnglishStashMessages {
    /// `stash` は git の語彙であり訳さない。
    fn header_subject(&self) -> &'static str {
        "Pick a stash"
    }

    fn header_outcome_apply(&self) -> &'static str {
        "apply it and keep the stash"
    }

    fn header_outcome_pop(&self) -> &'static str {
        "apply it and drop the stash"
    }

    fn header_outcome_drop(&self) -> &'static str {
        "drop it after a confirmation"
    }

    fn drop_confirmation(&self) -> &'static str {
        "The following stash will be dropped (this cannot be undone):"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected stash `{selected}` is not among the candidates")
    }
}

/// `gz commit`（[`crate::commands::commit`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishCommitMessages;

impl CommitMessages for EnglishCommitMessages {
    /// キー名（`Tab` / `Enter`）は端末が送るキーの名前であり訳さない。
    fn header(&self) -> &'static str {
        "Tab: toggle the selection / Enter: commit (staged files are preselected)"
    }

    /// 案内の 2 行目以降は行頭の字下げも表示の一部であるため、字下げごと文言に含める。
    /// コマンド列・環境変数名は訳さない（design.md「翻訳しないもの」）。
    fn editor_hint(&self) -> &'static str {
        "Hint: if the commit message is not saved in the editor, \
git treats the message as empty and aborts the commit.
  - `gz commit -m \"<message>\"` sets the message without going through an editor.
  - If EDITOR / GIT_EDITOR points at a GUI editor, it needs the option that waits for \
the editor to exit (`code --wait`, for example). Without it the editor returns \
immediately and the message is treated as empty"
    }

    fn untracked_stage_failed(&self) -> &'static str {
        "Failed to stage the untracked files (git add)"
    }
}

/// `gz fixup`（[`crate::commands::fixup`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishFixupMessages;

impl FixupMessages for EnglishFixupMessages {
    fn header_subject(&self) -> &'static str {
        "Pick the commit to correct"
    }

    /// `label`（`fixup` / `squash`）は作成されるコミットの接頭辞と同じ綴りであり訳さない。
    fn header_outcome(&self, label: &str) -> String {
        format!("create the {label} commit")
    }

    fn staged_changes_read_failed(&self) -> &'static str {
        "Failed to read the staged changes"
    }

    /// `label`（`fixup` / `squash`）は作成されるコミットの接頭辞と同じ綴りであり訳さない。
    fn staged_required(&self, label: &str) -> String {
        format!(
            "There are no staged changes. A {label} commit needs staged changes, \
             so stage them with `gz add` and run this command again"
        )
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected commit `{selected}` is not among the candidates")
    }

    fn commit_creation_failed(&self, label: &str) -> String {
        format!("Failed to create the {label} commit")
    }

    /// コマンド列を続けて示すため、末尾は `:` で終える。
    fn autosquash_hint(&self, start: &str) -> String {
        format!(
            "Hint: run the following to fold the new commit into the history:\n  \
             git rebase -i --autosquash {start}"
        )
    }

    fn root_start_note(&self) -> &'static str {
        "(the selected commit has no parent (it is the first commit), \
         so the rebase starts from `--root` instead of `<hash>^`)"
    }
}

/// `gz status`（[`crate::commands::status`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishStatusMessages;

impl StatusMessages for EnglishStatusMessages {
    fn clean(&self) -> &'static str {
        "There are no changes (the working tree is clean)"
    }

    /// 括弧内の git コマンド名は、実際に何が実行されるのかを示すため訳さない。
    fn add_action(&self) -> &'static str {
        "Stage the selected files (git add)"
    }

    fn restore_action(&self) -> &'static str {
        "Discard the changes in the selected files (git restore)"
    }

    fn stash_action(&self) -> &'static str {
        "Stash the selected files (git stash push)"
    }

    fn commit_action(&self) -> &'static str {
        "Commit the selected files (git commit)"
    }

    fn print_action(&self) -> &'static str {
        "Print the paths of the selected files to standard output"
    }

    fn menu_selection_not_found(&self, selected: &str) -> String {
        format!("The selected menu entry `{selected}` is not in the menu")
    }
}

/// `gz merge`（[`crate::commands::merge`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishMergeMessages;

impl MergeMessages for EnglishMergeMessages {
    /// `merge` は git のサブコマンド名であり訳さない。
    fn header_subject(&self) -> &'static str {
        "Pick the branch to merge"
    }

    fn header_outcome(&self) -> &'static str {
        "run the merge"
    }

    /// オプション名は訳さない（design.md「翻訳しないもの」）。
    fn conflicting_modes(&self) -> &'static str {
        "`--no-ff`, `--squash` and `--ff-only` cannot be given at the same time"
    }

    fn no_candidates(&self) -> &'static str {
        "There is no branch to merge \
(a local or remote-tracking branch other than the current one is needed)"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected branch `{selected}` is not among the candidates")
    }

    fn merged_commit_count_failed(&self, branch: &str) -> String {
        format!("Failed to count the commits that `{branch}` would bring in")
    }

    fn merge_failed(&self, branch: &str) -> String {
        format!("Failed to merge `{branch}`")
    }

    fn confirmation(&self, branch: &str, count: usize) -> String {
        format!(
            "`{branch}` will be merged ({count} {noun} will be brought in)",
            noun = commits(count)
        )
    }

    /// コマンド列は訳さない（design.md「翻訳しないもの」）。
    fn command_line(&self, command: &str) -> String {
        format!("Command to run: {command}")
    }

    fn prediction_clean(&self) -> &'static str {
        "Conflict prediction: the merge is expected to be conflict-free"
    }

    fn prediction_conflicted(&self, count: usize) -> String {
        format!(
            "Conflict prediction: conflicts are expected in the following {count} {noun}",
            noun = files(count)
        )
    }

    fn prediction_unnamed(&self) -> &'static str {
        "Conflict prediction: conflicts are expected \
(the names of the files could not be obtained)"
    }

    /// コマンド列と Git のバージョンは原因を確かめるための情報であり訳さない。
    fn prediction_unavailable(&self) -> &'static str {
        "Conflict prediction: skipped \
(`git merge-tree --write-tree` could not be run; the prediction needs Git 2.38 or later)"
    }
}

/// 件数に合わせて `commit` / `commits` を選ぶ。
///
/// 単複を合わせる理由は [`files`] と同じ。
fn commits(count: usize) -> &'static str {
    if count == 1 { "commit" } else { "commits" }
}

/// `gz rebase`（[`crate::commands::rebase`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishRebaseMessages;

impl RebaseMessages for EnglishRebaseMessages {
    /// `rebase` は git のサブコマンド名であり訳さない。
    fn header_subject(&self) -> &'static str {
        "Pick the branch to rebase onto"
    }

    fn header_outcome(&self) -> &'static str {
        "run the rebase"
    }

    fn no_candidates(&self) -> &'static str {
        "There is no branch to rebase onto \
(a local or remote-tracking branch other than the current one is needed)"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected branch `{selected}` is not among the candidates")
    }

    fn replayed_commit_count_failed(&self, base: &str) -> String {
        format!("Failed to count the commits that would be replayed onto `{base}`")
    }

    fn rebase_failed(&self, base: &str) -> String {
        format!("Failed to rebase onto `{base}`")
    }

    fn confirmation(&self, base: &str, count: usize) -> String {
        format!(
            "{count} {noun} will be replayed onto `{base}`",
            noun = commits(count)
        )
    }
}

/// merge / rebase の復帰メニュー（[`crate::commands::in_progress`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishInProgressMessages;

impl InProgressMessages for EnglishInProgressMessages {
    /// 括弧内の git コマンド名は、実際に何が実行されるのかを示すため訳さない。
    fn conflicts_action(&self) -> &'static str {
        "Review the conflicted files and mark them as resolved (git add)"
    }

    /// `operation`（`merge` / `rebase`）は git のサブコマンド名と同じ綴りであり訳さない。
    fn continue_action(&self, operation: &str) -> String {
        format!("Resume the {operation}")
    }

    fn skip_action(&self) -> &'static str {
        "Skip the current commit"
    }

    fn abort_action(&self, operation: &str) -> String {
        format!("Abort the {operation}")
    }

    fn menu_selection_not_found(&self, selected: &str) -> String {
        format!("The selected menu entry `{selected}` is not in the menu")
    }

    fn abort_confirmation(&self, operation: &str) -> String {
        format!(
            "Aborting the {operation} discards every conflict resolution made so far \
             (including the staged ones) and returns to the state before the {operation} started"
        )
    }

    /// キー名（`Tab` / `Enter`）は端末が送るキーの名前であり訳さない。
    fn conflicts_header(&self) -> &'static str {
        "Tab: toggle the selection / Enter: stage the selected files as resolved"
    }

    fn conflicts_read_failed(&self) -> &'static str {
        "Failed to list the conflicted files"
    }

    /// 次に取れる操作として示す `continue` はメニュー項目の呼称であり訳さない。
    fn no_conflicts(&self) -> &'static str {
        "There is no conflicted file. \
         If everything is resolved, resume with continue in the menu"
    }

    fn stage_failed(&self) -> &'static str {
        "Failed to stage the files as resolved (git add)"
    }
}

/// `gz worktree`（[`crate::commands::worktree`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishWorktreeMessages;

impl WorktreeMessages for EnglishWorktreeMessages {
    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected worktree `{selected}` is not among the candidates")
    }

    fn path_not_utf8(&self, path: &Path) -> String {
        format!(
            "The worktree path `{path}` cannot be read as UTF-8",
            path = path.display()
        )
    }

    fn no_available_branch(&self) -> &'static str {
        "There is no local branch left to put in a worktree \
(a branch checked out in another worktree is not offered)"
    }

    fn branch_selection_not_found(&self, selected: &str) -> String {
        format!("The selected branch `{selected}` is not among the candidates")
    }

    fn creation_failed(&self, path: &str) -> String {
        format!("Failed to create the worktree `{path}`")
    }

    /// `git worktree remove` はユーザーが打ち込むコマンドであり訳さない。
    fn no_removable(&self) -> &'static str {
        "There is no worktree to remove \
(the main worktree is never a target of `git worktree remove`)"
    }

    fn remove_confirmation(&self) -> &'static str {
        "The following worktree will be removed \
(its directory and administrative files are deleted; the branch itself stays)"
    }

    fn removal_failed(&self, path: &str) -> String {
        format!("Failed to remove the worktree `{path}`")
    }

    fn prune_targets_read_failed(&self) -> &'static str {
        "Failed to check what would be pruned"
    }

    fn prune_confirmation(&self) -> &'static str {
        "The administrative files of the following worktrees will be pruned"
    }

    fn prune_failed(&self) -> &'static str {
        "Failed to prune the worktrees"
    }

    fn nothing_to_prune(&self) -> &'static str {
        "There is no worktree to prune"
    }

    fn install_running(&self, directory: &Path, command: &str) -> String {
        format!(
            "Running `{command}` in {directory}",
            directory = directory.display()
        )
    }

    /// エコシステム名・lockfile 名は固有名詞であり訳さない。
    fn install_ambiguous(&self, ecosystem: &str, lockfiles: &str) -> String {
        format!(
            "Skipping the {ecosystem} dependencies: more than one lockfile is present ({lockfiles})"
        )
    }

    /// オプション名は訳さない（design.md「翻訳しないもの」）。
    fn install_flavour_unknown(&self, lockfile: &str) -> String {
        format!(
            "Skipping the dependencies: the Yarn version cannot be told from {lockfile} \
(`--immutable` is Yarn 2 and later, `--frozen-lockfile` is Yarn 1)"
        )
    }

    fn install_command_missing(&self, program: &str) -> String {
        format!("Skipped the dependencies: `{program}` was not found")
    }

    fn agent_config_copied(&self, copied: usize, skipped: usize) -> String {
        if skipped == 0 {
            format!("Copied {copied} file(s) into .claude/")
        } else {
            format!(
                "Copied {copied} file(s) into .claude/ ({skipped} already there were left alone)"
            )
        }
    }

    fn agent_config_copy_failed(&self, path: &str) -> String {
        format!("warning: failed to copy .claude/: {path}")
    }

    fn install_failed(&self, command: &str) -> String {
        format!(
            "Failed to install the dependencies \
(the worktree was created; run `{command}` again to retry)"
        )
    }

    /// `git worktree list` はユーザーが打ち込むコマンドであり訳さない。
    fn name_is_not_a_directory_name(&self, name: &str) -> String {
        format!(
            "a worktree name cannot contain a path separator: {name} (worktrees are always created next to the repository, so pass a name only)"
        )
    }

    fn no_parent_directory(&self, root: &str) -> String {
        format!("{root} has no parent directory, so a sibling worktree cannot be created")
    }

    fn main_worktree_not_found(&self) -> &'static str {
        "the main work tree could not be identified, so there is nowhere to create the worktree"
    }

    fn created_at(&self, path: &str) -> String {
        format!("Created the worktree at {path}")
    }

    fn install_subdirectory_missing(&self, relative: &str) -> String {
        format!("warning: the new worktree has no {relative}, so nothing is installed")
    }

    fn install_directory_not_found(&self, path: &str) -> String {
        format!(
            "Skipped the dependencies: the new worktree `{path}` is not listed by \
`git worktree list`"
        )
    }
}

/// `gz sync`（[`crate::commands::sync`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishSyncMessages;

impl SyncMessages for EnglishSyncMessages {
    /// オプション名は訳さない（design.md「翻訳しないもの」）。
    fn conflicting_modes(&self) -> &'static str {
        "`--rebase` and `--merge` cannot be given at the same time"
    }

    fn fetch_failed(&self, remote: &str) -> String {
        format!("Failed to fetch from the remote `{remote}`")
    }

    /// コマンド列は訳さない（design.md「翻訳しないもの」）。
    fn tracking_ref_unavailable(&self, branch: &str, remote: &str, merge_ref: &str) -> String {
        format!(
            "The upstream of `{branch}` ({remote} / {merge_ref}) does not yield \
a remote-tracking reference. \
Set it again with `git branch --set-upstream-to=<remote>/<branch>`"
        )
    }

    /// プレースホルダ（`<name>` / `<url>`）は読み手のための語であり表示言語に合わせる。
    fn unknown_remote(&self, branch: &str, remote: &str) -> String {
        format!(
            "`{remote}`, the upstream remote of `{branch}`, is not a registered remote. \
Register it with `git remote add <name> <url>`, \
or set the upstream again with `git branch --set-upstream-to=<remote>/<branch>`"
        )
    }

    fn missing_tracking_ref(&self, reference: &str, remote: &str, branch: &str) -> String {
        format!(
            "The remote-tracking reference `{reference}` does not exist. \
`{remote}` may have no upstream for `{branch}` \
(create it with `git push -u <remote> <branch>`, \
or set the upstream again with `git branch --set-upstream-to=<remote>/<branch>`)"
        )
    }

    fn up_to_date(&self, branch: &str, reference: &str) -> String {
        format!("`{branch}` is up to date (there is nothing to bring in from `{reference}`)")
    }

    fn unpushed_commits(&self, count: usize) -> String {
        format!(
            "{count} {noun} {verb} not been pushed yet (`git push` pushes them)",
            noun = commits(count),
            verb = if count == 1 { "has" } else { "have" }
        )
    }

    fn integration_failed(&self, reference: &str) -> String {
        format!("Failed to bring in `{reference}`")
    }

    fn confirmation(&self, reference: &str, count: usize, branch: &str) -> String {
        format!(
            "{count} {noun} from `{reference}` will be brought into `{branch}`",
            noun = commits(count)
        )
    }
}

/// `gz diff`（[`crate::commands::diff`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishDiffMessages;

impl DiffMessages for EnglishDiffMessages {
    /// オプション名は訳さない（design.md「翻訳しないもの」）。
    fn conflicting_modes(&self) -> &'static str {
        "`--staged`, `--head`, `--upstream`, `--branch` and `--commit` \
cannot be given at the same time"
    }

    /// `index` は git の語彙であり訳さない（`staged_range` / `head_range` も同じ）。
    fn unstaged_range(&self) -> &'static str {
        "Unstaged changes: index and work tree"
    }

    fn staged_range(&self) -> &'static str {
        "Staged changes: HEAD and index"
    }

    fn head_range(&self) -> &'static str {
        "HEAD and work tree"
    }

    /// 進捗を示す `1/2` / `2/2` は数値であり訳さない。
    fn base_branch_header(&self) -> &'static str {
        "1/2 branch to compare from"
    }

    fn target_branch_header(&self) -> &'static str {
        "2/2 branch to compare to"
    }

    fn base_commit_header(&self) -> &'static str {
        "1/2 commit to compare from"
    }

    fn target_commit_header(&self) -> &'static str {
        "2/2 commit to compare to"
    }

    /// コマンド列は訳さない（design.md「翻訳しないもの」）。
    fn tracking_ref_unavailable(&self, branch: &str, remote: &str, merge_ref: &str) -> String {
        format!(
            "The upstream of `{branch}` ({remote} / {merge_ref}) does not yield \
a remote-tracking reference. \
Pick the revisions with `gz diff --branch`, or use plain `git diff`"
        )
    }

    fn files_read_failed(&self, description: &str) -> String {
        format!("Failed to list the changed files ({description})")
    }

    fn branch_selection_not_found(&self, selected: &str) -> String {
        format!("The selected branch `{selected}` is not among the candidates")
    }

    fn commit_selection_not_found(&self, selected: &str) -> String {
        format!("The selected commit `{selected}` is not among the candidates")
    }

    fn no_diff(&self, description: &str) -> String {
        format!("There is no difference ({description})")
    }

    fn diff_failed(&self, description: &str) -> String {
        format!("Failed to show the diff ({description})")
    }
}

/// `gz fetch`（[`crate::commands::fetch`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishFetchMessages;

impl FetchMessages for EnglishFetchMessages {
    fn remote_header_subject(&self) -> &'static str {
        "Pick the remote to fetch from"
    }

    fn remote_header_outcome(&self) -> &'static str {
        "fetch"
    }

    fn all_remotes_label(&self) -> &'static str {
        "all remotes"
    }

    fn remote_description(&self, remote: &str) -> String {
        format!("the remote `{remote}`")
    }

    /// プレースホルダ（`<name>` / `<url>`）は読み手のための語であり表示言語に合わせる。
    fn no_remotes(&self) -> &'static str {
        "No remote is configured to fetch from. Add one with `git remote add <name> <url>`"
    }

    fn fixed_target(&self, target: &str) -> String {
        format!(
            "Fetching from {target} (only one remote is registered, so the selection was skipped)"
        )
    }

    fn fetch_failed(&self, target: &str) -> String {
        format!("Failed to fetch from {target}")
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The selected remote `{selected}` is not among the candidates")
    }

    fn url_section(&self) -> &'static str {
        "Remote URL"
    }

    fn tracking_section(&self) -> &'static str {
        "Known remote-tracking branches"
    }

    fn sibling_scan_failed(&self) -> &'static str {
        "Failed to look for the sibling repositories"
    }

    fn no_sibling_candidates(&self) -> &'static str {
        "There is no repository to fetch \
(a repository without a remote and a bare repository are not offered)"
    }

    fn single_sibling_reason(&self) -> &'static str {
        "The current repository is the only target, so the selection was skipped"
    }

    /// キー名（`Tab` / `Enter`）は skim のキー表記であり訳さない。
    fn siblings_header(&self) -> &'static str {
        "The current repository is preselected. Tab: toggle the selection / Enter: fetch"
    }

    fn excluded_count(&self, count: usize) -> String {
        format!("{count} excluded (no remote / bare)")
    }

    /// オプション名は訳さない（design.md「翻訳しないもの」）。
    fn prune_scope_note(&self) -> &'static str {
        "--prune: applies to every selected repository"
    }

    fn tracking_state_section(&self) -> &'static str {
        "Branch tracking status"
    }

    fn path_not_utf8(&self, path: &Path) -> String {
        format!(
            "The work tree path cannot be handled as text: {path}",
            path = path.display()
        )
    }

    fn sibling_selection_not_found(&self, paths: &str) -> String {
        format!("The selected repositories {paths} are not among the candidates")
    }

    fn sibling_start_failed(&self, name: &str) -> String {
        format!("Failed to start fetching `{name}`")
    }

    fn serial_fallback(&self, count: usize) -> String {
        format!(
            "Running the {count} repositories that failed in parallel again, one at a time, so they can ask for credentials"
        )
    }

    fn partial_failure(&self) -> &'static str {
        "Fetching failed for some repositories"
    }

    fn notification_title(&self) -> &'static str {
        // 通知の本文は件数だけであるため、どのコマンドが終わったのかはここで示す
        "gz fetch --siblings finished"
    }
}

/// `gz pull`（[`crate::commands::pull`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishPullMessages;

impl PullMessages for EnglishPullMessages {
    fn targets_read_failed(&self) -> &'static str {
        "Failed to collect the branches that could follow an upstream"
    }

    /// コマンド列（`git push -u` / `git branch --set-upstream-to`）は訳さない
    /// （design.md「翻訳しないもの」）。
    fn no_candidates(&self) -> &'static str {
        "There is no branch that can follow an upstream \
(only a local branch with an upstream on a registered remote is offered). \
Push with `git push -u <remote> <branch>`, or set one with `git branch --set-upstream-to=<remote>/<branch>`"
    }

    /// キー名（`Tab` / `Enter`）は skim のキー表記であり、`fast-forward` は git の語彙で
    /// あるためどちらも訳さない。
    fn header(&self) -> &'static str {
        "The current branch is preselected. Tab: toggle the selection / Enter: integrate with a fast-forward only"
    }

    fn excluded_count(&self, count: usize) -> String {
        format!(
            "{count} excluded (no upstream / remote not registered / in use by another worktree)"
        )
    }

    /// [`EnglishPullMessages::fixed_target`] の括弧の中へ入る節であるため小文字で始める。
    fn single_target_reason(&self) -> &'static str {
        "there is only one candidate, so the selection was skipped"
    }

    fn fixed_target(&self, branch: &str) -> String {
        format!(
            "Integrating `{branch}` ({reason})",
            reason = self.single_target_reason()
        )
    }

    fn unmerged_section(&self) -> &'static str {
        "Commits not integrated yet (as of the last fetch)"
    }

    fn selection_not_found(&self, branches: &str) -> String {
        format!("The selected branches {branches} are not among the candidates")
    }

    fn skipped(&self, remote: &str, branch: &str) -> String {
        format!("Skipped integrating `{branch}` because fetching from `{remote}` failed")
    }

    fn fetch_start_failed(&self, remote: &str) -> String {
        format!("Failed to start fetching from the remote `{remote}`")
    }

    fn integration_start_failed(&self, branch: &str) -> String {
        format!("Failed to start integrating `{branch}`")
    }

    /// コマンド列（`gz sync --rebase` / `gz sync --merge`）は訳さない。
    fn fast_forward_guidance(&self) -> &'static str {
        "A branch that could not fast-forward can be integrated with \
`gz sync --rebase` or `gz sync --merge` after switching to it"
    }

    fn partial_failure(&self) -> &'static str {
        "Integrating failed for some branches"
    }

    fn notification_title(&self) -> &'static str {
        "gz pull finished"
    }
}

/// `gz log`（[`crate::commands::log`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishLogMessages;

impl LogMessages for EnglishLogMessages {
    fn header_subject(&self) -> &'static str {
        "Pick a commit"
    }

    fn header_outcome_print(&self) -> &'static str {
        "print its full hash"
    }

    fn header_outcome_menu(&self) -> &'static str {
        "pick what to do next"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("The picked commit {selected} was not found among the candidates")
    }
}

/// コミット選択後のアクションメニュー（[`CommitMenuMessages`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishCommitMenuMessages;

impl CommitMenuMessages for EnglishCommitMenuMessages {
    fn subject(&self, short_id: &str) -> String {
        format!("Pick what to do with commit {short_id}")
    }

    fn outcome(&self) -> &'static str {
        "run the picked action"
    }

    fn show_action(&self) -> &'static str {
        "Show the commit (git show)"
    }

    fn switch_action(&self) -> &'static str {
        "Switch to it as a detached HEAD (git switch --detach)"
    }

    fn cherry_pick_action(&self) -> &'static str {
        "Apply it onto the current branch (git cherry-pick)"
    }

    fn revert_action(&self) -> &'static str {
        "Create a commit that undoes it (git revert)"
    }

    fn fixup_action(&self) -> &'static str {
        "Create a fixup commit from the staged changes (git commit --fixup)"
    }

    fn reset_action(&self) -> &'static str {
        "Move the current branch back to it (git reset --hard)"
    }

    fn print_action(&self) -> &'static str {
        "Print the full hash to stdout"
    }

    fn menu_selection_not_found(&self, selected: &str) -> String {
        format!("The picked menu entry {selected} was not found")
    }

    fn reset_confirmation(&self) -> &'static str {
        "This discards every uncommitted change in the working tree and the index, and moves the current branch to the commit below (the commit you leave stays in the reflog)"
    }

    fn show_failed(&self, id: &str) -> String {
        format!("Failed to show commit {id}")
    }

    fn switch_failed(&self, id: &str) -> String {
        format!("Failed to switch to commit {id}")
    }

    fn reset_failed(&self, id: &str) -> String {
        format!("Failed to reset to commit {id}")
    }
}

/// `clap` のヘルプ（[`CliMessages`]）の英語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnglishCliMessages;

impl CliMessages for EnglishCliMessages {
    // `gz` 自身

    fn about(&self) -> &'static str {
        "A git CLI that lets you pick, search, and trace with a fuzzy finder"
    }

    fn lang_help(&self) -> &'static str {
        "Display language (falls back to FUZGIT_LANG / git config fuzgit.lang / the locale)"
    }

    // `gz branch`（切替と管理サブコマンド）

    fn branch_about(&self) -> &'static str {
        "Pick a branch and switch to it (subcommands also create, delete and tidy up)"
    }

    fn branch_all_help(&self) -> &'static str {
        "Include remote-tracking branches in the candidates"
    }

    fn branch_create_about(&self) -> &'static str {
        "Pick a starting point, create a new branch and switch to it"
    }

    fn branch_create_name_help(&self) -> &'static str {
        "Name of the branch to create"
    }

    fn branch_create_no_switch_help(&self) -> &'static str {
        "Create the branch without switching to it"
    }

    fn branch_delete_about(&self) -> &'static str {
        "Pick a branch and delete it"
    }

    fn branch_delete_force_help(&self) -> &'static str {
        "Delete a branch even when it is not merged (`git branch -D`)"
    }

    fn branch_delete_into_help(&self) -> &'static str {
        "Branch that `merged` is judged against (defaults to HEAD)"
    }

    fn branch_cleanup_about(&self) -> &'static str {
        "Delete every merged branch at once"
    }

    fn branch_cleanup_into_help(&self) -> &'static str {
        "Branch that `merged` is judged against (defaults to HEAD)"
    }

    // `gz log` / `gz cherry-pick` / `gz restore` / `gz add`

    fn log_about(&self) -> &'static str {
        "Trace the commit history and print the full hash of the picked commit"
    }

    fn log_limit_help(&self) -> &'static str {
        "Maximum number of commits to read"
    }

    fn log_action_help(&self) -> &'static str {
        "Pick what to do with the chosen commit from a menu"
    }

    fn cherry_pick_about(&self) -> &'static str {
        "Pick a commit and cherry-pick it"
    }

    fn cherry_pick_branch_help(&self) -> &'static str {
        "Target branch (without it, the commits of every branch are offered)"
    }

    fn restore_about(&self) -> &'static str {
        "Pick files and restore them with git restore"
    }

    fn restore_source_help(&self) -> &'static str {
        "Revision to restore from"
    }

    fn restore_staged_help(&self) -> &'static str {
        "Unstage the staged changes"
    }

    fn add_about(&self) -> &'static str {
        "Pick unstaged and untracked files and stage them with git add"
    }

    // `gz stash`

    fn stash_about(&self) -> &'static str {
        "Stash changes away, then search the stashes to apply or drop them"
    }

    fn stash_push_about(&self) -> &'static str {
        "Pick changed files and stash them away"
    }

    fn stash_push_message_help(&self) -> &'static str {
        "Message to attach to the stash"
    }

    fn stash_push_include_untracked_help(&self) -> &'static str {
        "Include untracked files in the candidates (tracked changes only by default)"
    }

    fn stash_apply_about(&self) -> &'static str {
        "Pick a stash and apply it (the stash is kept)"
    }

    fn stash_pop_about(&self) -> &'static str {
        "Pick a stash, apply it and drop that stash"
    }

    fn stash_drop_about(&self) -> &'static str {
        "Pick a stash and drop it"
    }

    // `gz reflog`

    fn reflog_about(&self) -> &'static str {
        "Trace the reflog of HEAD and print the hash of the picked commit"
    }

    fn reflog_restore_help(&self) -> &'static str {
        "Create a new branch with the given name from the picked commit"
    }

    fn reflog_action_help(&self) -> &'static str {
        "Pick what to do with the chosen commit from a menu"
    }

    // `gz commit` / `gz fixup`

    fn commit_about(&self) -> &'static str {
        "Pick the files to commit and commit them"
    }

    fn commit_message_help(&self) -> &'static str {
        "Commit message (without it, git starts an editor)"
    }

    fn fixup_about(&self) -> &'static str {
        "Pick the commit to amend and create a fixup commit"
    }

    fn fixup_squash_help(&self) -> &'static str {
        "Create a squash commit (which joins the messages) instead of a fixup one"
    }

    // `gz merge` / `gz rebase` / `gz revert` / `gz status`

    fn merge_about(&self) -> &'static str {
        "Pick a branch and merge it"
    }

    fn merge_no_ff_help(&self) -> &'static str {
        "Create a merge commit even when a fast-forward is possible"
    }

    fn merge_squash_help(&self) -> &'static str {
        "Apply the merge result to the working tree and the index without committing"
    }

    fn merge_ff_only_help(&self) -> &'static str {
        "Merge only when a fast-forward is possible"
    }

    fn rebase_about(&self) -> &'static str {
        "Pick a branch and rebase onto it"
    }

    fn revert_about(&self) -> &'static str {
        "Pick a commit and undo it (creates a revert commit)"
    }

    fn revert_no_edit_help(&self) -> &'static str {
        "Commit with the default message of git without starting an editor"
    }

    fn status_about(&self) -> &'static str {
        "List the state of the changed files and act on the picked ones"
    }

    // `gz diff`

    fn diff_about(&self) -> &'static str {
        "Pick what to compare and show the diff"
    }

    fn diff_staged_help(&self) -> &'static str {
        "Target the staged changes (same as `git diff --staged`)"
    }

    fn diff_head_help(&self) -> &'static str {
        "Compare HEAD with the working tree (including the staged changes)"
    }

    fn diff_upstream_help(&self) -> &'static str {
        "Compare HEAD with the upstream"
    }

    fn diff_branch_help(&self) -> &'static str {
        "Pick two branches and compare them"
    }

    fn diff_commit_help(&self) -> &'static str {
        "Pick two commits and compare them"
    }

    // `gz fetch` / `gz pull` / `gz sync`

    fn fetch_about(&self) -> &'static str {
        "Decide what to fetch and fetch it"
    }

    fn fetch_prune_help(&self) -> &'static str {
        "Also clean up the tracking refs of branches deleted on the remote"
    }

    fn fetch_siblings_help(&self) -> &'static str {
        "Include the repositories next to this one and fetch them all at once"
    }

    fn pull_about(&self) -> &'static str {
        "Pick branches and make them follow their upstream (fast-forward only)"
    }

    fn sync_about(&self) -> &'static str {
        "Synchronize the current branch with its upstream"
    }

    fn sync_rebase_help(&self) -> &'static str {
        "Integrate by rebasing onto the upstream (rewrites history)"
    }

    fn sync_merge_help(&self) -> &'static str {
        "Integrate by merging the upstream"
    }

    // `gz worktree`

    fn worktree_about(&self) -> &'static str {
        "List and manage worktrees (without arguments, prints the path picked from the list)"
    }

    fn worktree_add_about(&self) -> &'static str {
        "Pick a branch and create a new worktree for it"
    }

    fn worktree_add_path_help(&self) -> &'static str {
        "Name of the worktree to create (always made next to the repository root)"
    }

    fn worktree_remove_about(&self) -> &'static str {
        "Pick a worktree and remove it (the main worktree is not offered)"
    }

    fn worktree_prune_about(&self) -> &'static str {
        "Tidy up the bookkeeping of worktrees whose directory is gone"
    }

    fn worktree_add_no_install_help(&self) -> &'static str {
        "Do not install dependencies after creating it (no lockfile is looked up either)"
    }
}
