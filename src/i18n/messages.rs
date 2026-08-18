//! 言語ごとの文言を提供する trait。
//!
//! 骨格（[`Messages::language`]）に対し、コマンド別のアクセサ（`errors()` / `finder()` /
//! `pull()` …）を、文言の移行が進むたびに 1 つずつ追加していく
//! （design.md「文言 trait をどう分割するか」）。現時点で移行済みなのは
//! [`Messages::errors`]（[`crate::error::Error`] の表示）、
//! [`Messages::finder`]（選択 UI のプレビュー）、
//! [`Messages::confirm`]（破壊的操作の確認プロンプト）の共通レイヤーに加え、
//! [`Messages::common`]（複数コマンドで共有する語彙）と
//! `gz branch` / `gz cherry-pick` / `gz tag` / `gz reflog` / `gz restore` /
//! `gz revert` / `gz stash` / `gz commit` / `gz fixup` / `gz status` / `gz merge` /
//! `gz rebase` / `gz worktree` / `gz sync` / `gz diff` / `gz fetch` / `gz pull` /
//! `gz branch create` / `delete` / `cleanup`、および複数コマンドが共用するファイル選択・
//! merge / rebase の復帰メニュー・兄弟リポジトリ走査。
//!
//! `gz log` / `gz add` のように、文言がすべて [`CommonMessages`] で賄えるコマンドには専用の
//! trait を置かない（空の trait はコンパイル時の完全性に何も寄与しないため）。
//!
//! 唯一の例外が [`Messages::cli`]（`clap` のヘルプ）で、これはコマンドの実行時ではなく
//! **引数のパース時**に使われる。差し替えを行うのが `clap` の `Command` を組み立てる
//! 実行時コードであるため、ここだけは呼び忘れがコンパイルエラーにならない
//! （[`CliMessages`] の doc comment を参照）。

use std::path::Path;

use super::Language;

/// 言語ごとの文言一式。
///
/// 実装はフィールドを持たない ZST であり、[`Language::messages`] が返す
/// `&'static dyn Messages` を**値として引き回す**。グローバル（`OnceLock`）にしないのは、
/// 単体テストが並列実行されるため `ja` 前提のテストと `en` 前提のテストが同一プロセス内で
/// 干渉するため（`crate::git::exec` の `is_debug_enabled` が同じ理由で
/// `std::env::set_var` を避けている既存パターンの踏襲）。
///
/// `Sync` を要求するのは、skim のプレビュー生成が別スレッドから呼ばれるため。
/// `Debug` を要求するのは、この参照を持つ構造体（`FinderItem` 等）が `Debug` を
/// 導出できるようにするため。
pub trait Messages: Sync + std::fmt::Debug {
    /// この文言一式が表す言語。
    ///
    /// 子プロセス `git` へ言語を伝播する際（FR-26 の (B) 系）は、文言一式ではなく
    /// ここで取り出した [`Language`] だけを渡す。
    fn language(&self) -> Language;

    /// 複数のコマンドで共有する語彙。
    fn common(&self) -> &dyn CommonMessages;

    /// `clap` が組み立てるヘルプ（[`crate::cli::localized_command`]）の文言。
    fn cli(&self) -> &dyn CliMessages;

    /// [`crate::error::Error`] の表示を担う文言。
    fn errors(&self) -> &dyn ErrorMessages;

    /// 選択 UI（[`crate::finder`]）が自ら出す文言。
    fn finder(&self) -> &dyn FinderMessages;

    /// 破壊的操作の確認プロンプト（[`crate::commands::confirmation`]）の文言。
    fn confirm(&self) -> &dyn ConfirmMessages;

    /// `gz branch`（[`crate::commands::branch`]）の文言。
    fn branch(&self) -> &dyn BranchMessages;

    /// `gz branch create` / `delete` / `cleanup`（[`crate::commands::branch_manage`]）の文言。
    fn branch_manage(&self) -> &dyn BranchManageMessages;

    /// `gz cherry-pick`（[`crate::commands::cherry_pick`]）の文言。
    fn cherry_pick(&self) -> &dyn CherryPickMessages;

    /// ファイル選択（[`crate::commands::file_selection`]）の文言。
    fn file_selection(&self) -> &dyn FileSelectionMessages;

    /// `gz tag`（[`crate::commands::tag`]）の文言。
    fn tag(&self) -> &dyn TagMessages;

    /// `gz reflog`（[`crate::commands::reflog`]）の文言。
    fn reflog(&self) -> &dyn ReflogMessages;

    /// `gz restore`（[`crate::commands::restore`]）の文言。
    fn restore(&self) -> &dyn RestoreMessages;

    /// `gz revert`（[`crate::commands::revert`]）の文言。
    fn revert(&self) -> &dyn RevertMessages;

    /// `gz stash`（[`crate::commands::stash`]）の文言。
    fn stash(&self) -> &dyn StashMessages;

    /// `gz commit`（[`crate::commands::commit`]）の文言。
    fn commit(&self) -> &dyn CommitMessages;

    /// `gz fixup`（[`crate::commands::fixup`]）の文言。
    fn fixup(&self) -> &dyn FixupMessages;

    /// `gz status`（[`crate::commands::status`]）の文言。
    fn status(&self) -> &dyn StatusMessages;

    /// `gz merge`（[`crate::commands::merge`]）の文言。
    fn merge(&self) -> &dyn MergeMessages;

    /// `gz rebase`（[`crate::commands::rebase`]）の文言。
    fn rebase(&self) -> &dyn RebaseMessages;

    /// merge / rebase の復帰メニュー（[`crate::commands::in_progress`]）の文言。
    ///
    /// [`Messages::merge`] / [`Messages::rebase`] の下へ入れず独立したアクセサにするのは、
    /// この復帰メニューが `gz merge` / `gz rebase` だけでなく `gz pull` / `gz sync` からも
    /// 委譲されるため。どちらか一方のコマンドの trait にぶら下げると、rebase 進行中に出す
    /// 文言を `messages.merge()` から引くような対応が生まれ、trait の粒度（モジュール単位）が
    /// 崩れる。
    fn in_progress(&self) -> &dyn InProgressMessages;

    /// `gz worktree`（[`crate::commands::worktree`]）の文言。
    fn worktree(&self) -> &dyn WorktreeMessages;

    /// `gz sync`（[`crate::commands::sync`]）の文言。
    fn sync(&self) -> &dyn SyncMessages;

    /// `gz diff`（[`crate::commands::diff`]）の文言。
    fn diff(&self) -> &dyn DiffMessages;

    /// `gz fetch`（[`crate::commands::fetch`]）の文言。
    fn fetch(&self) -> &dyn FetchMessages;

    /// `gz pull`（[`crate::commands::pull`]）の文言。
    fn pull(&self) -> &dyn PullMessages;
}

/// 複数のコマンドで共有する語彙（design.md の `Messages::common`）。
///
/// 置くのは**同じ文言が複数のコマンドに現れるもの**だけに限る。コマンド固有の語彙まで
/// ここへ集めると、`messages.common().…` という呼び出しから文脈が読めなくなり、
/// trait をコマンド単位に分けた意味が失われるため。
pub trait CommonMessages: Sync + std::fmt::Debug {
    /// 標準出力への書き込みに失敗したことを伝える（`gz log` / `gz tag` / `gz reflog` 等）。
    fn stdout_write_failed(&self) -> &'static str;

    /// 標準エラー出力への書き込みに失敗したことを伝える（`gz reflog` 等）。
    fn stderr_write_failed(&self) -> &'static str;

    /// コミット履歴を読み取れなかったことを伝える（`gz log` / `gz cherry-pick` 等）。
    fn commit_history_read_failed(&self) -> &'static str;

    /// コミットハッシュを解釈できなかったことを伝える（`gz revert` / `gz fixup`）。
    fn commit_hash_parse_failed(&self, id: &str) -> String;

