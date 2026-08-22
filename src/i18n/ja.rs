//! 日本語の文言。

use std::path::Path;

use super::Language;
use super::messages::{
    BranchManageMessages, BranchMessages, CherryPickMessages, CliMessages, CommitMenuMessages,
    CommitMessages, CommonMessages, ConfirmMessages, DiffMessages, ErrorMessages, FetchMessages,
    FileSelectionMessages, FinderMessages, FixupMessages, InProgressMessages, LogMessages,
    MergeMessages, Messages, PullMessages, RebaseMessages, ReflogMessages, RestoreMessages,
    RevertMessages, StashMessages, StatusMessages, SyncMessages, TagMessages, WorktreeMessages,
};
use crate::error::{Error, stderr_suffix};
use crate::git::read::{BRANCH_REF_PREFIX, MalformedOutput, ReadOperation, WORKTREE_LABEL};
use crate::git::siblings::FilesystemOperation;

/// 日本語の文言一式。
///
/// フィールドを持たない ZST であり、[`Language::messages`] から `&'static` 参照として
/// 返せる。文言を追加するときは [`Messages`] にメソッドを足し、ここと
/// [`crate::i18n::en`] の双方へ実装を書く（片方でも欠けるとコンパイルエラーになる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseMessages;

impl Messages for JapaneseMessages {
    fn language(&self) -> Language {
        Language::Japanese
    }

    fn common(&self) -> &dyn CommonMessages {
        &JapaneseCommonMessages
    }

    fn cli(&self) -> &dyn CliMessages {
        &JapaneseCliMessages
    }

    fn errors(&self) -> &dyn ErrorMessages {
        &JapaneseErrorMessages
    }

    fn finder(&self) -> &dyn FinderMessages {
        &JapaneseFinderMessages
    }

    fn confirm(&self) -> &dyn ConfirmMessages {
        &JapaneseConfirmMessages
    }

    fn branch(&self) -> &dyn BranchMessages {
        &JapaneseBranchMessages
    }

    fn branch_manage(&self) -> &dyn BranchManageMessages {
        &JapaneseBranchManageMessages
    }

    fn cherry_pick(&self) -> &dyn CherryPickMessages {
        &JapaneseCherryPickMessages
    }

    fn file_selection(&self) -> &dyn FileSelectionMessages {
        &JapaneseFileSelectionMessages
    }

    fn tag(&self) -> &dyn TagMessages {
        &JapaneseTagMessages
    }

    fn reflog(&self) -> &dyn ReflogMessages {
        &JapaneseReflogMessages
    }

    fn restore(&self) -> &dyn RestoreMessages {
        &JapaneseRestoreMessages
    }

    fn revert(&self) -> &dyn RevertMessages {
        &JapaneseRevertMessages
    }

    fn stash(&self) -> &dyn StashMessages {
        &JapaneseStashMessages
    }

    fn commit(&self) -> &dyn CommitMessages {
        &JapaneseCommitMessages
    }

    fn fixup(&self) -> &dyn FixupMessages {
        &JapaneseFixupMessages
    }

    fn status(&self) -> &dyn StatusMessages {
        &JapaneseStatusMessages
    }

    fn merge(&self) -> &dyn MergeMessages {
        &JapaneseMergeMessages
    }

    fn rebase(&self) -> &dyn RebaseMessages {
        &JapaneseRebaseMessages
    }

    fn in_progress(&self) -> &dyn InProgressMessages {
        &JapaneseInProgressMessages
    }

    fn worktree(&self) -> &dyn WorktreeMessages {
        &JapaneseWorktreeMessages
    }

    fn sync(&self) -> &dyn SyncMessages {
        &JapaneseSyncMessages
    }

    fn diff(&self) -> &dyn DiffMessages {
        &JapaneseDiffMessages
    }

    fn fetch(&self) -> &dyn FetchMessages {
        &JapaneseFetchMessages
    }

    fn pull(&self) -> &dyn PullMessages {
        &JapanesePullMessages
    }

    fn log(&self) -> &dyn LogMessages {
        &JapaneseLogMessages
    }

    fn commit_menu(&self) -> &dyn CommitMenuMessages {
        &JapaneseCommitMenuMessages
    }
}

/// 複数のコマンドで共有する語彙の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseCommonMessages;

impl CommonMessages for JapaneseCommonMessages {
    fn stdout_write_failed(&self) -> &'static str {
        "標準出力への書き込みに失敗しました"
    }

    fn stderr_write_failed(&self) -> &'static str {
        "標準エラー出力への書き込みに失敗しました"
    }

    fn commit_history_read_failed(&self) -> &'static str {
        "コミット履歴の取得に失敗しました"
    }

    fn commit_hash_parse_failed(&self, id: &str) -> String {
        format!("コミットハッシュ `{id}` を解釈できません")
    }

    fn commit_read_failed(&self, id: &str) -> String {
        format!("コミット `{id}` の取得に失敗しました")
    }

    fn changed_files_read_failed(&self) -> &'static str {
        "変更ファイル一覧の取得に失敗しました"
    }

    fn branch_list_read_failed(&self) -> &'static str {
        "ブランチ一覧の取得に失敗しました"
    }

    fn stash_list_read_failed(&self) -> &'static str {
        "stash 一覧の取得に失敗しました"
    }

    fn tag_list_read_failed(&self) -> &'static str {
        "タグ一覧の取得に失敗しました"
    }

    fn worktree_list_read_failed(&self) -> &'static str {
        "worktree 一覧の取得に失敗しました"
    }

    fn remote_list_read_failed(&self) -> &'static str {
        "リモート一覧の取得に失敗しました"
    }

    fn current_branch_read_failed(&self) -> &'static str {
        "現在のブランチの取得に失敗しました"
    }

    fn upstream_read_failed(&self, branch: &str) -> String {
        format!("`{branch}` の upstream の取得に失敗しました")
    }

    fn ahead_behind_failed(&self, reference: &str) -> String {
        format!("`{reference}` との差の算出に失敗しました")
    }

    fn detached_head_without_upstream(&self) -> &'static str {
        "detached HEAD には upstream がありません。\
`gz branch` でブランチへ切り替えてから実行してください"
    }

    fn upstream_not_configured(&self, branch: &str) -> String {
        format!(
            "`{branch}` に upstream が設定されていません。\
`git push -u <remote> <branch>` で push するか、`git branch --set-upstream-to=<remote>/<branch>` で設定してください"
        )
    }

    fn history_rewrite_note(&self) -> &'static str {
        "rebase は replay したコミットを作り直すため、\
コミットハッシュが変わります（push 済みのコミットを含む場合は特に注意してください）"
    }

    fn command_run_failed(&self, command: &str) -> String {
        format!("{command} の実行に失敗しました")
    }

    fn run_summary(&self, succeeded: usize, failed: usize) -> String {
        format!("成功 {succeeded} 件 / 失敗 {failed} 件")
    }

    fn switch_failed(&self, target: &str) -> String {
        format!("`{target}` への切り替えに失敗しました")
    }

    fn failed_targets(&self, names: &str) -> String {
        format!("（失敗: {names}）")
    }
}

/// [`crate::error::Error`] の日本語表示。
///
/// 文言 trait ごとに ZST を分けているのは、移行の単位（trait 1 つ）と型が一致し、
/// `ja.rs` が育っても実装ブロックの範囲が読み取れるようにするため。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseErrorMessages;

