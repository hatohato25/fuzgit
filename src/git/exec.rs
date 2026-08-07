//! システムの `git` コマンド実行ラッパー。
//!
//! すべての呼び出しは `Command::new("git").args(...)` の引数配列渡しで行い、
//! シェル（`sh -c` 等）を一切経由しない。これによりブランチ名・パス・検索クエリ等の
//! ユーザー由来データによるシェルインジェクションを構造的に排除する。

use std::io::ErrorKind;
use std::path::Path;
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
    capture(None, args)
}

/// [`capture_git`] と同じだが、`directory` をカレントディレクトリとして実行する。
///
/// 候補一覧の読み取り（`git status` / `git ls-tree`）を、プロセスのカレントディレクトリではなく
/// 開いたリポジトリに対して確実に行うために用いる。
///
/// # Errors
///
/// [`capture_git`] と同じ。
pub fn capture_git_in(directory: &Path, args: &[&str]) -> Result<Vec<u8>> {
    capture(Some(directory), args)
}

/// `git` を実行して標準出力をキャプチャする。`directory` 指定時はそこを作業ディレクトリとする。
fn capture(directory: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).stdin(Stdio::null());
    if let Some(directory) = directory {
        command.current_dir(directory);
    }

    let output = command
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

/// 作業ツリールート基準の相対パスを、git へ渡すパススペックへ変換する。
///
/// `git status --porcelain` や `git ls-tree --full-tree` が返すパスはリポジトリルート基準だが、
/// git のパス引数はカレントディレクトリ基準で解釈されるため、サブディレクトリから実行すると
/// 対象がずれる。また git のパススペックは既定でワイルドカードとして解釈されるため、
/// `a[1].txt` のような名前のファイルが取りこぼされる。
/// `:(top,literal)` を付けて「ルート基準」「ワイルドカード解釈なし」を明示することで
/// 両方を防ぐ（このパススペックは常に `--` の後ろに置く）。
#[must_use]
pub fn pathspec(path: &str) -> String {
    format!(":(top,literal){path}")
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

    #[test]
    fn capture_git_in_runs_in_the_given_directory() {
        use crate::test_support::{TempDir, commit, init_repository};

        let dir = TempDir::new("exec-capture-in");
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");

        let stdout = capture_git_in(dir.path(), &["rev-parse", "HEAD"])
            .expect("rev-parse should succeed in the temporary repository");

        let text = String::from_utf8(stdout).expect("rev-parse emits utf-8");
        assert_eq!(text.trim(), head);
    }

    #[test]
    fn a_pathspec_is_rooted_and_literal() {
        assert_eq!(pathspec("src/main.rs"), ":(top,literal)src/main.rs");
    }

    #[test]
    fn a_pathspec_keeps_wildcard_characters_verbatim() {
        assert_eq!(pathspec("dir/a[1].txt"), ":(top,literal)dir/a[1].txt");
        assert_eq!(pathspec("with space.txt"), ":(top,literal)with space.txt");
    }
}