    /// コミットオブジェクトを読み取れなかったことを伝える（`gz revert` / `gz fixup`）。
    fn commit_read_failed(&self, id: &str) -> String;

    /// 変更ファイル一覧を読み取れなかったことを伝える（`gz add` / `gz restore` 等）。
    fn changed_files_read_failed(&self) -> &'static str;

    /// ブランチ一覧を読み取れなかったことを伝える（`gz branch` 等）。
    fn branch_list_read_failed(&self) -> &'static str;

    /// stash 一覧を読み取れなかったことを伝える（`gz stash` / `gz status`）。
    fn stash_list_read_failed(&self) -> &'static str;

    /// タグ一覧を読み取れなかったことを伝える（`gz tag` / `gz branch create`）。
    fn tag_list_read_failed(&self) -> &'static str;

    /// worktree 一覧を読み取れなかったことを伝える（`gz worktree` / `gz branch delete`）。
    fn worktree_list_read_failed(&self) -> &'static str;

    /// リモート一覧を読み取れなかったことを伝える（`gz sync` / `gz fetch`）。
    fn remote_list_read_failed(&self) -> &'static str;

    /// 現在のブランチを読み取れなかったことを伝える（`gz status` / `gz sync` / `gz diff`）。
    fn current_branch_read_failed(&self) -> &'static str;

    /// upstream の設定を読み取れなかったことを伝える（`gz status` / `gz sync` / `gz diff`）。
    fn upstream_read_failed(&self, branch: &str) -> String;

    /// upstream に対する進み・遅れを算出できなかったことを伝える（`gz status` / `gz sync`）。
    fn ahead_behind_failed(&self, reference: &str) -> String;

    /// detached HEAD には upstream が無いことと、次に取れる操作を伝える。
    ///
    /// `gz sync` と `gz diff --upstream` はどちらも「現在のブランチの upstream」を対象に
    /// するため、同じ案内を出す。案内に含まれる `gz branch` はユーザーがそのまま打ち込む
    /// コマンドであるため翻訳しない（design.md「翻訳しないもの」）。
    fn detached_head_without_upstream(&self) -> &'static str;

    /// upstream が設定されていないことと、設定する手立てを伝える
    /// （`gz sync` / `gz diff --upstream`）。
    ///
    /// 案内に含まれる `git push -u` / `git branch --set-upstream-to` はユーザーがそのまま
    /// 打ち込むコマンド列であるため翻訳しない。
    fn upstream_not_configured(&self, branch: &str) -> String;

    /// rebase が履歴改変であることを示す注記。
    ///
    /// `gz rebase` の確認プロンプトと `gz sync --rebase` の確認プロンプトが同じ注記を出す。
    /// どちらか一方のコマンドの trait にぶら下げると、`gz sync` の注記を
    /// `messages.rebase()` から引くことになり trait の粒度（モジュール単位）が崩れるため、
    /// 共有語彙としてここへ置く（[`Messages::in_progress`] と同じ判断）。
    fn history_rewrite_note(&self) -> &'static str;

    /// git コマンドの実行に失敗したことを伝える。
    ///
    /// `command` には実行した git のサブコマンド（`git add` 等）が入る。git のサブコマンド名は
    /// 翻訳しない（design.md「翻訳しないもの」）。
    fn command_run_failed(&self, command: &str) -> String;

    /// 直列実行の結果（成功件数と失敗件数）を伝える集計。
    ///
    /// `gz fetch --siblings`（リポジトリ単位）と `gz pull`（ブランチ単位）が同じ形式で
    /// 出すため共有語彙に置く。何に対して実行したのかは直前の進捗表示で分かるため、
    /// 対象の呼称は含めない。
    fn run_summary(&self, succeeded: usize, failed: usize) -> String;

    /// ブランチの切り替えに失敗したことを伝える。
    ///
    /// `gz branch`（切替そのもの）と `gz branch create --switch`（作成後の切替）が
    /// 同じ操作を行うため共有語彙に置く。
    fn switch_failed(&self, target: &str) -> String;

    /// 失敗した対象の名前を集計へ添える節。
    ///
    /// [`CommonMessages::run_summary`] の直後へ連結される。日本語は全角括弧が、英語は
    /// 空白が区切りを兼ねるため、**区切りは装飾ではなく文言の一部**として各言語が持つ。
    fn failed_targets(&self, names: &str) -> String;
}

/// `clap` が組み立てるヘルプ（`gz --help` / `gz <サブコマンド> --help`）の文言。
///
/// ほかの文言 trait と違い、**差し替え漏れがコンパイルエラーにならない**唯一の trait。
/// `clap` の `Command` を組み立てるのは実行時であり、[`crate::cli::localized_command`] が
/// メソッドを呼び忘れても型検査は通ってしまう（derive のリテラル＝英語がそのまま残る）。
/// そのため `tests/cli.rs` の統合テストで全サブコマンド・全オプションの説明を突き合わせて
/// 担保する（design.md「差し替え漏れはコンパイルエラーにならない」）。
///
/// **翻訳しないもの**: サブコマンド名（`branch` / `log` …）、オプションの長短名
/// （`--all` / `-a`）、値の名前（`<NAME>` / `<BRANCH>`）、`clap` 自身が出す見出しと
/// パーサエラー（`Usage:` / `Options:` / `Commands:`）。後者は `clap` に差し替える機構が
/// 無く、requirements.md のスコープ外である。
///
/// 一覧で読まれるヘルプであるため、いずれの文言も 1 行に収まる長さとし、**末尾に句点を
/// 置かない**（`clap` の既定のヘルプが句点を持たない書式であるため）。
pub trait CliMessages: Sync + std::fmt::Debug {
    // `gz` 自身

    /// `gz` 自体の説明（`gz --help` の先頭行）。
    fn about(&self) -> &'static str;

    /// `--lang` の説明。
    fn lang_help(&self) -> &'static str;

    // `gz branch`（切替と管理サブコマンド）

    /// `gz branch` の説明。
    fn branch_about(&self) -> &'static str;

    /// `gz branch --all` の説明。
    fn branch_all_help(&self) -> &'static str;

    /// `gz branch create` の説明。
    fn branch_create_about(&self) -> &'static str;

    /// `gz branch create <NAME>` の説明。
    fn branch_create_name_help(&self) -> &'static str;

    /// `gz branch create --switch` の説明。
    fn branch_create_switch_help(&self) -> &'static str;

    /// `gz branch delete` の説明。
    fn branch_delete_about(&self) -> &'static str;

    /// `gz branch delete --force` の説明。
    fn branch_delete_force_help(&self) -> &'static str;

    /// `gz branch delete --into` の説明。
    fn branch_delete_into_help(&self) -> &'static str;

    /// `gz branch cleanup` の説明。
    fn branch_cleanup_about(&self) -> &'static str;

    /// `gz branch cleanup --into` の説明。
    ///
    /// [`CliMessages::branch_delete_into_help`] と同じ内容だが、別のオプションであるため
    /// 別のメソッドとして持つ（片方だけ言い回しを変えられるようにするため）。
    fn branch_cleanup_into_help(&self) -> &'static str;

    // `gz log` / `gz cherry-pick` / `gz restore` / `gz add`

    /// `gz log` の説明。
    fn log_about(&self) -> &'static str;

    /// `gz log --limit` の説明。
    fn log_limit_help(&self) -> &'static str;

    /// `gz cherry-pick` の説明。
    fn cherry_pick_about(&self) -> &'static str;

    /// `gz cherry-pick --branch` の説明。
    fn cherry_pick_branch_help(&self) -> &'static str;

    /// `gz restore` の説明。
    fn restore_about(&self) -> &'static str;

    /// `gz restore --source` の説明。
    fn restore_source_help(&self) -> &'static str;

    /// `gz restore --staged` の説明。
    fn restore_staged_help(&self) -> &'static str;