impl ErrorMessages for JapaneseErrorMessages {
    fn prefix(&self) -> &'static str {
        "エラー: "
    }

    /// 原因と次にとるべき操作を日本語で説明する。
    ///
    /// 文言は移行前（`#[error("…")]` が日本語だった時点）が伝えていた内容をそのまま維持する
    /// （requirements.md FR-27「移行前後でメッセージが伝える内容を変えない」）。
    ///
    /// `RepositoryReadFailed` / `FilesystemReadFailed` の `operation` は、`git/read.rs` が
    /// 組み立てた**日本語の文字列**をそのまま埋め込む暫定実装である。enum 化は
    /// `git/read.rs` の文言移行と併せた最終フェーズで行う。
    fn describe(&self, error: &Error) -> String {
        match error {
            Error::NotARepository { .. } => {
                "git リポジトリではありません。git リポジトリ内で実行してください".to_owned()
            }
            Error::RepositoryReadFailed { operation, .. } => {
                format!(
                    "リポジトリ情報の読み取りに失敗しました（{operation}）",
                    operation = read_operation_text(operation)
                )
            }
            Error::GitOutputMalformed { operation, detail } => format!(
                "リポジトリ情報の読み取りに失敗しました（{operation}）: {detail}",
                operation = read_operation_text(operation),
                detail = malformed_output_text(detail)
            ),
            Error::GitCommandFailed { args, stderr } => {
                format!(
                    "git コマンドが失敗しました: git {args}{suffix}",
                    suffix = stderr_suffix(stderr)
                )
            }
            Error::GitRunFailed { command, code } => {
                format!("{command} が{status}", status = exit_status_text(*code))
            }
            Error::GitNotFound => {
                "git コマンドが見つかりません。git をインストールして PATH を通してください"
                    .to_owned()
            }
            Error::GitSpawnFailed { args, .. } => {
                format!("git コマンドの起動に失敗しました: git {args}")
            }
            Error::UnbornHead { branch } => format!(
                "現在のブランチ `{branch}` にはまだコミットがありません。\
                 `git commit` で最初のコミットを作成してから実行してください"
            ),
            Error::NoWorktree => {
                "作業ツリーがありません。bare リポジトリでは実行できない操作です".to_owned()
            }
            Error::NoSiblingScope { workdir } => format!(
                "ワークツリーのルート `{workdir}` の親ディレクトリを取得できないため、\
                 兄弟リポジトリの走査範囲を決められません",
                workdir = workdir.display()
            ),
            Error::FilesystemReadFailed {
                operation, path, ..
            } => format!(
                "{operation}に失敗しました: {path}",
                operation = filesystem_operation_text(*operation),
                path = path.display()
            ),
            Error::InvalidFetchJobs { value } => format!(
                "git config `fuzgit.fetchJobs` の値 `{value}` は同時実行数として使えません。\
                 1 以上の整数を設定するか、設定を削除して既定値に戻してください"
            ),
            Error::InvalidNotify { value } => format!(
                "git config `fuzgit.notify` の値 `{value}` は真偽値として解釈できません。\
                 `true` / `false` のような git の真偽値を設定するか、設定を削除して\
                 通知を無効のままにしてください"
            ),
            Error::Cancelled => "選択が中断されました".to_owned(),
            Error::NoCandidates => "選択できる候補がありません".to_owned(),
            Error::FinderFailed { message } => {
                format!("fuzzy finder の実行に失敗しました: {message}")
            }
        }
    }
}

/// 読み取り操作を日本語の名詞句へ変換する。
///
/// [`JapaneseErrorMessages::describe`] の `RepositoryReadFailed` は
/// `リポジトリ情報の読み取りに失敗しました（{operation}）` という書式であるため、
/// ここが返すのは助詞を伴わない名詞句である。
///
/// ワイルドカードの腕（`_ =>`）を置かない。バリアントが増えたときにコンパイルエラーで
/// 翻訳漏れへ気づくためである（[`crate::git::read::ReadOperation`] を参照）。
fn read_operation_text(operation: &ReadOperation) -> String {
    match operation {
        ReadOperation::HeadRead => "HEAD の読み取り".to_owned(),
        ReadOperation::HeadBranchNameDecode => "HEAD のブランチ名の解釈".to_owned(),
        ReadOperation::HeadResolve => "HEAD の解決".to_owned(),
        ReadOperation::ReferenceList => "参照の列挙".to_owned(),
        ReadOperation::LocalBranchList => "ローカルブランチの列挙".to_owned(),
        ReadOperation::LocalBranchRead => "ローカルブランチの読み取り".to_owned(),
        ReadOperation::LocalBranchNameDecode => "ローカルブランチ名の解釈".to_owned(),
        ReadOperation::RemoteBranchList => "リモート追跡ブランチの列挙".to_owned(),
        ReadOperation::RemoteBranchRead => "リモート追跡ブランチの読み取り".to_owned(),
        ReadOperation::RemoteBranchNameDecode => "リモート追跡ブランチ名の解釈".to_owned(),
        ReadOperation::RemoteNameDecode => "リモート名の解釈".to_owned(),
        ReadOperation::BranchNameParse { branch } => format!("ブランチ名 `{branch}` の解釈"),
        ReadOperation::RemoteUrlDecode => "リモート URL の解釈".to_owned(),
        ReadOperation::UpstreamResolve { branch } => format!("`{branch}` の upstream の解決"),
        ReadOperation::UpstreamRefNameDecode => "upstream の参照名の解釈".to_owned(),
        ReadOperation::RevListOutputParse => "git rev-list 出力の解釈".to_owned(),
        ReadOperation::BranchRead => "ブランチの読み取り".to_owned(),
        ReadOperation::BranchTipResolve => "ブランチ先端の解決".to_owned(),
        ReadOperation::BranchResolve { branch } => format!("ブランチ `{branch}` の解決"),
        ReadOperation::CommitHistoryWalk => "コミット履歴の走査".to_owned(),
        ReadOperation::CommitObjectFetch => "コミットオブジェクトの取得".to_owned(),
        ReadOperation::ShortIdCompute => "短縮ハッシュの算出".to_owned(),
        ReadOperation::CommitMessageDecode => "コミットメッセージの解釈".to_owned(),
        ReadOperation::CommitAuthorDecode => "コミット作者の解釈".to_owned(),
        ReadOperation::CommitTimeDecode => "コミット日時の解釈".to_owned(),
        ReadOperation::CommitTimeFormat => "コミット日時の整形".to_owned(),
        ReadOperation::CommitSummaryDecode => "コミットサマリの解釈".to_owned(),
        ReadOperation::CommitAuthorNameDecode => "コミット作者名の解釈".to_owned(),
        ReadOperation::StatusOutputParse => "git status 出力の解釈".to_owned(),
        ReadOperation::PathDecode => "パスの解釈".to_owned(),
        ReadOperation::RenameOriginPathDecode => "リネーム元のパスの解釈".to_owned(),
        ReadOperation::RevisionResolve { revision } => format!("リビジョン `{revision}` の解決"),
        ReadOperation::TagList => "タグの列挙".to_owned(),
        ReadOperation::TagRead => "タグの読み取り".to_owned(),
        ReadOperation::TagNameDecode => "タグ名の解釈".to_owned(),
        ReadOperation::TagResolve { tag } => format!("タグ `{tag}` の解決"),
        ReadOperation::TagTargetFetch { tag } => format!("タグ `{tag}` の対象の取得"),
        ReadOperation::TagParse { tag } => format!("タグ `{tag}` の解釈"),
        ReadOperation::TagDecode { tag } => format!("タグ `{tag}` の復号"),
        ReadOperation::TagMessageDecode => "タグメッセージの解釈".to_owned(),
        ReadOperation::HeadReflogRead => "HEAD の reflog の読み取り".to_owned(),
        ReadOperation::HeadReflogParse => "HEAD の reflog の解釈".to_owned(),
        ReadOperation::ReflogMessageDecode => "reflog メッセージの解釈".to_owned(),
        ReadOperation::StashListOutputParse => "git stash list 出力の解釈".to_owned(),
        ReadOperation::StashSelectorDecode => "stash の参照の解釈".to_owned(),
        ReadOperation::StashMessageDecode => "stash メッセージの解釈".to_owned(),
        ReadOperation::MergedBranchOutputParse => "git branch --merged 出力の解釈".to_owned(),
        ReadOperation::ForEachRefOutputParse => "git for-each-ref 出力の解釈".to_owned(),
        ReadOperation::WorktreeListOutputParse => "git worktree list 出力の解釈".to_owned(),
    }
}

