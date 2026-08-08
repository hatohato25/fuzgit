//! `gz branch create` / `delete` / `cleanup` — ブランチの作成・削除・整理（FR-20）。
//!
//! ブランチの切替（FR-1）は [`crate::commands::branch`] が担う。切替は引数なしの
//! `gz branch` として従来どおり維持し、このモジュールはサブコマンドで指定された
//! 管理操作だけを扱う。

use anyhow::Result;

use crate::cli::BranchCommand;
use crate::commands::unimplemented_command;

/// ブランチ管理のサブコマンドを対応する処理へ振り分ける。
///
/// # Errors
///
/// 各操作が失敗した場合にエラーを返す。
pub fn run(command: &BranchCommand) -> Result<()> {
    match command {
        BranchCommand::Create { .. } => unimplemented_command("gz branch create"),
        BranchCommand::Delete { .. } => unimplemented_command("gz branch delete"),
        BranchCommand::Cleanup { .. } => unimplemented_command("gz branch cleanup"),
    }
}