    /// `gz add` の説明。
    fn add_about(&self) -> &'static str;

    // `gz stash`

    /// `gz stash` の説明。
    fn stash_about(&self) -> &'static str;

    /// `gz stash push` の説明。
    fn stash_push_about(&self) -> &'static str;

    /// `gz stash push --message` の説明。
    fn stash_push_message_help(&self) -> &'static str;

    /// `gz stash push --include-untracked` の説明。
    fn stash_push_include_untracked_help(&self) -> &'static str;

    /// `gz stash apply` の説明。
    fn stash_apply_about(&self) -> &'static str;

    /// `gz stash pop` の説明。
    fn stash_pop_about(&self) -> &'static str;

    /// `gz stash drop` の説明。
    fn stash_drop_about(&self) -> &'static str;

    // `gz tag` / `gz reflog`

    /// `gz tag` の説明。
    fn tag_about(&self) -> &'static str;

    /// `gz tag --switch` の説明。
    fn tag_switch_help(&self) -> &'static str;

    /// `gz tag --diff` の説明。
    fn tag_diff_help(&self) -> &'static str;

    /// `gz reflog` の説明。
    fn reflog_about(&self) -> &'static str;

    /// `gz reflog --restore` の説明。
    fn reflog_restore_help(&self) -> &'static str;

    // `gz commit` / `gz fixup`

    /// `gz commit` の説明。
    fn commit_about(&self) -> &'static str;

    /// `gz commit --message` の説明。
    fn commit_message_help(&self) -> &'static str;

    /// `gz fixup` の説明。
    fn fixup_about(&self) -> &'static str;

    /// `gz fixup --squash` の説明。
    fn fixup_squash_help(&self) -> &'static str;

    // `gz merge` / `gz rebase` / `gz revert` / `gz status`

    /// `gz merge` の説明。
    fn merge_about(&self) -> &'static str;

    /// `gz merge --no-ff` の説明。
    fn merge_no_ff_help(&self) -> &'static str;

    /// `gz merge --squash` の説明。
    fn merge_squash_help(&self) -> &'static str;

    /// `gz merge --ff-only` の説明。
    fn merge_ff_only_help(&self) -> &'static str;

    /// `gz rebase` の説明。
    fn rebase_about(&self) -> &'static str;

    /// `gz revert` の説明。
    fn revert_about(&self) -> &'static str;

    /// `gz revert --no-edit` の説明。
    fn revert_no_edit_help(&self) -> &'static str;

    /// `gz status` の説明。
    fn status_about(&self) -> &'static str;

    // `gz diff`

    /// `gz diff` の説明。
    fn diff_about(&self) -> &'static str;

    /// `gz diff --staged` の説明。
    fn diff_staged_help(&self) -> &'static str;

    /// `gz diff --head` の説明。
    fn diff_head_help(&self) -> &'static str;

    /// `gz diff --upstream` の説明。
    fn diff_upstream_help(&self) -> &'static str;

    /// `gz diff --branch` の説明。
    fn diff_branch_help(&self) -> &'static str;

    /// `gz diff --commit` の説明。
    fn diff_commit_help(&self) -> &'static str;

    // `gz fetch` / `gz pull` / `gz sync`

    /// `gz fetch` の説明。
    fn fetch_about(&self) -> &'static str;

    /// `gz fetch --prune` の説明。
    fn fetch_prune_help(&self) -> &'static str;

    /// `gz fetch --siblings` の説明。
    fn fetch_siblings_help(&self) -> &'static str;

    /// `gz pull` の説明。
    fn pull_about(&self) -> &'static str;

    /// `gz sync` の説明。
    fn sync_about(&self) -> &'static str;

    /// `gz sync --rebase` の説明。
    fn sync_rebase_help(&self) -> &'static str;

    /// `gz sync --merge` の説明。
    fn sync_merge_help(&self) -> &'static str;

    // `gz worktree`

    /// `gz worktree` の説明。
    fn worktree_about(&self) -> &'static str;

    /// `gz worktree add` の説明。
    fn worktree_add_about(&self) -> &'static str;

    /// `gz worktree add <PATH>` の説明。
    fn worktree_add_path_help(&self) -> &'static str;

    /// `gz worktree remove` の説明。
    fn worktree_remove_about(&self) -> &'static str;

    /// `gz worktree prune` の説明。
    fn worktree_prune_about(&self) -> &'static str;
}

/// `gz branch`（[`crate::commands::branch`]）の文言。
pub trait BranchMessages: Sync + std::fmt::Debug {
    /// 選択されたブランチが候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;

    /// リモート追跡ブランチから追跡先のブランチ名を決められなかったことを伝える。
    fn tracking_target_undetermined(&self, name: &str) -> String;
}

/// `gz branch create` / `delete` / `cleanup`（[`crate::commands::branch_manage`]）の文言。
///
/// 切替（`gz branch`）を担う [`BranchMessages`] とは別の trait に分ける。これまでの方針が
/// **trait の粒度＝モジュール単位**（`branch.rs` と `branch_manage.rs` は別モジュール）で
/// あることに加え、管理操作の文言は削除の確認・警告が中心で切替とは語彙が重ならないため。
/// 両者で完全に一致する「切り替えに失敗した」は [`CommonMessages::switch_failed`] が持つ。
///
/// 候補行のうち `merged` / `unmerged` は git の語彙であるため翻訳せず、この trait にも
/// 含めない（design.md「翻訳しないもの」）。一方 upstream の有無と最終更新日時の表示は
/// fuzgit 自身の文言であるため、`gz fetch` の「すべてのリモート」行と同じ扱いで翻訳する。
pub trait BranchManageMessages: Sync + std::fmt::Debug {
    /// 選択された作成元が候補一覧に見つからなかったことを伝える（`create`）。
    fn base_selection_not_found(&self, selected: &str) -> String;

    /// ブランチの作成に失敗したことを伝える（`create`）。
    fn creation_failed(&self, name: &str) -> String;

    /// 作成したブランチと、その作成元を伝える（`create`）。
    ///
    /// `base` には候補の finder キー（`branch main` / `tag v1.0`）が入る。種別の呼称は
    /// git の語彙であるため翻訳しない。
    fn created(&self, name: &str, base: &str) -> String;

    /// 作成したブランチへ切り替える手立てを伝える（`create`。`--switch` なしの場合のみ）。
    ///
    /// 案内に含まれる `git switch` はユーザーがそのまま打ち込むコマンドであるため翻訳しない。
    fn switch_hint(&self, name: &str) -> String;

    /// 候補行に示す upstream（リモート追跡ブランチ）の節（`delete` / `cleanup`）。
    ///
    /// `upstream` には表示用の追跡ブランチ名が入る。
    fn tracking(&self, upstream: &str) -> String;

    /// 候補行に示す「upstream が設定されていない」の節（`delete` / `cleanup`）。
    fn no_tracking(&self) -> &'static str;

    /// 候補行に示す「最終更新日時を取得できなかった」の節（`delete` / `cleanup`）。
    ///
    /// 取得できた日時は git が出力した相対表記をそのまま載せるため文言を持たない。
    fn unknown_date(&self) -> &'static str;

    /// 取り込み済みかどうかの判定に失敗したことを伝える（`delete` / `cleanup`）。
    fn merged_state_read_failed(&self) -> &'static str;

    /// ブランチの最終更新日時を読み取れなかったことを伝える（`delete` / `cleanup`）。
    fn activity_read_failed(&self) -> &'static str;

    /// `--into` に指定された名前がブランチ一覧に無いことを伝える（`delete` / `cleanup`）。
    ///
    /// オプション名（`--into`）は翻訳しない。
    fn unknown_merge_base(&self, into: &str) -> String;

    /// 選択されたブランチが候補一覧に見つからなかったことを伝える（`delete` / `cleanup`）。
    ///
    /// `names` には見つからなかったブランチ名を `, ` で連結したものが入る。
    fn selection_not_found(&self, names: &str) -> String;