/// `git` の出力の食い違いを日本語の 1 文へ変換する。
///
/// 受け取った値は `{:?}` で囲んで示す。空文字列や制御文字を含む出力でも
/// 「何を受け取ったか」が読み取れるようにするためである。
fn malformed_output_text(detail: &MalformedOutput) -> String {
    match detail {
        MalformedOutput::AheadBehind { output } => {
            format!("ahead/behind の形式が想定と異なります: {output:?}")
        }
        MalformedOutput::CommitCount { output } => {
            format!("コミット数の形式が想定と異なります: {output:?}")
        }
        MalformedOutput::StatusEntry { record } => {
            format!("エントリの形式が想定と異なります: {record:?}")
        }
        MalformedOutput::StatusRenameOriginMissing { path } => {
            format!("`{path}` のリネーム元のパスが見つかりません")
        }
        MalformedOutput::StashRecordPairing { records } => {
            format!("参照とメッセージの組になっていません（{records} レコード）")
        }
        MalformedOutput::StashSelectorFormat { selector } => {
            format!("`{selector}` は `stash@{{n}}` 形式ではありません")
        }
        MalformedOutput::BranchActivityPair { line } => {
            format!("参照名と日時の組になっていません: {line:?}")
        }
        MalformedOutput::WorktreeAttributeValueMissing { label } => {
            format!("`{label}` 属性の値がありません")
        }
        MalformedOutput::WorktreeBranchReference { reference } => {
            format!("`{reference}` は `{BRANCH_REF_PREFIX}` で始まる参照名ではありません")
        }
        MalformedOutput::WorktreeRecordStart { line } => {
            format!("レコードが `{WORKTREE_LABEL}` 属性で始まっていません: {line:?}")
        }
        MalformedOutput::WorktreeRecordUnterminated { path } => {
            format!("`{path}` のレコードが終端されていません")
        }
    }
}

/// ファイルシステムの操作を日本語の名詞句へ変換する。
///
/// [`JapaneseErrorMessages::describe`] の `FilesystemReadFailed` は
/// `{operation}に失敗しました: {path}` という書式であるため、ここが返すのは
/// 助詞を伴わない名詞句である。
fn filesystem_operation_text(operation: FilesystemOperation) -> &'static str {
    match operation {
        FilesystemOperation::PathCanonicalization => "パスの正規化",
        FilesystemOperation::DirectoryScan => "ディレクトリの走査",
    }
}

/// プロセスの終了状況を日本語の述部として整形する。
///
/// シグナルで終了した場合は終了コードが得られないため、コードを取り繕わず理由の方を示す。
fn exit_status_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("終了コード {code} で終了しました"),
        None => "シグナルにより終了しました".to_owned(),
    }
}

/// 選択 UI（[`crate::finder`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseFinderMessages;

impl FinderMessages for JapaneseFinderMessages {
    fn truncation_notice(&self) -> &'static str {
        "… （以降は省略しました）"
    }

    fn file_read_failed(&self, path: &Path, error: &std::io::Error) -> String {
        format!("{path} を読み込めません: {error}", path = path.display())
    }
}

/// 確認プロンプト（[`crate::commands::confirmation`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseConfirmMessages;

impl ConfirmMessages for JapaneseConfirmMessages {
    fn prompt(&self) -> &'static str {
        "実行しますか? [y/N]: "
    }

    fn cancelled(&self) -> &'static str {
        "中止しました"
    }

    fn output_failed(&self) -> &'static str {
        "確認メッセージの出力に失敗しました"
    }

    fn input_failed(&self) -> &'static str {
        "確認入力の読み取りに失敗しました"
    }
}

/// `gz branch`（[`crate::commands::branch`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseBranchMessages;

impl BranchMessages for JapaneseBranchMessages {
    fn header_subject(&self) -> &'static str {
        "切り替えるブランチを選択"
    }

    fn header_outcome(&self) -> &'static str {
        "切り替え"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたブランチ `{selected}` が候補に見つかりません")
    }

    fn tracking_target_undetermined(&self, name: &str) -> String {
        format!("リモート追跡ブランチ `{name}` から追跡先のブランチ名を決定できません")
    }
}

/// `gz branch create` / `delete` / `cleanup`（[`crate::commands::branch_manage`]）の
/// 日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseBranchManageMessages;

impl BranchManageMessages for JapaneseBranchManageMessages {
    fn base_header_subject(&self) -> &'static str {
        "新しいブランチの作成元を選択"
    }

    fn base_header_outcome_switch(&self) -> &'static str {
        "作成して切り替え"
    }

    fn base_header_outcome_stay(&self) -> &'static str {
        "作成のみ（切り替えない）"
    }

    fn base_selection_not_found(&self, selected: &str) -> String {
        format!("選択された作成元 `{selected}` が候補に見つかりません")
    }

    fn creation_failed(&self, name: &str) -> String {
        format!("ブランチ `{name}` の作成に失敗しました")
    }

    fn created(&self, name: &str, base: &str) -> String {
        format!("ブランチ `{name}` を {base} から作成しました")
    }

    fn switch_hint(&self, name: &str) -> String {
        format!("切り替えるには `git switch {name}` を実行してください")
    }

    fn tracking(&self, upstream: &str) -> String {
        format!("追跡: {upstream}")
    }

    fn no_tracking(&self) -> &'static str {
        "追跡なし"
    }

    fn unknown_date(&self) -> &'static str {
        "更新日時不明"
    }

    fn merged_state_read_failed(&self) -> &'static str {
        "取り込み済みブランチの判定に失敗しました"
    }

    fn activity_read_failed(&self) -> &'static str {
        "ブランチの最終更新日時の取得に失敗しました"
    }

    fn unknown_merge_base(&self, into: &str) -> String {
        format!("`--into` に指定したブランチ `{into}` が見つかりません")
    }

    fn selection_not_found(&self, names: &str) -> String {
        format!("選択されたブランチ {names} が候補に見つかりません")
    }

    fn deletion_failed(&self) -> &'static str {
        "ブランチの削除に失敗しました"
    }

    fn delete_header(&self) -> &'static str {
        "Tab: 選択の切替 / Enter: 選択したブランチを削除"
    }

    fn no_delete_candidates(&self) -> &'static str {
        "削除できるブランチがありません\
（現在のブランチと、worktree でチェックアウト中のブランチは対象になりません）"
    }

    fn unmerged_rejection(&self, names: &str) -> String {
        format!(
            "取り込まれていない（unmerged）ブランチが選択に含まれています: {names}\n\
             これらを削除すると、そのブランチにしか無いコミットが失われる可能性があります。\
             それでも削除する場合は `gz branch delete --force` を実行してください"
        )
    }

    fn delete_confirmation(&self) -> &'static str {
        "以下のブランチを削除します（削除したブランチは元に戻せません）"
    }

    fn unmerged_confirmation(&self, names: &str) -> String {
        format!(
            "{base}\n\
             警告: 取り込まれていない（unmerged）ブランチが含まれます: {names}\n\
             これらのブランチにしか無いコミットは失われます",
            base = self.delete_confirmation()
        )
    }

    fn cleanup_header(&self) -> &'static str {
        "全件を選択済みにしています。Tab: 残すブランチの選択を外す / Enter: 削除"
    }

    fn no_cleanup_candidates(&self) -> &'static str {
        "取り込み済み（merged）のブランチがありません"
    }
}

