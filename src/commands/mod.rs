//! 各サブコマンドのユースケース実装。
//!
//! 各コマンドは「`git::read` で候補取得 → `finder` で選択 → `git::exec` で実行」の
//! 直列オーケストレーションのみを担う。

use anyhow::{Result, bail};

use crate::cli::Command;
use crate::commands::restore::RestoreTarget;
use crate::git::read::{BranchScope, CommitScope};
use crate::git::repo;

pub mod add;
pub mod branch;
pub mod cherry_pick;
pub mod file_selection;
pub mod log;
pub mod restore;

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
        Command::Stash { .. } => bail!("`gz stash` は未実装です"),
        Command::Tag { .. } => bail!("`gz tag` は未実装です"),
        Command::Reflog { .. } => bail!("`gz reflog` は未実装です"),
    }
}
