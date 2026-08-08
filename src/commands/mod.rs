//! 各サブコマンドのユースケース実装。
//!
//! 各コマンドは「`git::read` で候補取得 → `finder` で選択 → `git::exec` で実行」の
//! 直列オーケストレーションのみを担う。

use anyhow::{Result, bail};

use crate::cli::{Command, StashCommand};
use crate::commands::push::UpstreamUpdate;
use crate::commands::restore::RestoreTarget;
use crate::commands::stash::{StashAction, UntrackedFiles};
use crate::commands::tag::TagAction;
use crate::git::read::{BranchScope, CommitScope};
use crate::git::repo;

pub mod add;
pub mod branch;
pub mod cherry_pick;
pub mod commit;
pub mod confirmation;
pub mod file_selection;
pub mod log;
pub mod push;
pub mod reflog;
pub mod restore;
pub mod stash;
pub mod tag;

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
        Command::Branch { all } => {
            let scope = if *all {
                BranchScope::All
            } else {
                BranchScope::Local
            };
            branch::run(&repository, scope)
        }
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
        // FR-11〜FR-13 の 3 コマンドは後続フェーズで実装へ差し替える
        Command::Fixup { .. } => unimplemented_command("gz fixup"),
        Command::Merge { .. } => unimplemented_command("gz merge"),
        Command::Rebase => unimplemented_command("gz rebase"),
    }
}

/// 未実装のサブコマンドであることを伝えて失敗する。
///
/// 何もせずに成功終了すると「実行したのに何も起きない」ことになるため、
/// 明示的に非ゼロ終了させる。
fn unimplemented_command(name: &str) -> Result<()> {
    bail!("`{name}` は未実装です");
}