    /// ブランチの削除に失敗したことを伝える（`delete` / `cleanup`）。
    fn deletion_failed(&self) -> &'static str;

    /// 削除候補一覧の上部に固定表示する操作説明（`delete`）。
    ///
    /// キー名（`Tab` / `Enter`）は skim のキー表記であり翻訳しない。
    fn delete_header(&self) -> &'static str;

    /// 削除できる候補が 1 件も無いことと、候補から外れる条件を伝える（`delete`）。
    fn no_delete_candidates(&self) -> &'static str;

    /// merged でないブランチが `--force` なしで選ばれたため実行を止めたことを伝える（`delete`）。
    ///
    /// `names` には該当するブランチ名を `, ` で連結したものが入る。件数は 1 件のことも
    /// 複数件のこともあるため、数に依存しない言い回しにする。案内に含まれる
    /// `gz branch delete --force` はユーザーがそのまま打ち込むコマンド列であるため翻訳しない。
    fn unmerged_rejection(&self, names: &str) -> String;

    /// 削除の確認プロンプトに示す説明（`delete` / `cleanup`）。
    ///
    /// 対象一覧は [`crate::commands::confirmation::confirm`] が候補行をそのまま並べるため
    /// ここには含まない。
    fn delete_confirmation(&self) -> &'static str;

    /// merged でないブランチを含む場合の確認プロンプトの説明（`delete`）。
    ///
    /// [`BranchManageMessages::delete_confirmation`] に、失われるものを名指しする警告を
    /// 加えた複数行の説明を返す。
    fn unmerged_confirmation(&self, names: &str) -> String;

    /// 整理候補一覧の上部に固定表示する操作説明（`cleanup`）。
    ///
    /// キー名（`Tab` / `Enter`）は skim のキー表記であり翻訳しない。
    fn cleanup_header(&self) -> &'static str;

    /// 整理できる候補が 1 件も無いことを伝える（`cleanup`）。
    ///
    /// `merged` は git の語彙であるため翻訳しない。
    fn no_cleanup_candidates(&self) -> &'static str;
}

/// `gz cherry-pick`（[`crate::commands::cherry_pick`]）の文言。
pub trait CherryPickMessages: Sync + std::fmt::Debug {
    /// 選択されたコミットが候補一覧に見つからなかったことを伝える。
    ///
    /// `hashes` には見つからなかったハッシュを `, ` で連結したものが入る。
    fn selection_not_found(&self, hashes: &str) -> String;

    /// cherry-pick が中断された（コンフリクト等）際にユーザーが取れる操作を案内する。
    ///
    /// 案内に含まれる `git cherry-pick --continue` / `--abort` は、ユーザーがそのまま
    /// 打ち込むコマンド列であるため翻訳しない（design.md「翻訳しないもの」）。
    fn resolution_hint(&self) -> &'static str;
}

/// ファイル選択（[`crate::commands::file_selection`]）の文言。
pub trait FileSelectionMessages: Sync + std::fmt::Debug {
    /// 選択されたファイルが候補一覧に見つからなかったことを伝える。
    ///
    /// `paths` には見つからなかったパスを `, ` で連結したものが入る。
    fn selection_not_found(&self, paths: &str) -> String;
}

/// `gz tag`（[`crate::commands::tag`]）の文言。
pub trait TagMessages: Sync + std::fmt::Debug {
    /// `--switch` と `--diff` が同時に指定されたことを伝える。
    ///
    /// オプション名は翻訳しない（design.md「翻訳しないもの」）。
    fn conflicting_actions(&self) -> &'static str;

    /// 選択されたタグが候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;

    /// タグの指すコミットへの切り替えに失敗したことを伝える。
    fn switch_failed(&self, name: &str) -> String;

    /// タグとの差分表示に失敗したことを伝える。
    fn diff_failed(&self, name: &str) -> String;
}

/// `gz reflog`（[`crate::commands::reflog`]）の文言。
pub trait ReflogMessages: Sync + std::fmt::Debug {
    /// reflog を読み取れなかったことを伝える。
    fn read_failed(&self) -> &'static str;

    /// 選択された reflog エントリが候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;

    /// ブランチの作成に失敗したことを伝える。
    fn branch_creation_failed(&self, name: &str) -> String;

    /// 作成したブランチと、その指すコミットを知らせる。
    ///
    /// `git branch` は成功時に何も出力しないため、fuzgit が結果を補って伝える。
    fn branch_created(&self, name: &str, id: &str) -> String;
}

/// `gz restore`（[`crate::commands::restore`]）の文言。
pub trait RestoreMessages: Sync + std::fmt::Debug {
    /// リビジョンに含まれるファイル一覧を読み取れなかったことを伝える。
    fn revision_files_read_failed(&self, revision: &str) -> String;

    /// 作業ツリーの変更を破棄することへの同意を求める見出し。
    ///
    /// 対象のパスは [`crate::commands::confirmation::confirm`] が別途列挙するため、
    /// ここには件数だけを含める。
    fn discard_confirmation(&self, count: usize) -> String;

    /// 作業ツリーを別のリビジョンの内容で上書きすることへの同意を求める見出し。
    ///
    /// `revision` にはユーザーが `--source` へ指定した文字列をそのまま示す
    /// （解決済みハッシュより読み取りやすいため）。
    fn overwrite_confirmation(&self, count: usize, revision: &str) -> String;
}

/// `gz revert`（[`crate::commands::revert`]）の文言。
pub trait RevertMessages: Sync + std::fmt::Debug {
    /// 選択されたコミットが候補一覧に見つからなかったことを伝える。
    ///
    /// `hashes` には見つからなかったハッシュを `, ` で連結したものが入る。
    fn selection_not_found(&self, hashes: &str) -> String;

    /// revert が中断された（コンフリクト等）際にユーザーが取れる操作を案内する。
    ///
    /// 案内に含まれる `git revert --continue` / `--abort` は、ユーザーがそのまま打ち込む
    /// コマンド列であるため翻訳しない（design.md「翻訳しないもの」）。
    fn resolution_hint(&self) -> &'static str;

    /// 選択にマージコミットが含まれており revert できないことを伝える。
    ///
    /// 対象のコミットと素の git で実行するコマンド列は呼び出し側が続けて列挙するため、
    /// ここが返すのは理由と次の操作への導入だけ。
    fn merge_commit_selected(&self) -> &'static str;
}

/// `gz stash`（[`crate::commands::stash`]）の文言。
pub trait StashMessages: Sync + std::fmt::Debug {
    /// stash を破棄することへの同意を求める見出し。
    ///
    /// 対象の stash は [`crate::commands::confirmation::confirm`] が別途列挙する。
    fn drop_confirmation(&self) -> &'static str;

    /// 選択された stash が候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;
}

/// `gz commit`（[`crate::commands::commit`]）の文言。
pub trait CommitMessages: Sync + std::fmt::Debug {
    /// 候補一覧の上部に固定表示する操作説明。
    ///
    /// skim はフラットな 1 本のリストであり、リスト途中に「Staged」等の見出しを挟めない。
    /// 事前選択されている理由（ステージ済み）をここで補う。ヘッダーは候補リストの幅で
    /// 打ち切られるため、行頭の状態コード（`git status` と同じ表記）の説明までは載せない。
    /// キー名（`Tab` / `Enter`）は端末が送るキーの名前であり翻訳しない。
    fn header(&self) -> &'static str;

    /// メッセージ入力を git（エディタ）に委ねたコミットが失敗したときに添える案内。
    ///
    /// 失敗理由はメッセージが空だった場合に限らない（マージ中の partial commit 拒否、
    /// フックによる拒否など）ため、原因を断定せず条件付きで示す。エディタが即座に終了する
    /// 設定（待機オプションの無い GUI エディタ）はユーザーが自力で原因に辿り着きにくいため、
    /// 次に取れる操作まで書く。
    ///
    /// 案内に含まれる `gz commit -m` / `code --wait` / 環境変数名は、ユーザーがそのまま
    /// 打ち込む・設定するものであるため翻訳しない（design.md「翻訳しないもの」）。
    fn editor_hint(&self) -> &'static str;

