//! システムの `git` コマンド実行ラッパー。
//!
//! すべての呼び出しは `Command::new("git").args(...)` の引数配列渡しで行い、
//! シェル（`sh -c` 等）を一切経由しない。これによりブランチ名・パス・検索クエリ等の
//! ユーザー由来データによるシェルインジェクションを構造的に排除する。

use std::io::ErrorKind;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// エラーメッセージ表示用に引数列を連結する。
///
/// あくまで表示専用であり、この文字列をコマンドとして実行することはない。
fn display_args(args: &[&str]) -> String {
    args.join(" ")
}

/// `git` の起動失敗を、原因に応じたドメインエラーへ変換する。
fn map_spawn_error(source: std::io::Error, args: &[&str]) -> Error {
    if source.kind() == ErrorKind::NotFound {
        Error::GitNotFound
    } else {
        Error::GitSpawnFailed {
            args: display_args(args),
            source,
        }
    }
}

/// `git` を標準入出力を継承したまま実行する。
///
/// `switch` / `cherry-pick` 等の書き込み系操作に用いる。git 自身の出力（進捗・
/// コンフリクト内容など）をそのままユーザーへ見せたいため、出力はキャプチャしない。
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - 非ゼロ終了した場合は [`Error::GitCommandFailed`]（stderr は端末へ直接出力済みのため空）
pub fn run_git(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|source| map_spawn_error(source, args))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::GitCommandFailed {
            args: display_args(args),
            stderr: String::new(),
        })
    }
}

/// `git` を実行し、標準出力をバイト列としてキャプチャする。
///
/// プレビュー生成（`git show --color=always` 等）や `git status --porcelain` の
/// パースに用いる。標準入力は閉じ、対話プロンプトで固まらないようにする。
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - 非ゼロ終了した場合は stderr を含む [`Error::GitCommandFailed`]
pub fn capture_git(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| map_spawn_error(source, args))?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::GitCommandFailed {
            args: display_args(args),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_git_returns_stdout_on_success() {
        let stdout = capture_git(&["--version"]).expect("git --version should succeed");
        let text = String::from_utf8(stdout).expect("git --version emits utf-8");
        assert!(
            text.starts_with("git version"),
            "unexpected output: {text:?}"
        );
    }

    #[test]
    fn capture_git_reports_stderr_on_failure() {
        let err =
            capture_git(&["fuzgit-no-such-subcommand"]).expect_err("unknown subcommand must fail");

        match err {
            Error::GitCommandFailed { args, stderr } => {
                assert_eq!(args, "fuzgit-no-such-subcommand");
                assert!(!stderr.trim().is_empty(), "stderr should be captured");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn display_args_joins_with_spaces() {
        assert_eq!(
            display_args(&["log", "--oneline", "-n", "5"]),
            "log --oneline -n 5"
        );
    }
}