/// `gz cherry-pick`（[`crate::commands::cherry_pick`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseCherryPickMessages;

impl CherryPickMessages for JapaneseCherryPickMessages {
    fn selection_not_found(&self, hashes: &str) -> String {
        format!("選択されたコミット {hashes} が候補に見つかりません")
    }

    fn resolution_hint(&self) -> &'static str {
        "cherry-pick に失敗しました。\
         解決後に `git cherry-pick --continue`、中止する場合は `git cherry-pick --abort` を実行してください"
    }
}

/// ファイル選択（[`crate::commands::file_selection`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseFileSelectionMessages;

impl FileSelectionMessages for JapaneseFileSelectionMessages {
    fn selection_not_found(&self, paths: &str) -> String {
        format!("選択されたファイル {paths} が候補に見つかりません")
    }
}

/// `gz tag`（[`crate::commands::tag`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseTagMessages;

impl TagMessages for JapaneseTagMessages {
    fn header_subject(&self) -> &'static str {
        "タグを選択"
    }

    fn header_outcome_print(&self) -> &'static str {
        "タグ名を出力"
    }

    fn header_outcome_switch(&self) -> &'static str {
        "detached HEAD で切り替え"
    }

    fn header_outcome_diff(&self) -> &'static str {
        "HEAD との差分を表示"
    }

    fn conflicting_actions(&self) -> &'static str {
        "`--switch` と `--diff` は同時に指定できません"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたタグ `{selected}` が候補に見つかりません")
    }

    fn switch_failed(&self, name: &str) -> String {
        format!("タグ `{name}` への切り替えに失敗しました")
    }

    fn diff_failed(&self, name: &str) -> String {
        format!("タグ `{name}` との差分表示に失敗しました")
    }
}

/// `gz reflog`（[`crate::commands::reflog`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseReflogMessages;

impl ReflogMessages for JapaneseReflogMessages {
    fn header_subject(&self) -> &'static str {
        "reflog のエントリを選択"
    }

    fn header_outcome_print(&self) -> &'static str {
        "フルハッシュを出力"
    }

    fn header_outcome_menu(&self) -> &'static str {
        "操作を選ぶ"
    }

    fn conflicting_actions(&self) -> &'static str {
        "--restore と --action は同時に指定できません"
    }

    fn header_outcome_restore(&self, name: &str) -> String {
        format!("ブランチ `{name}` を作成")
    }

    fn read_failed(&self) -> &'static str {
        "reflog の取得に失敗しました"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択された reflog エントリ `{selected}` が候補に見つかりません")
    }

    fn branch_creation_failed(&self, name: &str) -> String {
        format!("ブランチ `{name}` の作成に失敗しました")
    }

    fn branch_created(&self, name: &str, id: &str) -> String {
        format!("ブランチ `{name}` を {id} に作成しました")
    }
}

/// `gz restore`（[`crate::commands::restore`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseRestoreMessages;

impl RestoreMessages for JapaneseRestoreMessages {
    fn revision_files_read_failed(&self, revision: &str) -> String {
        format!("`{revision}` のファイル一覧の取得に失敗しました")
    }

    fn discard_confirmation(&self, count: usize) -> String {
        format!("以下 {count} 件のファイルの変更を破棄します（元に戻せません）:")
    }

    fn overwrite_confirmation(&self, count: usize, revision: &str) -> String {
        format!(
            "以下 {count} 件のファイルを `{revision}` の内容で上書きします（作業ツリーの変更は失われます）:"
        )
    }
}

/// `gz revert`（[`crate::commands::revert`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseRevertMessages;

impl RevertMessages for JapaneseRevertMessages {
    fn selection_not_found(&self, hashes: &str) -> String {
        format!("選択されたコミット {hashes} が候補に見つかりません")
    }

    fn resolution_hint(&self) -> &'static str {
        "revert に失敗しました。\
         解決後に `git revert --continue`、中止する場合は `git revert --abort` を実行してください"
    }

    fn merge_commit_selected(&self) -> &'static str {
        "選択にマージコミットが含まれています。マージコミットの revert には\
         打ち消す側の親の番号（`-m <parent-number>`）の指定が必要ですが、\
         fuzgit は対応していません。素の git で次のように実行してください。"
    }
}

/// `gz stash`（[`crate::commands::stash`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseStashMessages;

impl StashMessages for JapaneseStashMessages {
    fn header_subject(&self) -> &'static str {
        "stash を選択"
    }

    fn header_outcome_apply(&self) -> &'static str {
        "作業ツリーへ適用（stash は残す）"
    }

    fn header_outcome_pop(&self) -> &'static str {
        "作業ツリーへ適用して stash を取り除く"
    }

    fn header_outcome_drop(&self) -> &'static str {
        "確認のうえ破棄"
    }

    fn drop_confirmation(&self) -> &'static str {
        "以下の stash を破棄します（元に戻せません）:"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択された stash `{selected}` が候補に見つかりません")
    }
}

/// `gz commit`（[`crate::commands::commit`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseCommitMessages;

impl CommitMessages for JapaneseCommitMessages {
    fn header(&self) -> &'static str {
        "Tab: 選択の切替 / Enter: コミット（ステージ済みは選択済み）"
    }

    /// 案内の 2 行目以降は行頭の字下げも表示の一部であるため、字下げごと文言に含める。
    fn editor_hint(&self) -> &'static str {
        "ヒント: エディタでコミットメッセージが保存されなかった場合、\
git はメッセージが空だと判断してコミットを中止します。
  - `gz commit -m \"<メッセージ>\"` を使うと、エディタを介さずメッセージを指定できます。
  - EDITOR / GIT_EDITOR に GUI エディタを設定している場合は、終了を待つオプション\
（例: `code --wait`）が必要です。待機しない設定ではエディタがすぐに終了し、\
メッセージが空のまま扱われます。"
    }

    fn untracked_stage_failed(&self) -> &'static str {
        "未追跡ファイルのステージ（git add）に失敗しました"
    }
}

/// `gz fixup`（[`crate::commands::fixup`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseFixupMessages;