    /// 未追跡ファイルの事前ステージに失敗したことを伝える。
    ///
    /// 実行した git のサブコマンド名（`git add`）は翻訳しない。
    fn untracked_stage_failed(&self) -> &'static str;
}

/// `gz fixup`（[`crate::commands::fixup`]）の文言。
///
/// `label` には作成するコミットの種類（`fixup` / `squash`）が入る。これは作成される
/// コミットメッセージの接頭辞と同じ綴りであるため翻訳しない。
pub trait FixupMessages: Sync + std::fmt::Debug {
    /// ステージ済みの変更を読み取れなかったことを伝える。
    fn staged_changes_read_failed(&self) -> &'static str;

    /// ステージ済みの変更が無く実行できないことと、次に取れる操作を伝える。
    ///
    /// `git commit --fixup` は index の内容をコミットするため、ステージ済みの変更が無いと
    /// 対象コミットを選んでも空コミットとして拒否される。原因と次に取れる操作を示す。
    fn staged_required(&self, label: &str) -> String;

    /// 選択されたコミットが候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;

    /// fixup / squash コミットの作成に失敗したことを伝える。
    fn commit_creation_failed(&self, label: &str) -> String;

    /// 作成したコミットを履歴へ取り込む手順を案内する。
    ///
    /// `start` には rebase の起点（`<hash>^` / `--root`）が入る。案内に含まれる
    /// `git rebase -i --autosquash` はユーザーがそのまま打ち込むコマンド列であるため
    /// 翻訳しない。
    fn autosquash_hint(&self, start: &str) -> String;

    /// 起点が `--root` になる理由（対象コミットに親が無い）を補う注記。
    ///
    /// 本文と注記を区切る改行は**装飾**であり呼び出し側が付ける
    /// （[`FinderMessages::truncation_notice`] と同じ扱い）。
    fn root_start_note(&self) -> &'static str;
}

/// `gz status`（[`crate::commands::status`]）の文言。
pub trait StatusMessages: Sync + std::fmt::Debug {
    /// 変更が 1 件も無いことを伝える。
    fn clean(&self) -> &'static str;

    /// 「選択したファイルをステージする」アクションの表示。
    ///
    /// アクションメニューの表示はいずれも**候補行ではなく fuzgit 自身の説明**であるため
    /// 翻訳する。括弧内の git コマンド名は、実際に何が実行されるのかを示すため翻訳しない。
    /// 照合に使う [`crate::commands::status`] のキーは表示と分かれており、翻訳の影響を受けない。
    fn add_action(&self) -> &'static str;

    /// 「選択したファイルの変更を破棄する」アクションの表示。
    fn restore_action(&self) -> &'static str;

    /// 「選択したファイルを stash へ退避する」アクションの表示。
    fn stash_action(&self) -> &'static str;

    /// 「選択したファイルをコミットする」アクションの表示。
    fn commit_action(&self) -> &'static str;

    /// 「選択したファイルのパスを標準出力へ出力する」アクションの表示。
    fn print_action(&self) -> &'static str;

    /// 選択されたアクションがメニューに見つからなかったことを伝える。
    fn menu_selection_not_found(&self, selected: &str) -> String;
}

/// `gz merge`（[`crate::commands::merge`]）の文言。
pub trait MergeMessages: Sync + std::fmt::Debug {
    /// `--no-ff` / `--squash` / `--ff-only` が同時に指定されたことを伝える。
    ///
    /// オプション名は翻訳しない（design.md「翻訳しないもの」）。
    fn conflicting_modes(&self) -> &'static str;

    /// merge 対象になるブランチが 1 件も無いことを伝える。
    ///
    /// 一般の「選択できる候補がありません」では、候補から現在のブランチを除いた結果である
    /// ことが分からないため、必要な候補の条件まで示す。
    fn no_candidates(&self) -> &'static str;

    /// 選択されたブランチが候補一覧に見つからなかったことを伝える。
    ///
    /// [`BranchMessages::selection_not_found`] と同じ内容だが、選択対象の語彙は各コマンドの
    /// trait が持つ（[`CherryPickMessages::selection_not_found`] と
    /// [`RevertMessages::selection_not_found`] も同様）。呼び出し側から
    /// `messages.merge().…` と読めることを優先し、[`CommonMessages`] へは集約しない。
    fn selection_not_found(&self, selected: &str) -> String;

    /// 取り込まれるコミット数を数えられなかったことを伝える。
    fn merged_commit_count_failed(&self, branch: &str) -> String;

    /// merge の実行に失敗したことを伝える。
    fn merge_failed(&self, branch: &str) -> String;

    /// 確認プロンプトの見出し（対象のブランチと、取り込まれるコミット数）。
    ///
    /// 実行するコマンドとコンフリクト予測は別のメソッドが返し、行を区切る改行は
    /// **装飾**として呼び出し側が付ける（[`FinderMessages::truncation_notice`] と同じ扱い）。
    fn confirmation(&self, branch: &str, count: usize) -> String;

    /// 確認プロンプトに添える、実際に実行するコマンドの行。
    ///
    /// `command` には実行する引数配列から組み立てた `git merge …` が入る。コマンド列は
    /// 翻訳しない（design.md「翻訳しないもの」）。
    fn command_line(&self, command: &str) -> String;

    /// コンフリクトなく merge できる見込みであることを示す注記。
    fn prediction_clean(&self) -> &'static str;

    /// コンフリクトが予測されるファイルの件数を示す注記。
    fn prediction_conflicted(&self, count: usize) -> String;

    /// コンフリクトは予測されたが、対象のファイル名が得られなかったことを示す注記。
    fn prediction_unnamed(&self) -> &'static str;

    /// コンフリクト予測そのものが得られなかったことを示す注記。
    ///
    /// 案内に含まれる `git merge-tree --write-tree` と Git のバージョンは、原因を確かめる
    /// ための情報であるため翻訳しない。
    fn prediction_unavailable(&self) -> &'static str;
}

/// `gz rebase`（[`crate::commands::rebase`]）の文言。
pub trait RebaseMessages: Sync + std::fmt::Debug {
    /// base になるブランチが 1 件も無いことを伝える。
    fn no_candidates(&self) -> &'static str;

    /// 選択されたブランチが候補一覧に見つからなかったことを伝える。
    ///
    /// 各コマンドの trait が自分の選択対象の語彙を持つ理由は
    /// [`MergeMessages::selection_not_found`] と同じ。
    fn selection_not_found(&self, selected: &str) -> String;

    /// replay されるコミット数を数えられなかったことを伝える。
    fn replayed_commit_count_failed(&self, base: &str) -> String;

    /// rebase の実行に失敗したことを伝える。
    fn rebase_failed(&self, base: &str) -> String;

    /// 確認プロンプトの見出し（base と、replay されるコミット数）。
    ///
    /// 履歴改変の注記は [`CommonMessages::history_rewrite_note`]（`gz sync --rebase` と共有）
    /// が返し、区切りの改行は**装飾**として呼び出し側が付ける。
    fn confirmation(&self, base: &str, count: usize) -> String;
}

/// merge / rebase 進行中の復帰メニュー（[`crate::commands::in_progress`]）の文言。
///
/// `operation` には進行中の操作の呼称（`merge` / `rebase`）が入る。これは実行する git の
/// サブコマンド名と同じ綴りであるため翻訳しない（design.md「翻訳しないもの」）。
pub trait InProgressMessages: Sync + std::fmt::Debug {
    /// 「コンフリクトファイルを確認して解決済みにする」項目の表示。
    ///
    /// メニューの表示はいずれも**候補行ではなく fuzgit 自身の説明**であるため翻訳する。
    /// 括弧内の git コマンド名は、実際に何が実行されるのかを示すため翻訳しない。
    /// 照合に使う [`crate::commands::in_progress`] のキーは表示と分かれており、
    /// 翻訳の影響を受けない（[`StatusMessages::add_action`] と同じ扱い）。
    fn conflicts_action(&self) -> &'static str;

