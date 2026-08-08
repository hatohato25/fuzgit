//! システムの `git` コマンド実行ラッパー。
//!
//! すべての呼び出しは `Command::new("git").args(...)` の引数配列渡しで行い、
//! シェル（`sh -c` 等）を一切経由しない。これによりブランチ名・パス・検索クエリ等の
//! ユーザー由来データによるシェルインジェクションを構造的に排除する。
//!
//! # デバッグログ
//!
//! 環境変数 `FUZGIT_DEBUG=1` を指定すると、このモジュールが実行する git コマンドを
//! 標準エラーへ出力する。ロギングクレートは導入せず、ここだけの軽量な実装で済ませる。

use std::io::{ErrorKind, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// デバッグログを有効にする環境変数名。
const DEBUG_ENV: &str = "FUZGIT_DEBUG";

/// デバッグログを有効にする値。この値と完全に一致する場合のみ出力する。
const DEBUG_ENABLED_VALUE: &str = "1";

/// デバッグログの各行に付ける接頭辞。git 自身の出力と区別するために付ける。
const DEBUG_PREFIX: &str = "[fuzgit]";

/// エラーメッセージ表示用に引数列を連結する。
///
/// あくまで表示専用であり、この文字列をコマンドとして実行することはない。
fn display_args(args: &[&str]) -> String {
    args.join(" ")
}

/// 環境変数の値からデバッグログの有効・無効を判定する。
///
/// 有効とみなすのは値が厳密に `1` の場合だけで、未設定（`None`）・空文字・`0`・`true` などは
/// すべて無効とする。「設定されていれば何でも有効」にすると `FUZGIT_DEBUG=0` を無効化のつもりで
/// 指定したときに挙動が食い違うため、判定基準を 1 つに固定する。
///
/// 値を引数で受け取るのは、`std::env::set_var` に依存せず単体テストできるようにするため
/// （テストは並列実行されるため、プロセス全体の環境変数を書き換えると他のテストと干渉する）。
fn is_debug_enabled(value: Option<&str>) -> bool {
    value == Some(DEBUG_ENABLED_VALUE)
}

/// 現在のプロセスの環境変数からデバッグログの有効・無効を判定する。
///
/// 値が UTF-8 でない場合は [`DEBUG_ENABLED_VALUE`] と一致し得ないため無効とする。
fn debug_enabled() -> bool {
    is_debug_enabled(std::env::var(DEBUG_ENV).ok().as_deref())
}

/// デバッグログに出力する 1 行を組み立てる。
///
/// 実際に実行する引数配列をそのまま並べる（表示専用であり、この文字列を実行することはない）。
/// 出力されるのは git のサブコマンド・リビジョン・パスだけであり、認証情報の類は含まれない。
fn debug_line(directory: Option<&Path>, args: &[&str]) -> String {
    match directory {
        Some(directory) => format!(
            "{DEBUG_PREFIX} (cwd: {directory}) git {args}",
            directory = directory.display(),
            args = display_args(args)
        ),
        None => format!("{DEBUG_PREFIX} git {args}", args = display_args(args)),
    }
}

/// これから実行する git コマンドをデバッグログとして標準エラーへ出力する。
///
/// 標準出力はハッシュ・タグ名のパイプ用途に使うため、ログは必ず標準エラーへ出す。
fn log_command(directory: Option<&Path>, args: &[&str]) {
    if !debug_enabled() {
        return;
    }

    // ログの書き込み失敗（標準エラーの閉鎖など）で git の実行そのものを止めたくないため、
    // ここだけは結果を破棄する。デバッグ出力は本来の処理に影響を与えない
    let _ = writeln!(std::io::stderr(), "{}", debug_line(directory, args));
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
    log_command(None, args);

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
    log_command(directory, args);

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

    #[test]
    fn debug_logging_is_enabled_only_by_the_documented_value() {
        assert!(is_debug_enabled(Some("1")));
    }

    #[test]
    fn debug_logging_is_disabled_when_the_variable_is_unset() {
        assert!(!is_debug_enabled(None));
    }

    #[test]
    fn debug_logging_is_disabled_for_an_empty_value() {
        // 空文字は「設定されているが有効化はしていない」状態として無効に倒す
        assert!(!is_debug_enabled(Some("")));
    }

    #[test]
    fn debug_logging_is_disabled_for_any_other_value() {
        for value in ["0", "true", "yes", "on", "2", " 1", "1\n", "01"] {
            assert!(
                !is_debug_enabled(Some(value)),
                "{value:?} must not enable debug logging"
            );
        }
    }

    #[test]
    fn a_debug_line_shows_the_command_that_is_about_to_run() {
        assert_eq!(
            debug_line(None, &["switch", "feature"]),
            "[fuzgit] git switch feature"
        );
    }

    #[test]
    fn a_debug_line_names_the_directory_when_the_command_runs_elsewhere() {
        assert_eq!(
            debug_line(
                Some(Path::new("/tmp/repo")),
                &["status", "--porcelain", "-z"]
            ),
            "[fuzgit] (cwd: /tmp/repo) git status --porcelain -z"
        );
    }

    #[test]
    fn a_debug_line_keeps_the_arguments_verbatim() {
        // パススペックの取りこぼしを追えるよう、引数は加工せずそのまま並べる
        assert_eq!(
            debug_line(None, &["add", "--", ":(top,literal)dir/a[1].txt"]),
            "[fuzgit] git add -- :(top,literal)dir/a[1].txt"
        );
    }
}
