//! `gz` コマンドのエントリポイント。
//!
//! CLI をパースして [`fuzgit::commands::dispatch`] へ振り分け、
//! 結果に応じた終了コードを返すだけの薄い層に留める。

use std::process::ExitCode;

use clap::Parser;
use fuzgit::cli::Cli;
use fuzgit::commands;
use fuzgit::error::is_cancelled;

/// fuzzy finder 中断時の終了コード（128 + SIGINT(2)）。
const EXIT_CANCELLED: u8 = 130;

/// エラー終了時の終了コード。
const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    let cli = Cli::parse();

    match commands::dispatch(&cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 中断はユーザーの意図した操作であり異常ではないため、
            // メッセージを出さずに専用の終了コードで抜ける
            if is_cancelled(&error) {
                return ExitCode::from(EXIT_CANCELLED);
            }

            eprintln!("エラー: {error:#}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}