impl FixupMessages for JapaneseFixupMessages {
    fn header_subject(&self) -> &'static str {
        "修正対象のコミットを選択"
    }

    fn header_outcome(&self, label: &str) -> String {
        format!("{label} コミットを作成")
    }

    fn staged_changes_read_failed(&self) -> &'static str {
        "ステージ済みの変更の取得に失敗しました"
    }

    fn staged_required(&self, label: &str) -> String {
        format!(
            "ステージ済みの変更がありません。{label} コミットにはステージ済みの変更が必要なため、\
             `gz add` などでステージしてから実行してください"
        )
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたコミット `{selected}` が候補に見つかりません")
    }

    fn commit_creation_failed(&self, label: &str) -> String {
        format!("{label} コミットの作成に失敗しました")
    }

    fn autosquash_hint(&self, start: &str) -> String {
        format!(
            "ヒント: 作成したコミットを履歴へ取り込むには次を実行してください。\n  \
             git rebase -i --autosquash {start}"
        )
    }

    fn root_start_note(&self) -> &'static str {
        "（選択したコミットには親が無い（最初のコミット）ため、\
         `<hash>^` ではなく `--root` を起点にします）"
    }
}

/// `gz status`（[`crate::commands::status`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseStatusMessages;

impl StatusMessages for JapaneseStatusMessages {
    fn clean(&self) -> &'static str {
        "変更はありません（作業ツリーはクリーンです）"
    }

    fn add_action(&self) -> &'static str {
        "選択したファイルをステージする (git add)"
    }

    fn restore_action(&self) -> &'static str {
        "選択したファイルの変更を破棄する (git restore)"
    }

    fn stash_action(&self) -> &'static str {
        "選択したファイルを stash へ退避する (git stash push)"
    }

    fn commit_action(&self) -> &'static str {
        "選択したファイルをコミットする (git commit)"
    }

    fn print_action(&self) -> &'static str {
        "選択したファイルのパスを標準出力へ出力する"
    }

    fn menu_selection_not_found(&self, selected: &str) -> String {
        format!("選択されたメニュー項目 `{selected}` が見つかりません")
    }
}

/// `gz merge`（[`crate::commands::merge`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseMergeMessages;

impl MergeMessages for JapaneseMergeMessages {
    fn header_subject(&self) -> &'static str {
        "merge するブランチを選択"
    }

    fn header_outcome(&self) -> &'static str {
        "merge を実行"
    }

    fn conflicting_modes(&self) -> &'static str {
        "`--no-ff` / `--squash` / `--ff-only` は同時に指定できません"
    }

    fn no_candidates(&self) -> &'static str {
        "merge 対象になるブランチがありません。\
現在のブランチ以外のローカルブランチ・リモート追跡ブランチが必要です"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたブランチ `{selected}` が候補に見つかりません")
    }

    fn merged_commit_count_failed(&self, branch: &str) -> String {
        format!("`{branch}` から取り込まれるコミット数の取得に失敗しました")
    }

    fn merge_failed(&self, branch: &str) -> String {
        format!("`{branch}` の merge に失敗しました")
    }

    fn confirmation(&self, branch: &str, count: usize) -> String {
        format!("`{branch}` を merge します（取り込まれるコミット: {count} 件）")
    }

    fn command_line(&self, command: &str) -> String {
        format!("実行するコマンド: {command}")
    }

    fn prediction_clean(&self) -> &'static str {
        "コンフリクト予測: コンフリクトなく merge できる見込みです"
    }

    fn prediction_conflicted(&self, count: usize) -> String {
        format!("コンフリクト予測: 次の {count} 件のファイルで発生する見込みです")
    }

    fn prediction_unnamed(&self) -> &'static str {
        "コンフリクト予測: コンフリクトが発生する見込みです\
（対象のファイル名は取得できませんでした）"
    }

    fn prediction_unavailable(&self) -> &'static str {
        "コンフリクト予測: 省略しました\
（`git merge-tree --write-tree` を実行できませんでした。予測には Git 2.38 以降が必要です）"
    }
}

/// `gz rebase`（[`crate::commands::rebase`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseRebaseMessages;

impl RebaseMessages for JapaneseRebaseMessages {
    fn header_subject(&self) -> &'static str {
        "rebase 先のブランチを選択"
    }

    fn header_outcome(&self) -> &'static str {
        "rebase を実行"
    }

    fn no_candidates(&self) -> &'static str {
        "rebase の base になるブランチがありません。\
現在のブランチ以外のローカルブランチ・リモート追跡ブランチが必要です"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたブランチ `{selected}` が候補に見つかりません")
    }

    fn replayed_commit_count_failed(&self, base: &str) -> String {
        format!("`{base}` の上に replay されるコミット数の取得に失敗しました")
    }

    fn rebase_failed(&self, base: &str) -> String {
        format!("`{base}` への rebase に失敗しました")
    }

    fn confirmation(&self, base: &str, count: usize) -> String {
        format!("`{base}` の上に {count} 件のコミットを replay します")
    }
}

/// merge / rebase の復帰メニュー（[`crate::commands::in_progress`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseInProgressMessages;

impl InProgressMessages for JapaneseInProgressMessages {
    fn conflicts_action(&self) -> &'static str {
        "コンフリクトファイルを確認して解決済みにする (git add)"
    }

    fn continue_action(&self, operation: &str) -> String {
        format!("{operation} を再開する")
    }

    fn skip_action(&self) -> &'static str {
        "現在のコミットを飛ばす"
    }

    fn abort_action(&self, operation: &str) -> String {
        format!("{operation} を中止する")
    }

    fn menu_selection_not_found(&self, selected: &str) -> String {
        format!("選択されたメニュー項目 `{selected}` が見つかりません")
    }

    fn abort_confirmation(&self, operation: &str) -> String {
        format!(
            "{operation} を中止すると、これまでのコンフリクトの解決内容（stage 済みのものを含む）は失われ、\
{operation} を開始する前の状態に戻ります"
        )
    }

    fn conflicts_header(&self) -> &'static str {
        "Tab: 選択の切替 / Enter: 選択したファイルを解決済みとして stage"
    }

    fn conflicts_read_failed(&self) -> &'static str {
        "コンフリクト中のファイル一覧の取得に失敗しました"
    }

    fn no_conflicts(&self) -> &'static str {
        "コンフリクト中のファイルはありません。\
すべて解決済みであれば、メニューの continue で処理を再開してください"
    }

    fn stage_failed(&self) -> &'static str {
        "解決済みとしての stage（git add）に失敗しました"
    }
}

/// `gz worktree`（[`crate::commands::worktree`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseWorktreeMessages;