    /// 「処理を再開する」項目の表示。
    fn continue_action(&self, operation: &str) -> String;

    /// 「現在のコミットを飛ばす」項目の表示（rebase のみ）。
    fn skip_action(&self) -> &'static str;

    /// 「処理を中止する」項目の表示。
    fn abort_action(&self, operation: &str) -> String;

    /// 選択された項目がメニューに見つからなかったことを伝える。
    fn menu_selection_not_found(&self, selected: &str) -> String;

    /// 中止することで何が失われるのかを示す、確認プロンプトの見出し。
    fn abort_confirmation(&self, operation: &str) -> String;

    /// コンフリクトファイル一覧の上部に固定表示する操作説明。
    ///
    /// キー名（`Tab` / `Enter`）は端末が送るキーの名前であり翻訳しない
    /// （[`CommitMessages::header`] と同じ扱い）。
    fn conflicts_header(&self) -> &'static str;

    /// コンフリクト中のファイル一覧を読み取れなかったことを伝える。
    fn conflicts_read_failed(&self) -> &'static str;

    /// 未解決のファイルが 1 件も無いことと、次に取れる操作を伝える。
    fn no_conflicts(&self) -> &'static str;

    /// 解決済みとしての stage に失敗したことを伝える。
    ///
    /// 実行した git のサブコマンド名（`git add`）は翻訳しない。
    fn stage_failed(&self) -> &'static str;
}

/// `gz worktree`（[`crate::commands::worktree`]）の文言。
///
/// 候補行に並ぶ種別・状態の語（`main` / `linked` / `detached` / `bare` / `locked` /
/// `prunable`）は、`git worktree list --porcelain` の属性名そのものであるため翻訳せず、
/// この trait にも含めない（design.md「翻訳しないもの」）。
pub trait WorktreeMessages: Sync + std::fmt::Debug {
    /// 選択された worktree が候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;

    /// `gz worktree add` に渡されたパスを UTF-8 として解釈できなかったことを伝える。
    fn path_not_utf8(&self, path: &Path) -> String;

    /// 新しい worktree に割り当てられるローカルブランチが 1 件も無いことを伝える。
    fn no_available_branch(&self) -> &'static str;

    /// 選択されたブランチが候補一覧に見つからなかったことを伝える。
    fn branch_selection_not_found(&self, selected: &str) -> String;

    /// worktree の作成に失敗したことを伝える。
    fn creation_failed(&self, path: &str) -> String;

    /// 削除できる worktree が 1 件も無いことを伝える。
    fn no_removable(&self) -> &'static str;

    /// worktree を削除することへの同意を求める見出し。
    ///
    /// 対象の worktree は [`crate::commands::confirmation::confirm`] が別途列挙する。
    fn remove_confirmation(&self) -> &'static str;

    /// worktree の削除に失敗したことを伝える。
    fn removal_failed(&self, path: &str) -> String;

    /// 整理対象の確認（ドライラン）に失敗したことを伝える。
    fn prune_targets_read_failed(&self) -> &'static str;

    /// worktree の管理情報を整理することへの同意を求める見出し。
    ///
    /// 対象として列挙するのは `git worktree prune --dry-run --verbose` の報告そのもの
    /// であり、fuzgit は解釈も翻訳もしない（design.md「`capture_git_stderr_in` を (B) と
    /// する根拠」）。
    fn prune_confirmation(&self) -> &'static str;

    /// 整理の実行に失敗したことを伝える。
    fn prune_failed(&self) -> &'static str;

    /// 整理対象が 1 件も無いことを伝える。
    ///
    /// 対象が無いのは正常な状態であるためエラーにはせず、この文言を標準エラーへ出す
    /// （requirements.md FR-21）。
    fn nothing_to_prune(&self) -> &'static str;
}

/// `gz sync`（[`crate::commands::sync`]）の文言。
///
/// 取り込み方式そのもの（`merge` / `rebase` / `--ff-only`）は git のサブコマンド名・
/// オプション名であるため翻訳せず、この trait にも含めない（design.md「翻訳しないもの」）。
pub trait SyncMessages: Sync + std::fmt::Debug {
    /// `--rebase` と `--merge` が同時に指定されたことを伝える。
    ///
    /// オプション名は翻訳しない（design.md「翻訳しないもの」）。
    fn conflicting_modes(&self) -> &'static str;

    /// upstream のリモートからの取得（`git fetch <remote>`）に失敗したことを伝える。
    fn fetch_failed(&self, remote: &str) -> String;

    /// upstream からリモート追跡参照を組み立てられない設定であることを伝える。
    ///
    /// `remote` / `merge_ref` には `branch.<name>.remote` / `branch.<name>.merge` の値が
    /// そのまま入る。案内に含まれる `git branch --set-upstream-to` はユーザーがそのまま
    /// 打ち込むコマンドであるため翻訳しない。
    fn tracking_ref_unavailable(&self, branch: &str, remote: &str, merge_ref: &str) -> String;

    /// upstream に設定されたリモートが登録済みでないことを伝える。
    ///
    /// `branch.<name>.remote` には URL を直接書けるため、登録済みのリモート名でない値は
    /// fetch せずに拒否する（design.md セキュリティ設計）。案内に含まれるコマンド列は
    /// 翻訳しないが、`<名前>` `<URL>` のようなプレースホルダは読み手のための語であり
    /// 表示言語に合わせる（[`FetchMessages::no_remotes`] と同じ扱い）。
    fn unknown_remote(&self, branch: &str, remote: &str) -> String;

    /// fetch してもリモート追跡参照が現れなかったことを伝える。
    fn missing_tracking_ref(&self, reference: &str, remote: &str, branch: &str) -> String;

    /// 取り込むコミットが 1 件も無い（最新である）ことを伝える。
    fn up_to_date(&self, branch: &str, reference: &str) -> String;

    /// まだ push していないコミットが残っていることを伝える注記。
    ///
    /// 本文と注記を区切る改行は**装飾**であり呼び出し側が付ける
    /// （[`FinderMessages::truncation_notice`] と同じ扱い）。
    fn unpushed_commits(&self, count: usize) -> String;

    /// 取り込み（`git merge` / `git rebase`）の実行に失敗したことを伝える。
    fn integration_failed(&self, reference: &str) -> String;

    /// 確認プロンプトの見出し（取り込み元の参照・コミット数・取り込み先のブランチ）。
    ///
    /// `--rebase` の場合に添える履歴改変の注記は
    /// [`CommonMessages::history_rewrite_note`] が返し、区切りの改行は**装飾**として
    /// 呼び出し側が付ける。
    fn confirmation(&self, reference: &str, count: usize, branch: &str) -> String;
}

/// `gz diff`（[`crate::commands::diff`]）の文言。
///
/// 比較範囲の呼称（`unstaged_range` 等）とヘッダーは**候補行ではなく fuzgit 自身の説明**で
/// あるため翻訳する（[`StatusMessages::add_action`] と同じ扱い）。ヘッダーで選ばせた結果の
/// 照合は候補の名前・ハッシュで行うため、翻訳の影響を受けない。範囲に含まれる `HEAD` /
/// `index` は git の語彙であり翻訳しない。
pub trait DiffMessages: Sync + std::fmt::Debug {
    /// 比較モードのフラグが 2 つ以上同時に指定されたことを伝える。
    ///
    /// オプション名は翻訳しない（design.md「翻訳しないもの」）。
    fn conflicting_modes(&self) -> &'static str;

