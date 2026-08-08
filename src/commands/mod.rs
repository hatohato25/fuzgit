//! 各サブコマンドのユースケース実装。
//!
//! 各コマンドは「`git::read` で候補取得 → `finder` で選択 → `git::exec` で実行」の
//! 直列オーケストレーションのみを担う。

use anyhow::{Result, bail};

use crate::cli::{Command, StashCommand, WorktreeCommand};
use crate::commands::fixup::FixupKind;
use crate::commands::merge::MergeMode;
use crate::commands::push::UpstreamUpdate;
use crate::commands::restore::RestoreTarget;
use crate::commands::revert::MessageEditing;
use crate::commands::stash::{StashAction, UntrackedFiles};
use crate::commands::tag::TagAction;
use crate::git::read::{BranchScope, CommitScope};
use crate::git::repo;

pub mod add;
pub mod branch;
pub mod branch_manage;
pub mod cherry_pick;
pub mod commit;
pub mod confirmation;
pub mod file_selection;
pub mod fixup;
pub mod in_progress;
pub mod log;
pub mod merge;
pub mod push;
pub mod rebase;
pub mod reflog;
pub mod restore;
pub mod revert;
pub mod stash;
pub mod status;
pub mod tag;

/// これから実行する git コマンドを表示用の 1 行に整形する。
///
/// 確認プロンプトや復帰メニューで「何が実行されるのか」を示すために用いる。
/// 表示専用であり、この文字列をコマンドとして実行することはない（実行は常に引数配列渡し）。
/// 実行する引数配列そのものから組み立てるため、説明と実際の操作が食い違わない。
pub(crate) fn command_display(args: &[&str]) -> String {
    format!("git {args}", args = args.join(" "))
}

/// 現在の状態を色付きで示す `git status` の引数を組み立てる。
///
/// 固定項目のメニュー（FR-14 の復帰メニュー、FR-16 のアクションメニュー）で、
/// どの項目を選んでいても現在の状態（未解決 / 解決済み・staged / unstaged の区別を含む
/// 短縮表記）が見えるようにするために用いる。
/// 出力をキャプチャして実行する場合 git は色付けを止めるため、明示的に有効化する。
pub(crate) fn status_preview_args() -> Vec<String> {
    ["-c", "color.status=always", "status", "--short", "--branch"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// まだ実装されていないサブコマンドであることを伝えて失敗する。
///
/// `cli` の定義（ヘルプ・オプションの検証）を先に固めるための一時的な実装であり、
/// 各機能のフェーズで本実装へ差し替える。何もせずに正常終了すると「実行された」と
/// 誤解されるため、必ずエラーとして終了する。
pub(crate) fn unimplemented_command(name: &str) -> Result<()> {
    bail!("`{name}` はまだ実装されていません");
}

/// サブコマンドを対応する実装へ振り分ける。
///
/// # Errors
///
/// リポジトリのオープンに失敗した場合、および各サブコマンドの処理が失敗した場合にエラーを返す。
pub fn dispatch(command: &Command) -> Result<()> {
    // すべてのサブコマンドが git リポジトリ内での実行を前提とするため、
    // 個別処理へ入る前にここで一度だけ検証してエラーメッセージを統一する
    let repository = repo::discover_from_current_dir()?;

    match command {
        // サブコマンドなしは従来どおりの切替（FR-1）。管理操作は branch_manage が担う
        Command::Branch { all, command } => match command {
            None => {
                let scope = if *all {
                    BranchScope::All
                } else {
                    BranchScope::Local
                };
                branch::run(&repository, scope)
            }
            Some(command) => branch_manage::run(command),
        },
        Command::Log { limit } => log::run(&repository, *limit),
        Command::CherryPick { branch } => {
            let scope = match branch {
                Some(name) => CommitScope::Branch(name),
                None => CommitScope::AllBranches,
            };
            cherry_pick::run(&repository, scope)
        }
        Command::Restore { source, staged } => {
            let target = if *staged {
                RestoreTarget::Index
            } else {
                RestoreTarget::Worktree
            };
            restore::run(&repository, target, source.as_deref())
        }
        Command::Add => add::run(&repository),
        Command::Stash { command } => match command {
            StashCommand::Push {
                message,
                include_untracked,
            } => {
                let untracked = if *include_untracked {
                    UntrackedFiles::Include
                } else {
                    UntrackedFiles::Exclude
                };
                stash::push(&repository, message.as_deref(), untracked)
            }
            StashCommand::Apply => stash::run(&repository, StashAction::Apply),
            StashCommand::Pop => stash::run(&repository, StashAction::Pop),
            StashCommand::Drop => stash::run(&repository, StashAction::Drop),
        },
        Command::Tag { switch, diff } => {
            tag::run(&repository, TagAction::from_flags(*switch, *diff)?)
        }
        Command::Reflog { restore } => reflog::run(&repository, restore.as_deref()),
        Command::Commit { message } => commit::run(&repository, message.as_deref()),
        Command::Push { set_upstream } => {
            let upstream = if *set_upstream {
                UpstreamUpdate::Set
            } else {
                UpstreamUpdate::Keep
            };
            push::run(&repository, upstream)
        }
        Command::Fixup { squash } => {
            let kind = if *squash {
                FixupKind::Squash
            } else {
                FixupKind::Fixup
            };
            fixup::run(&repository, kind)
        }
        Command::Merge {
            no_ff,
            squash,
            ff_only,
        } => merge::run(
            &repository,
            MergeMode::from_flags(*no_ff, *squash, *ff_only)?,
        ),
        Command::Rebase => rebase::run(&repository),
        Command::Revert { no_edit } => {
            let editing = if *no_edit {
                MessageEditing::Skip
            } else {
                MessageEditing::Interactive
            };
            revert::run(&repository, editing)
        }
        Command::Status => status::run(&repository),
        Command::Diff { .. } => unimplemented_command("gz diff"),
        Command::Fetch { .. } => unimplemented_command("gz fetch"),
        Command::Sync { .. } => unimplemented_command("gz sync"),
        Command::Worktree { command } => match command {
            None => unimplemented_command("gz worktree"),
            Some(WorktreeCommand::Add { .. }) => unimplemented_command("gz worktree add"),
            Some(WorktreeCommand::Remove) => unimplemented_command("gz worktree remove"),
            Some(WorktreeCommand::Prune) => unimplemented_command("gz worktree prune"),
        },
    }
}