impl WorktreeMessages for JapaneseWorktreeMessages {
    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択された worktree `{selected}` が候補に見つかりません")
    }

    fn path_not_utf8(&self, path: &Path) -> String {
        format!(
            "worktree のパス `{path}` を UTF-8 として解釈できません",
            path = path.display()
        )
    }

    fn no_available_branch(&self) -> &'static str {
        "worktree に割り当てられるローカルブランチがありません\
（他の worktree でチェックアウト中のブランチは対象になりません）"
    }

    fn branch_selection_not_found(&self, selected: &str) -> String {
        format!("選択されたブランチ `{selected}` が候補に見つかりません")
    }

    fn creation_failed(&self, path: &str) -> String {
        format!("worktree `{path}` の作成に失敗しました")
    }

    fn no_removable(&self) -> &'static str {
        "削除できる worktree がありません\
（main worktree は `git worktree remove` の対象になりません）"
    }

    fn remove_confirmation(&self) -> &'static str {
        "以下の worktree を削除します\
（作業ツリーのディレクトリと管理情報が削除されます。ブランチは残ります）"
    }

    fn removal_failed(&self, path: &str) -> String {
        format!("worktree `{path}` の削除に失敗しました")
    }

    fn prune_targets_read_failed(&self) -> &'static str {
        "整理対象の確認に失敗しました"
    }

    fn prune_confirmation(&self) -> &'static str {
        "以下の worktree の管理情報を整理します"
    }

    fn prune_failed(&self) -> &'static str {
        "worktree の整理に失敗しました"
    }

    fn nothing_to_prune(&self) -> &'static str {
        "整理する worktree はありません"
    }

    fn install_running(&self, directory: &Path, command: &str) -> String {
        format!(
            "{directory} で `{command}` を実行します",
            directory = directory.display()
        )
    }

    /// エコシステム名・lockfile 名は固有名詞であり訳さない。
    fn install_ambiguous(&self, ecosystem: &str, lockfiles: &str) -> String {
        format!(
            "{ecosystem} の lockfile が複数あるため依存インストールを実行しません（{lockfiles}）"
        )
    }

    /// オプション名は訳さない（design.md「翻訳しないもの」）。
    fn install_flavour_unknown(&self, lockfile: &str) -> String {
        format!(
            "{lockfile} から Yarn の版を判別できないため依存インストールを実行しません\
（`--immutable` は Yarn 2 以降、`--frozen-lockfile` は Yarn 1 の綴りです）"
        )
    }

    fn install_command_missing(&self, program: &str) -> String {
        format!("`{program}` が見つからないため依存インストールを実行しませんでした")
    }

    fn agent_config_copied(&self, copied: usize, skipped: usize) -> String {
        if skipped == 0 {
            format!(".claude/ を {copied} 件コピーしました")
        } else {
            format!(
                ".claude/ を {copied} 件コピーしました（既存の {skipped} 件はそのまま残しました）"
            )
        }
    }

    fn agent_config_copy_failed(&self, path: &str) -> String {
        format!("警告: .claude/ のコピーに失敗しました: {path}")
    }

    fn install_failed(&self, command: &str) -> String {
        format!(
            "依存インストールに失敗しました（worktree は作成済みです。`{command}` で実行し直せます）"
        )
    }

    fn install_directory_not_found(&self, path: &str) -> String {
        format!(
            "作成した worktree `{path}` が worktree の一覧に見つからないため\
依存インストールを実行しませんでした"
        )
    }
}

/// `gz sync`（[`crate::commands::sync`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseSyncMessages;

impl SyncMessages for JapaneseSyncMessages {
    fn conflicting_modes(&self) -> &'static str {
        "`--rebase` / `--merge` は同時に指定できません"
    }

    fn fetch_failed(&self, remote: &str) -> String {
        format!("リモート `{remote}` からの取得に失敗しました")
    }

    fn tracking_ref_unavailable(&self, branch: &str, remote: &str, merge_ref: &str) -> String {
        format!(
            "`{branch}` の upstream（{remote} / {merge_ref}）からリモート追跡参照を組み立てられません。\
`git branch --set-upstream-to=<remote>/<branch>` で設定し直してください"
        )
    }

    fn unknown_remote(&self, branch: &str, remote: &str) -> String {
        format!(
            "`{branch}` の upstream に設定された `{remote}` は登録済みのリモートではありません。\
`git remote add <名前> <URL>` で登録するか、\
`git branch --set-upstream-to=<remote>/<branch>` で設定し直してください"
        )
    }

    fn missing_tracking_ref(&self, reference: &str, remote: &str, branch: &str) -> String {
        format!(
            "リモート追跡参照 `{reference}` が見つかりません。\
`{remote}` に `{branch}` の upstream が存在しない可能性があります\
（`git push -u <remote> <branch>` で作成するか、`git branch --set-upstream-to=<remote>/<branch>` で設定し直してください）"
        )
    }

    fn up_to_date(&self, branch: &str, reference: &str) -> String {
        format!("`{branch}` は最新です（`{reference}` から取り込むコミットはありません）")
    }

    fn unpushed_commits(&self, count: usize) -> String {
        format!("push していないコミットが {count} 件あります（`git push` で push できます）")
    }

    fn integration_failed(&self, reference: &str) -> String {
        format!("`{reference}` の取り込みに失敗しました")
    }

    fn confirmation(&self, reference: &str, count: usize, branch: &str) -> String {
        format!("`{reference}` から {count} 件のコミットを `{branch}` へ取り込みます")
    }
}

/// `gz diff`（[`crate::commands::diff`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseDiffMessages;

impl DiffMessages for JapaneseDiffMessages {
    fn conflicting_modes(&self) -> &'static str {
        "`--staged` / `--head` / `--upstream` / `--branch` / `--commit` は同時に指定できません"
    }

    fn unstaged_range(&self) -> &'static str {
        "未ステージの変更: index と作業ツリー"
    }

    fn staged_range(&self) -> &'static str {
        "ステージ済みの変更: HEAD と index"
    }

    fn head_range(&self) -> &'static str {
        "HEAD と作業ツリー"
    }

    fn base_branch_header(&self) -> &'static str {
        "1/2 比較元のブランチ"
    }

    fn target_branch_header(&self) -> &'static str {
        "2/2 比較先のブランチ"
    }

    fn base_commit_header(&self) -> &'static str {
        "1/2 比較元のコミット"
    }

    fn target_commit_header(&self) -> &'static str {
        "2/2 比較先のコミット"
    }

    fn tracking_ref_unavailable(&self, branch: &str, remote: &str, merge_ref: &str) -> String {
        format!(
            "`{branch}` の upstream（{remote} / {merge_ref}）からリモート追跡参照を組み立てられません。\
比較するリビジョンを `gz diff --branch` で選ぶか、素の `git diff` を使ってください"
        )
    }

    fn files_read_failed(&self, description: &str) -> String {
        format!("変更ファイル一覧の取得に失敗しました（{description}）")
    }

    fn branch_selection_not_found(&self, selected: &str) -> String {
        format!("選択されたブランチ `{selected}` が候補に見つかりません")
    }

    fn commit_selection_not_found(&self, selected: &str) -> String {
        format!("選択されたコミット `{selected}` が候補に見つかりません")
    }

    fn no_diff(&self, description: &str) -> String {
        format!("差分はありません（{description}）")
    }

    fn diff_failed(&self, description: &str) -> String {
        format!("差分の表示に失敗しました（{description}）")
    }
}

/// `gz fetch`（[`crate::commands::fetch`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseFetchMessages;

