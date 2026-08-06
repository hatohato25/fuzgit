//! 各サブコマンドのユースケース実装。
//!
//! 各コマンドは「`git::read` で候補取得 → `finder` で選択 → `git::exec` で実行」の
//! 直列オーケストレーションのみを担う。

use anyhow::{Result, bail};

use crate::cli::Command;
use crate::git::read::BranchScope;
use crate::git::repo;

pub mod branch;
pub mod log;

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
        Command::CherryPick { .. } => bail!("`gz cherry-pick` は未実装です"),
        Command::Restore { .. } => bail!("`gz restore` は未実装です"),
        Command::Add => bail!("`gz add` は未実装です"),
        Command::Stash { .. } => bail!("`gz stash` は未実装です"),
        Command::Tag { .. } => bail!("`gz tag` は未実装です"),
        Command::Reflog { .. } => bail!("`gz reflog` は未実装です"),
    }
}
