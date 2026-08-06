//! 共通要件（requirements.md「共通要件」）のうち、非対話パスの統合テスト。
//!
//! TUI（skim）が起動する対話パスは自動テスト対象外とし、手動確認とする。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use assert_cmd::Command;

/// 検証対象のバイナリ（パッケージ名 `fuzgit` に対する実行ファイル名は `gz`）。
const BIN_NAME: &str = "gz";

/// `gz` のすべてのサブコマンド名。ヘルプ出力の検証に用いる。
const SUBCOMMANDS: [&str; 8] = [
    "branch",
    "log",
    "cherry-pick",
    "restore",
    "add",
    "stash",
    "tag",
    "reflog",
];

/// `gz log --limit` の既定値（`fuzgit::cli::DEFAULT_LOG_LIMIT` と対応）。
const DEFAULT_LOG_LIMIT: &str = "1000";

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// テストごとに一意な一時ディレクトリ。Drop で再帰削除する。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fuzgit-it-{label}-{pid}-{unique}",
            pid = std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn gz() -> Command {
    Command::cargo_bin(BIN_NAME).expect("gz binary should be built")
}

/// コミットを 1 件も持たない git リポジトリを用意する。
///
/// 候補が 0 件になり TUI を起動しないため、統合テストから安全に実行できる。
fn empty_repository(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    let status = std::process::Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(dir.path())
        .status()
        .expect("failed to run git init");
    assert!(status.success(), "git init failed in {:?}", dir.path());
    dir
}

#[test]
fn help_lists_all_subcommands() {
    let output = gz()
        .arg("--help")
        .output()
        .expect("failed to run gz --help");

    assert!(output.status.success(), "--help should exit successfully");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for subcommand in SUBCOMMANDS {
        assert!(
            stdout.contains(subcommand),
            "`{subcommand}` should appear in --help output:\n{stdout}"
        );
    }
}

#[test]
fn bare_invocation_shows_help_and_exits_non_zero() {
    let output = gz().output().expect("failed to run gz without arguments");

    assert!(
        !output.status.success(),
        "bare `gz` should not exit successfully"
    );

    // clap の `arg_required_else_help` はヘルプを stderr へ出す
    let stderr = String::from_utf8(output.stderr).expect("help output should be utf-8");
    for subcommand in SUBCOMMANDS {
        assert!(
            stderr.contains(subcommand),
            "`{subcommand}` should appear in bare invocation output:\n{stderr}"
        );
    }
}

#[test]
fn running_outside_a_repository_fails_with_a_dedicated_message() {
    let dir = TempDir::new("outside");

    let output = gz()
        .arg("branch")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz branch");

    assert!(
        !output.status.success(),
        "running outside a repository should exit non-zero"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("git リポジトリではありません"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn every_subcommand_has_its_own_help() {
    for subcommand in SUBCOMMANDS {
        let output = gz()
            .args([subcommand, "--help"])
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {subcommand} --help: {err}"));

        assert!(
            output.status.success(),
            "gz {subcommand} --help should exit successfully"
        );

        let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
        assert!(
            stdout.contains(&format!("gz {subcommand}")),
            "usage line missing for {subcommand}:\n{stdout}"
        );
    }
}

#[test]
fn log_help_documents_the_default_limit() {
    let output = gz()
        .args(["log", "--help"])
        .output()
        .expect("failed to run gz log --help");

    assert!(output.status.success(), "gz log --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    assert!(
        stdout.contains(&format!("[default: {DEFAULT_LOG_LIMIT}]")),
        "default limit missing from help:\n{stdout}"
    );
}

#[test]
fn log_rejects_a_non_numeric_limit() {
    let output = gz()
        .args(["log", "--limit", "many"])
        .output()
        .expect("failed to run gz log --limit many");

    assert!(
        !output.status.success(),
        "a non-numeric limit should be rejected"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("--limit"),
        "the rejected option should be named:\n{stderr}"
    );
}

/// `--limit` の指定有無に関わらず、候補が 0 件なら TUI を起動せずに終了することを確認する。
///
/// 併せて、指定した値がパースを通過して読み取り処理まで到達していることも確かめる。
#[test]
fn log_accepts_the_limit_and_reports_when_there_are_no_commits() {
    let dir = empty_repository("log-limit");

    for arguments in [
        vec!["log"],
        vec!["log", "--limit", "5"],
        vec!["log", "-n", "5"],
        vec!["log", "--limit", DEFAULT_LOG_LIMIT],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when there are no commits"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("選択できる候補がありません"),
            "unexpected stderr for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// `gz branch` も候補が 0 件のときは TUI を起動しないことを確認する。
#[test]
fn branch_reports_when_there_are_no_branches() {
    let dir = empty_repository("branch-empty");

    let output = gz()
        .arg("branch")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz branch");

    assert!(
        !output.status.success(),
        "gz branch should exit non-zero when there are no branches"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("選択できる候補がありません"),
        "unexpected stderr:\n{stderr}"
    );
}