impl FetchMessages for JapaneseFetchMessages {
    fn remote_header_subject(&self) -> &'static str {
        "取得するリモートを選択"
    }

    fn remote_header_outcome(&self) -> &'static str {
        "取得"
    }

    fn all_remotes_label(&self) -> &'static str {
        "すべてのリモート"
    }

    fn remote_description(&self, remote: &str) -> String {
        format!("リモート `{remote}`")
    }

    fn no_remotes(&self) -> &'static str {
        "fetch 元のリモートが登録されていません。`git remote add <名前> <URL>` で追加してください"
    }

    fn fixed_target(&self, target: &str) -> String {
        format!(
            "{target} から取得します（登録されているリモートが 1 つのため、選択を省略しました）"
        )
    }

    fn fetch_failed(&self, target: &str) -> String {
        format!("{target} からの取得に失敗しました")
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたリモート `{selected}` が候補に見つかりません")
    }

    fn url_section(&self) -> &'static str {
        "リモート URL"
    }

    fn tracking_section(&self) -> &'static str {
        "既知のリモート追跡ブランチ"
    }

    fn sibling_scan_failed(&self) -> &'static str {
        "兄弟リポジトリの探索に失敗しました"
    }

    fn no_sibling_candidates(&self) -> &'static str {
        "fetch できるリポジトリがありません\
（リモートが登録されていないリポジトリと bare リポジトリは対象になりません）"
    }

    fn single_sibling_reason(&self) -> &'static str {
        "対象が現在のリポジトリ 1 件のため、選択を省略しました"
    }

    fn siblings_header(&self) -> &'static str {
        "現在のリポジトリを選択済みにしています。Tab: 選択の切替 / Enter: 取得"
    }

    fn excluded_count(&self, count: usize) -> String {
        format!("除外 {count} 件（リモート未登録 / bare）")
    }

    fn prune_scope_note(&self) -> &'static str {
        "--prune: 選択したすべてのリポジトリに適用"
    }

    fn tracking_state_section(&self) -> &'static str {
        "ブランチの追跡状況"
    }

    fn path_not_utf8(&self, path: &Path) -> String {
        format!(
            "ワークツリーのパスを文字列として扱えません: {path}",
            path = path.display()
        )
    }

    fn sibling_selection_not_found(&self, paths: &str) -> String {
        format!("選択されたリポジトリ {paths} が候補に見つかりません")
    }

    fn sibling_start_failed(&self, name: &str) -> String {
        format!("`{name}` の取得を開始できませんでした")
    }

    fn serial_fallback(&self, count: usize) -> String {
        format!(
            "並列での取得に失敗した {count} 件を、認証情報を入力できる形で 1 件ずつ実行し直します"
        )
    }

    fn partial_failure(&self) -> &'static str {
        "一部のリポジトリで取得に失敗しました"
    }

    fn notification_title(&self) -> &'static str {
        // 通知の本文は件数だけであるため、どのコマンドが終わったのかはここで示す。
        // コマンド列（`gz fetch --siblings`）は翻訳しない
        "gz fetch --siblings が完了しました"
    }
}

/// `gz pull`（[`crate::commands::pull`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapanesePullMessages;

impl PullMessages for JapanesePullMessages {
    fn targets_read_failed(&self) -> &'static str {
        "取り込み対象の候補生成に失敗しました"
    }

    fn no_candidates(&self) -> &'static str {
        "upstream へ追随させられるブランチがありません\
（upstream が設定され、そのリモートが登録済みのローカルブランチが対象です）。\
`git push -u <remote> <branch>` で push するか、`git branch --set-upstream-to=<remote>/<branch>` で設定してください"
    }

    fn header(&self) -> &'static str {
        "現在のブランチを選択済みにしています。Tab: 選択の切替 / Enter: fast-forward のみで取り込み"
    }

    fn excluded_count(&self, count: usize) -> String {
        format!("除外 {count} 件（upstream 未設定 / リモート未登録 / 他の worktree で使用中）")
    }

    fn single_target_reason(&self) -> &'static str {
        "候補が 1 件のため、選択を省略しました"
    }

    fn fixed_target(&self, branch: &str) -> String {
        format!(
            "`{branch}` を取り込みます（{reason}）",
            reason = self.single_target_reason()
        )
    }

    fn unmerged_section(&self) -> &'static str {
        "未取り込みのコミット（前回の fetch 時点）"
    }

    fn selection_not_found(&self, branches: &str) -> String {
        format!("選択されたブランチ {branches} が候補に見つかりません")
    }

    fn skipped(&self, remote: &str, branch: &str) -> String {
        format!("`{remote}` からの取得に失敗したため、`{branch}` の取り込みを飛ばしました")
    }

    fn fetch_start_failed(&self, remote: &str) -> String {
        format!("リモート `{remote}` からの取得を開始できませんでした")
    }

    fn integration_start_failed(&self, branch: &str) -> String {
        format!("`{branch}` の取り込みを開始できませんでした")
    }

    fn fast_forward_guidance(&self) -> &'static str {
        "fast-forward できなかったブランチは、\
そのブランチへ切り替えてから `gz sync --rebase` または `gz sync --merge` で取り込めます"
    }

    fn partial_failure(&self) -> &'static str {
        "一部のブランチで取り込みに失敗しました"
    }

    fn notification_title(&self) -> &'static str {
        "gz pull が完了しました"
    }
}

/// `gz log`（[`crate::commands::log`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseLogMessages;

impl LogMessages for JapaneseLogMessages {
    fn header_subject(&self) -> &'static str {
        "コミットを選択"
    }

    fn header_outcome_print(&self) -> &'static str {
        "フルハッシュを出力"
    }

    fn header_outcome_menu(&self) -> &'static str {
        "操作を選ぶ"
    }

    fn selection_not_found(&self, selected: &str) -> String {
        format!("選択されたコミット {selected} が候補一覧に見つかりません")
    }
}

/// コミット選択後のアクションメニュー（[`CommitMenuMessages`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseCommitMenuMessages;

impl CommitMenuMessages for JapaneseCommitMenuMessages {
    fn subject(&self, short_id: &str) -> String {
        format!("コミット {short_id} に対する操作を選択")
    }

    fn outcome(&self) -> &'static str {
        "選んだ操作を実行"
    }

    fn show_action(&self) -> &'static str {
        "コミットの詳細を表示する (git show)"
    }

    fn switch_action(&self) -> &'static str {
        "detached HEAD で切り替える (git switch --detach)"
    }

    fn cherry_pick_action(&self) -> &'static str {
        "現在のブランチへ取り込む (git cherry-pick)"
    }

    fn revert_action(&self) -> &'static str {
        "打ち消すコミットを作る (git revert)"
    }

    fn fixup_action(&self) -> &'static str {
        "ステージ済みの変更で fixup コミットを作る (git commit --fixup)"
    }

    fn reset_action(&self) -> &'static str {
        "現在のブランチをこのコミットへ戻す (git reset --hard)"
    }

    fn print_action(&self) -> &'static str {
        "フルハッシュを標準出力へ出力する"
    }

    fn menu_selection_not_found(&self, selected: &str) -> String {
        format!("選択されたメニュー項目 {selected} が見つかりません")
    }

    fn reset_confirmation(&self) -> &'static str {
        "作業ツリーとインデックスの未コミットの変更をすべて破棄し、現在のブランチを次のコミットへ移動します（移動元のコミットは reflog に残ります）"
    }

    fn show_failed(&self, id: &str) -> String {
        format!("コミット {id} の表示に失敗しました")
    }

    fn switch_failed(&self, id: &str) -> String {
        format!("コミット {id} への切り替えに失敗しました")
    }

    fn reset_failed(&self, id: &str) -> String {
        format!("コミット {id} への reset に失敗しました")
    }
}

/// `clap` のヘルプ（[`CliMessages`]）の日本語表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseCliMessages;

impl CliMessages for JapaneseCliMessages {
    // `gz` 自身