    /// index と作業ツリーの比較（フラグ指定なし）の呼称。
    fn unstaged_range(&self) -> &'static str;

    /// HEAD と index の比較（`--staged`）の呼称。
    fn staged_range(&self) -> &'static str;

    /// HEAD と作業ツリーの比較（`--head`）の呼称。
    fn head_range(&self) -> &'static str;

    /// 比較元のブランチを選ぶ際のヘッダー。
    ///
    /// 2 段階のうちどちらを選んでいるのかを示す `1/2` / `2/2` は数値であり翻訳しない。
    fn base_branch_header(&self) -> &'static str;

    /// 比較先のブランチを選ぶ際のヘッダー。
    fn target_branch_header(&self) -> &'static str;

    /// 比較元のコミットを選ぶ際のヘッダー。
    fn base_commit_header(&self) -> &'static str;

    /// 比較先のコミットを選ぶ際のヘッダー。
    fn target_commit_header(&self) -> &'static str;

    /// upstream からリモート追跡参照を組み立てられない設定であることを伝える。
    ///
    /// 次に取れる操作が `gz sync` とは異なる（比較なので別の対象を選べばよい）ため、
    /// [`SyncMessages::tracking_ref_unavailable`] とは別の文言を持つ。
    fn tracking_ref_unavailable(&self, branch: &str, remote: &str, merge_ref: &str) -> String;

    /// 比較範囲の変更ファイル一覧を読み取れなかったことを伝える。
    ///
    /// どの比較で失敗したのかが分かるよう、[`CommonMessages::changed_files_read_failed`]
    /// とは別に比較範囲の呼称を添える。
    fn files_read_failed(&self, description: &str) -> String;

    /// 選択されたブランチが候補一覧に見つからなかったことを伝える。
    fn branch_selection_not_found(&self, selected: &str) -> String;

    /// 選択されたコミットが候補一覧に見つからなかったことを伝える。
    fn commit_selection_not_found(&self, selected: &str) -> String;

    /// 比較範囲に差分が無いことを伝える。
    ///
    /// 差分が無いのは正常な状態であるためエラーにはしない（requirements.md FR-17）。
    fn no_diff(&self, description: &str) -> String;

    /// `git diff` の実行に失敗したことを伝える。
    fn diff_failed(&self, description: &str) -> String;
}

/// `gz fetch`（[`crate::commands::fetch`]）の文言。
///
/// 候補行のうちリモート名・兄弟リポジトリのディレクトリ名・ブランチ名は git とファイル
/// システムに由来する名前であるため翻訳せず、この trait にも含めない（design.md
/// 「翻訳しないもの」）。一方で固定候補の表示・finder のヘッダー・プレビューの見出しは
/// **fuzgit 自身が書いた説明**であるため翻訳する（[`DiffMessages`] と同じ扱い）。
/// 進捗表示 `[<n>/<全体>] <対象>` は数値・区切り記号・対象の名前だけで構成されるため
/// 文言を持たない。
pub trait FetchMessages: Sync + std::fmt::Debug {
    /// 「すべてのリモート」を表す固定候補の表示。
    ///
    /// 選択結果の解決には実在のリモート名と衝突しない固定キーを使うため、表示を翻訳しても
    /// 対象の取り違えは起こらない（[`StatusMessages::add_action`] と同じ扱い）。
    /// 対象の呼称としても用いる（[`FetchMessages::fetch_failed`] の引数）。
    fn all_remotes_label(&self) -> &'static str;

    /// リモート 1 つを対象とする場合の呼称。
    ///
    /// [`FetchMessages::fixed_target`] / [`FetchMessages::fetch_failed`] へ埋め込まれる
    /// 名詞句であり、それ自体は文にならない。
    fn remote_description(&self, remote: &str) -> String;

    /// fetch 元のリモートが 1 つも登録されていないことを伝える。
    ///
    /// 案内に含まれるコマンド列は翻訳しないが、`<名前>` `<URL>` のようなプレースホルダは
    /// 読み手のための語であり表示言語に合わせる（[`FetchMessages::no_remotes`] と同じ扱い）。
    fn no_remotes(&self) -> &'static str;

    /// finder を省略して対象を確定したことと、省略した理由を伝える 1 行。
    ///
    /// `target` には [`FetchMessages::remote_description`] が返す呼称が入る。
    fn fixed_target(&self, target: &str) -> String;

    /// `git fetch` の実行に失敗したことを伝える。
    ///
    /// `target` には [`FetchMessages::remote_description`] または
    /// [`FetchMessages::all_remotes_label`] が返す呼称が入る。
    fn fetch_failed(&self, target: &str) -> String;

    /// 選択されたリモートが候補一覧に見つからなかったことを伝える。
    fn selection_not_found(&self, selected: &str) -> String;

    /// プレビューでリモートの URL を示すセクションの見出し。
    fn url_section(&self) -> &'static str;

    /// プレビューで既知のリモート追跡ブランチを示すセクションの見出し。
    fn tracking_section(&self) -> &'static str;

    /// 兄弟リポジトリ（`--siblings`）の探索に失敗したことを伝える。
    fn sibling_scan_failed(&self) -> &'static str;

    /// fetch できる兄弟リポジトリが 1 件も無いことを伝える。
    fn no_sibling_candidates(&self) -> &'static str;

    /// 兄弟リポジトリの選択で finder を省略したことを伝える 1 行。
    fn single_sibling_reason(&self) -> &'static str;

    /// 兄弟リポジトリ一覧の上部に固定表示する操作説明。
    ///
    /// キー名（`Tab` / `Enter`）は skim のキー表記であり翻訳しない。
    fn siblings_header(&self) -> &'static str;

    /// 候補から除外した兄弟リポジトリの件数をヘッダーへ添える節。
    ///
    /// 節の区切り（`  |  `）は**装飾**であり呼び出し側が付ける。
    fn excluded_count(&self, count: usize) -> String;

    /// `--prune` の適用範囲をヘッダーへ添える節。
    ///
    /// オプション名は翻訳しない（design.md「翻訳しないもの」）。
    fn prune_scope_note(&self) -> &'static str;

    /// 兄弟リポジトリのプレビューでブランチの追跡状況を示すセクションの見出し。
    fn tracking_state_section(&self) -> &'static str;

    /// 兄弟リポジトリのワークツリーのパスを文字列として扱えないことを伝える。
    ///
    /// 対象が worktree ではなくリポジトリのワークツリーであるため、
    /// [`WorktreeMessages::path_not_utf8`] とは別の文言を持つ。
    fn path_not_utf8(&self, path: &Path) -> String;

    /// 選択された兄弟リポジトリが候補一覧に見つからなかったことを伝える。
    fn sibling_selection_not_found(&self, paths: &str) -> String;

    /// 兄弟リポジトリでの `git fetch` を開始できなかったことを伝える。
    ///
    /// リポジトリごとの取得の失敗（到達不能・認証拒否）は集計へ回すため、これは
    /// git を起動できなかった場合だけに使う。
    fn sibling_start_failed(&self, name: &str) -> String;

    /// 並列で取得できなかった対象を直列で実行し直すことと、その件数・理由を伝える 1 行（FR-28）。
    ///
    /// 並列フェーズは対話（認証情報の入力）を構造的に禁じているため、passphrase や資格情報を
    /// 求められた対象はそこで失敗する。直列フェーズは**リトライではなく**、それらを
    /// 「対話できる形」で 1 回だけ実行し直すものであり、黙って実行し直すと同じ対象の出力が
    /// 二度出る理由が読み手に分からない。
    ///
    /// この文言を [`CommonMessages`] へ置かないのは、読み手が知る必要のある事情
    /// （並列フェーズがあること）が `gz fetch --siblings` に固有であり、他のコマンドと
    /// 共有できないためである。
    fn serial_fallback(&self, count: usize) -> String;

    /// 1 件でも取得に失敗したことを伝える。
    ///
    /// 失敗の内訳は [`CommonMessages::failed_targets`] が直前に示すため再掲しない。
    fn partial_failure(&self) -> &'static str;
}

/// `gz pull`（[`crate::commands::pull`]）の文言。
///
/// 候補行（`  <ブランチ>  →  <upstream>`）はブランチ名と追跡参照の短縮名だけで
/// 構成されるため翻訳せず、この trait にも含めない（design.md「翻訳しないもの」）。
/// 進捗表示 `[<n>/<全体>] <ブランチ>` も同じ理由で文言を持たない。
/// 集計（成功件数 / 失敗件数と失敗した対象の列挙）は `gz fetch --siblings` と同じ形式で
/// 出すため [`CommonMessages::run_summary`] / [`CommonMessages::failed_targets`] を用いる。
pub trait PullMessages: Sync + std::fmt::Debug {
    /// 取り込み対象の候補を生成できなかったことを伝える。
    fn targets_read_failed(&self) -> &'static str;

    /// upstream へ追随させられるブランチが 1 件も無いことと、upstream の設定方法を伝える。
    ///
    /// 案内に含まれる `git push -u` / `git branch --set-upstream-to` はユーザーがそのまま
    /// 打ち込むコマンド列であるため翻訳しない（design.md「翻訳しないもの」）。
    /// 次に取れる操作が「対象を選び直す」ではなく「upstream を設定する」である点は
    /// [`CommonMessages::upstream_not_configured`] と同じだが、こちらは対象が複数ある
    /// 一括操作の案内であるため別の文言を持つ。
    fn no_candidates(&self) -> &'static str;

    /// 候補一覧の上部に固定表示する操作説明。
    ///
    /// キー名（`Tab` / `Enter`）は skim のキー表記であり翻訳しない。取り込み方式
    /// （fast-forward）は git の語彙であるため同様に翻訳しない。
    fn header(&self) -> &'static str;

    /// 候補から除外したブランチの件数をヘッダーへ添える節。
    ///
    /// 節の区切り（`  |  `）は**装飾**であり呼び出し側が付ける。除外の理由は
    /// `gz fetch --siblings`（[`FetchMessages::excluded_count`]）とは異なるため、
    /// 共有せずここに持つ。
    fn excluded_count(&self, count: usize) -> String;

    /// finder を省略した理由（候補が 1 件しか無いこと）。
    ///
    /// [`PullMessages::fixed_target`] の括弧の中へ入る節として各言語が組み立てる。
    fn single_target_reason(&self) -> &'static str;

    /// finder を省略して対象を確定したことと、省略した理由を伝える 1 行。
    ///
    /// `branch` には取り込むブランチ名が入る。
    fn fixed_target(&self, branch: &str) -> String;

    /// プレビューで未取り込みのコミットを示すセクションの見出し。
    ///
    /// 比較の相手はローカルに保存済みの追跡参照であるため、いつの情報なのかを見出しに含める。
    fn unmerged_section(&self) -> &'static str;

    /// 選択されたブランチが候補一覧に見つからなかったことを伝える。
    ///
    /// `branches` には見つからなかったブランチ名を `, ` で連結したものが入る。
    fn selection_not_found(&self, branches: &str) -> String;

    /// upstream のリモートを取得できなかったために取り込みを飛ばしたことを伝える 1 行。
    fn skipped(&self, remote: &str, branch: &str) -> String;

    /// リモートからの取得を開始できなかったことを伝える。
    ///
    /// 到達不能・認証拒否は集計へ回すため、これは git を起動できなかった場合だけに使う。
    fn fetch_start_failed(&self, remote: &str) -> String;

    /// ブランチの取り込みを開始できなかったことを伝える。
    ///
    /// fast-forward できないといった取り込みそのものの失敗は集計へ回すため、これは
    /// git を起動できなかった場合だけに使う。
    fn integration_start_failed(&self, branch: &str) -> String;

    /// fast-forward できなかったブランチがある場合にだけ添える案内。
    ///
    /// 案内に含まれる `gz sync --rebase` / `gz sync --merge` はユーザーがそのまま
    /// 打ち込むコマンド列であるため翻訳しない。
    fn fast_forward_guidance(&self) -> &'static str;

    /// 1 件でも取り込みに失敗したことを伝える。
    ///
    /// 失敗の内訳は [`CommonMessages::failed_targets`] が直前に示すため再掲しない。
    fn partial_failure(&self) -> &'static str;
}

/// [`crate::error::Error`] をユーザー向けの 1 文へ整形する文言。
///
/// [`crate::error::Error`] の `Display` は `anyhow` の `#[source]` 連鎖と `Debug` 出力のための
/// **英語の開発者向け表示**であり、ユーザーへ見せる文言はこの trait が担う（FR-27）。
///
/// 実装は `Error` に対する**網羅的な `match`** とし、ワイルドカードの腕（`_ =>`）を置かない。
/// バリアントを追加したときに ja / en の双方がコンパイルエラーになることが、この分離の目的
/// そのものであるため（design.md「trait 方式の利点をエラー型にも効かせる」）。
pub trait ErrorMessages: Sync + std::fmt::Debug {
    /// `main` がエラー連鎖の先頭へ付ける表示ラベル（`エラー: ` / `error: `）。
    ///
    /// 連鎖の各要素と同じ言語で組み立てないと、ラベルだけが別言語で残ってしまうため
    /// 文言として持つ。
    fn prefix(&self) -> &'static str;

    /// エラーの原因と、次にとるべき操作をユーザーの言語で説明する。
    fn describe(&self, error: &crate::error::Error) -> String;
}

/// 選択 UI（[`crate::finder`]）が**自ら**出す文言。
///
/// 候補の表示文字列（`FinderItem` の `display`）、[`crate::finder::FinderOptions::header`]、
/// `PreviewSource::Composite` のセクション見出しは、いずれも呼び出し側（コマンド）が
/// 組み立てるためここには含まない。見出しの装飾（`── … ──`）は文言ではないため同様に
/// 含まない（design.md「finder（i18n 導入により更新）」）。
pub trait FinderMessages: Sync + std::fmt::Debug {
    /// ファイル内容のプレビューが上限で打ち切られたことを示す注記。
    ///
    /// 本文と注記を区切る改行は**装飾**であり呼び出し側が付ける。ここが返すのは注記の
    /// 文言のみ。
    fn truncation_notice(&self) -> &'static str;

    /// ファイルを読み取れずプレビュー本文を作れなかったことを伝える。
    ///
    /// プレビューの失敗で選択操作全体を止めることはせず、この文言をプレビュー領域へ出す。
    fn file_read_failed(&self, path: &Path, error: &std::io::Error) -> String;
}

/// 破壊的操作の確認プロンプト（[`crate::commands::confirmation`]）の文言。
///
/// 「何が失われるのか」を示す `header` と対象一覧は呼び出し側のコマンドが組み立てるため
/// ここには含まない。また、承認とみなす応答（`y` / `yes`）は**言語に依らず固定**であり
/// 文言としては持たない（`はい` を受理すると、英語環境の利用者が意図せず承認できてしまう
/// 入力が増えるため。design.md「commands（i18n 導入により更新）」）。
pub trait ConfirmMessages: Sync + std::fmt::Debug {
    /// 実行の同意を求めるプロンプト。
    ///
    /// 同じ行で入力を待つため**末尾に改行を含めない**。既定が否認であることを
    /// `[y/N]` の大文字小文字で示す。
    fn prompt(&self) -> &'static str;

    /// 承認が得られず操作を行わなかったことを伝える。
    fn cancelled(&self) -> &'static str;

    /// 確認メッセージを標準エラーへ書き出せなかったことを伝える。
    fn output_failed(&self) -> &'static str;

    /// 確認応答を標準入力から読み取れなかったことを伝える。
    fn input_failed(&self) -> &'static str;
}