    fn about(&self) -> &'static str {
        "fuzzy finder で選んで操作する git CLI"
    }

    fn lang_help(&self) -> &'static str {
        "表示言語（省略時は FUZGIT_LANG / git config fuzgit.lang / ロケールから決まる）"
    }

    // `gz branch`（切替と管理サブコマンド）

    fn branch_about(&self) -> &'static str {
        "ブランチを選択して切り替える（サブコマンドで作成・削除・整理も行う）"
    }

    fn branch_all_help(&self) -> &'static str {
        "リモート追跡ブランチも候補に含める"
    }

    fn branch_create_about(&self) -> &'static str {
        "作成元を選択して新しいブランチを作成し、そのブランチへ切り替える"
    }

    fn branch_create_name_help(&self) -> &'static str {
        "作成するブランチ名"
    }

    fn branch_create_no_switch_help(&self) -> &'static str {
        "切り替えずに作成だけ行う"
    }

    fn branch_delete_about(&self) -> &'static str {
        "ブランチを選択して削除する"
    }

    fn branch_delete_force_help(&self) -> &'static str {
        "merged でないブランチも削除する（`git branch -D`）"
    }

    fn branch_delete_into_help(&self) -> &'static str {
        "merged 判定の基準ブランチ（既定は HEAD）"
    }

    fn branch_cleanup_about(&self) -> &'static str {
        "merged なブランチを一括で削除する"
    }

    fn branch_cleanup_into_help(&self) -> &'static str {
        "merged 判定の基準ブランチ（既定は HEAD）"
    }

    // `gz log` / `gz cherry-pick` / `gz restore` / `gz add`

    fn log_about(&self) -> &'static str {
        "コミット履歴を辿り、選択したコミットのフルハッシュを標準出力へ出す"
    }

    fn log_limit_help(&self) -> &'static str {
        "取得するコミットの最大件数"
    }

    fn log_action_help(&self) -> &'static str {
        "選択したコミットに対して行う操作をメニューから選ぶ"
    }

    fn cherry_pick_about(&self) -> &'static str {
        "コミットを選択して cherry-pick する"
    }

    fn cherry_pick_branch_help(&self) -> &'static str {
        "対象ブランチ（未指定時は全ブランチのコミットを候補にする）"
    }

    fn restore_about(&self) -> &'static str {
        "ファイルを選択して git restore する"
    }

    fn restore_source_help(&self) -> &'static str {
        "復元元のリビジョン"
    }

    fn restore_staged_help(&self) -> &'static str {
        "ステージ済みの変更をアンステージする"
    }

    fn add_about(&self) -> &'static str {
        "未ステージ・未追跡ファイルを選択して git add する"
    }

    // `gz stash`

    fn stash_about(&self) -> &'static str {
        "変更を stash へ退避し、stash を検索して適用・破棄する"
    }

    fn stash_push_about(&self) -> &'static str {
        "変更ファイルを選択して stash へ退避する"
    }

    fn stash_push_message_help(&self) -> &'static str {
        "stash に付けるメッセージ"
    }

    fn stash_push_include_untracked_help(&self) -> &'static str {
        "未追跡ファイルも候補に含める（既定は追跡済みの変更のみ）"
    }

    fn stash_apply_about(&self) -> &'static str {
        "stash を選択して適用する（stash は残す）"
    }

    fn stash_pop_about(&self) -> &'static str {
        "stash を選択して適用し、その stash を取り除く"
    }

    fn stash_drop_about(&self) -> &'static str {
        "stash を選択して破棄する"
    }

    // `gz tag` / `gz reflog`

    fn tag_about(&self) -> &'static str {
        "タグを選択する（既定はタグ名を標準出力へ出す）"
    }

    fn tag_switch_help(&self) -> &'static str {
        "選択したタグへ detached HEAD で切り替える"
    }

    fn tag_diff_help(&self) -> &'static str {
        "選択したタグと HEAD の差分を表示する"
    }

    fn reflog_about(&self) -> &'static str {
        "HEAD の reflog を辿り、選択したコミットのハッシュを標準出力へ出す"
    }

    fn reflog_restore_help(&self) -> &'static str {
        "選択したコミットから指定名の新規ブランチを作成する"
    }

    fn reflog_action_help(&self) -> &'static str {
        "選択したコミットに対して行う操作をメニューから選ぶ"
    }

    // `gz commit` / `gz fixup`

    fn commit_about(&self) -> &'static str {
        "コミットするファイルを選択してコミットする"
    }

    fn commit_message_help(&self) -> &'static str {
        "コミットメッセージ（省略時は git がエディタを起動する）"
    }

    fn fixup_about(&self) -> &'static str {
        "修正対象のコミットを選択して fixup コミットを作成する"
    }

    fn fixup_squash_help(&self) -> &'static str {
        "fixup ではなく squash コミット（メッセージを結合する）を作成する"
    }

    // `gz merge` / `gz rebase` / `gz revert` / `gz status`

    fn merge_about(&self) -> &'static str {
        "ブランチを選択して merge する"
    }

    fn merge_no_ff_help(&self) -> &'static str {
        "fast-forward できる場合でもマージコミットを作成する"
    }

    fn merge_squash_help(&self) -> &'static str {
        "マージ結果を作業ツリー・index へ反映するだけでコミットしない"
    }

    fn merge_ff_only_help(&self) -> &'static str {
        "fast-forward できる場合のみ merge する"
    }

    fn rebase_about(&self) -> &'static str {
        "ブランチを選択して rebase する"
    }

    fn revert_about(&self) -> &'static str {
        "コミットを選択して打ち消す（revert コミットを作成する）"
    }

    fn revert_no_edit_help(&self) -> &'static str {
        "エディタを起動せず、git の既定メッセージのままコミットする"
    }

    fn status_about(&self) -> &'static str {
        "変更ファイルの状態を一覧し、選択したファイルに操作を行う"
    }

    // `gz diff`

    fn diff_about(&self) -> &'static str {
        "比較対象を選択して差分を表示する"
    }

    fn diff_staged_help(&self) -> &'static str {
        "ステージ済みの変更を対象にする（`git diff --staged` と同じ）"
    }

    fn diff_head_help(&self) -> &'static str {
        "HEAD と作業ツリーを比較する（ステージ済みの変更を含む）"
    }

    fn diff_upstream_help(&self) -> &'static str {
        "HEAD と upstream を比較する"
    }

    fn diff_branch_help(&self) -> &'static str {
        "ブランチを 2 回選択して比較する"
    }

    fn diff_commit_help(&self) -> &'static str {
        "コミットを 2 回選択して比較する"
    }

    // `gz fetch` / `gz pull` / `gz sync`

    fn fetch_about(&self) -> &'static str {
        "fetch の対象を決めて取得する"
    }

    fn fetch_prune_help(&self) -> &'static str {
        "リモートで削除されたブランチの追跡参照も掃除する"
    }

    fn fetch_siblings_help(&self) -> &'static str {
        "同じ階層に並ぶリポジトリも対象に含めて一括で取得する"
    }

    fn pull_about(&self) -> &'static str {
        "ブランチを選んで upstream へ追随させる（fast-forward のみ）"
    }

    fn sync_about(&self) -> &'static str {
        "現在のブランチを upstream と同期する"
    }

    fn sync_rebase_help(&self) -> &'static str {
        "upstream の上へ rebase して取り込む（履歴改変）"
    }

    fn sync_merge_help(&self) -> &'static str {
        "upstream を merge して取り込む"
    }

    // `gz worktree`

    fn worktree_about(&self) -> &'static str {
        "worktree を一覧・管理する（引数なしは一覧からパスを標準出力へ出す）"
    }

    fn worktree_add_about(&self) -> &'static str {
        "ブランチを選択して新しい worktree を作成する"
    }

    fn worktree_add_path_help(&self) -> &'static str {
        "作成する worktree のパス（ディレクトリ名の自動提案は行わない）"
    }

    fn worktree_remove_about(&self) -> &'static str {
        "worktree を選択して削除する（main worktree は候補に含めない）"
    }

    fn worktree_prune_about(&self) -> &'static str {
        "実体を失った worktree の管理情報を整理する"
    }

    fn worktree_add_no_install_help(&self) -> &'static str {
        "作成後の依存インストールを行わない（lockfile の走査も行わない）"
    }
}
