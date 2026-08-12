//! 共通要件（requirements.md「共通要件」）のうち、非対話パスの統合テスト。
//!
//! TUI（skim）が起動する対話パスは自動テスト対象外とし、手動確認とする。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use assert_cmd::Command;

/// 検証対象のバイナリ（パッケージ名 `fuzgit` に対する実行ファイル名は `gz`）。
const BIN_NAME: &str = "gz";

/// `gz` のすべてのサブコマンド名。ヘルプ出力の検証に用いる。
const SUBCOMMANDS: [&str; 20] = [
    "branch",
    "log",
    "cherry-pick",
    "restore",
    "add",
    "stash",
    "tag",
    "reflog",
    "commit",
    "push",
    "fixup",
    "merge",
    "rebase",
    "revert",
    "status",
    "diff",
    "fetch",
    "pull",
    "sync",
    "worktree",
];

/// `gz stash` のサブコマンド名（`fuzgit::cli::StashCommand` と対応）。
const STASH_SUBCOMMANDS: [&str; 4] = ["push", "apply", "pop", "drop"];

/// `gz branch` のサブコマンド名（`fuzgit::cli::BranchCommand` と対応）。
const BRANCH_SUBCOMMANDS: [&str; 3] = ["create", "delete", "cleanup"];

/// `gz worktree` のサブコマンド名（`fuzgit::cli::WorktreeCommand` と対応）。
const WORKTREE_SUBCOMMANDS: [&str; 3] = ["add", "remove", "prune"];

/// `gz log --limit` の既定値（`fuzgit::cli::DEFAULT_COMMIT_LIMIT` と対応）。
const DEFAULT_COMMIT_LIMIT: &str = "1000";

/// unborn HEAD のエラーメッセージが伝えるべき原因（`fuzgit::error::Error::UnbornHead` と対応）。
const UNBORN_HEAD_CAUSE: &str = "まだコミットがありません";

/// unborn HEAD のエラーメッセージが伝えるべき次の操作。
const UNBORN_HEAD_NEXT_STEP: &str = "git commit";

/// デバッグログ（`FUZGIT_DEBUG=1`）の行頭に付く接頭辞（`fuzgit::git::exec` と対応）。
const DEBUG_PREFIX: &str = "[fuzgit]";

/// TUI を起動しないはずの実行に設ける待ち時間の上限。
///
/// 候補があるリポジトリで実行するテストは、事前チェックが失われると skim が端末を掴んで
/// 応答を待ち続ける。テストスイート全体が止まらないよう、明らかに超過した時点で打ち切る。
const FINDER_GUARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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

/// 指定ディレクトリで `git` を実行する（失敗はテストの前提が崩れたことを意味するため panic させる）。
fn git_in(directory: &Path, arguments: &[&str]) {
    let status = std::process::Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .status()
        .unwrap_or_else(|err| panic!("failed to run git {arguments:?}: {err}"));
    assert!(
        status.success(),
        "git {arguments:?} failed in {directory:?}"
    );
}

/// 指定ディレクトリでファイルを 1 件変更してコミットする。
fn commit_in(directory: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(directory.join(file), contents).expect("failed to write a file");
    git_in(directory, &["add", "--", file]);
    git_in(
        directory,
        &[
            "-c",
            "user.name=fuzgit test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "--message",
            message,
        ],
    );
}

/// コミットを 1 件持ち、ステージしていない変更が 1 件ある git リポジトリを用意する。
fn repository_with_an_unstaged_change(label: &str) -> TempDir {
    let dir = empty_repository(label);
    commit_in(dir.path(), "a.txt", "first\n", "first commit");
    std::fs::write(dir.path().join("a.txt"), "modified\n").expect("failed to write a file");
    dir
}

/// `main` と、`main` へ取り込まれていない `wip` ブランチを持つ git リポジトリを用意する。
///
/// merged なブランチが 1 件も無い状態であり、`gz branch cleanup` の候補が空になる。
fn repository_with_an_unmerged_branch(label: &str) -> TempDir {
    let dir = empty_repository(label);
    commit_in(dir.path(), "a.txt", "first\n", "first commit");
    git_in(dir.path(), &["switch", "--quiet", "--create", "wip"]);
    commit_in(dir.path(), "b.txt", "wip\n", "work in progress");
    git_in(dir.path(), &["switch", "--quiet", "main"]);
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
        stdout.contains(&format!("[default: {DEFAULT_COMMIT_LIMIT}]")),
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
        vec!["log", "--limit", DEFAULT_COMMIT_LIMIT],
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
            stderr.contains(UNBORN_HEAD_CAUSE),
            "unexpected stderr for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// コミットがまだ 1 件も無いリポジトリでは、候補ゼロではなくその原因を伝えることを確認する。
#[test]
fn an_unborn_head_is_reported_with_its_cause_and_next_step() {
    let dir = empty_repository("unborn-head");

    for subcommand in ["branch", "log", "cherry-pick"] {
        let output = gz()
            .arg(subcommand)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {subcommand}: {err}"));

        assert!(
            !output.status.success(),
            "gz {subcommand} should exit non-zero when there are no commits"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains(UNBORN_HEAD_CAUSE),
            "the cause should be explained for gz {subcommand}:\n{stderr}"
        );
        assert!(
            stderr.contains(UNBORN_HEAD_NEXT_STEP),
            "the next step should be suggested for gz {subcommand}:\n{stderr}"
        );
        assert!(
            !stderr.contains("選択できる候補がありません"),
            "the generic message should not be used for gz {subcommand}:\n{stderr}"
        );
    }
}

/// サブコマンドを追加しても、引数なし・`--all` の `gz branch` が切替のままであることを確認する。
///
/// 切替は FR-1 の最優先機能であり、管理サブコマンド（create/delete/cleanup）の追加で
/// 経路が変わっていないことを、切替と同じエラー（unborn HEAD）に到達するかどうかで確かめる。
#[test]
fn branch_without_a_subcommand_still_switches_branches() {
    let dir = empty_repository("branch-backward-compatibility");

    for arguments in [
        vec!["branch"],
        vec!["branch", "--all"],
        vec!["branch", "-a"],
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
            stderr.contains(UNBORN_HEAD_CAUSE),
            "gz {arguments:?} should still take the switch path:\n{stderr}"
        );
    }
}

/// 切替用のフラグと管理サブコマンドの併用が clap の段階で拒否されることを確認する。
#[test]
fn branch_rejects_switch_flags_combined_with_a_subcommand() {
    let dir = empty_repository("branch-flag-with-subcommand");

    let output = gz()
        .args(["branch", "--all", "create", "feature"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz branch --all create");

    assert!(
        !output.status.success(),
        "`--all` and a subcommand must not be combined"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("subcommand"),
        "the conflict should be explained:\n{stderr}"
    );
}

/// `gz branch` / `gz worktree` の各サブコマンドが独自のヘルプを持つことを確認する。
#[test]
fn nested_subcommands_have_their_own_help() {
    for (command, subcommands) in [
        ("branch", BRANCH_SUBCOMMANDS),
        ("worktree", WORKTREE_SUBCOMMANDS),
    ] {
        let output = gz()
            .args([command, "--help"])
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {command} --help: {err}"));

        assert!(
            output.status.success(),
            "gz {command} --help should exit successfully"
        );

        let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
        for subcommand in subcommands {
            assert!(
                stdout.contains(subcommand),
                "`{subcommand}` should appear in gz {command} --help:\n{stdout}"
            );
        }

        for subcommand in subcommands {
            let output = gz()
                .args([command, subcommand, "--help"])
                .output()
                .unwrap_or_else(|err| {
                    panic!("failed to run gz {command} {subcommand} --help: {err}")
                });

            assert!(
                output.status.success(),
                "gz {command} {subcommand} --help should exit successfully"
            );

            let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
            assert!(
                stdout.contains(&format!("gz {command} {subcommand}")),
                "usage line missing for gz {command} {subcommand}:\n{stdout}"
            );
        }
    }
}

/// 変更が 1 件も無い場合、`gz restore` / `gz add` は TUI を起動せずに終了することを確認する。
#[test]
fn restore_and_add_report_when_there_is_nothing_to_select() {
    let dir = empty_repository("nothing-to-select");

    for arguments in [vec!["restore"], vec!["restore", "--staged"], vec!["add"]] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when there is nothing to select"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("選択できる候補がありません"),
            "unexpected stderr for gz {arguments:?}:\n{stderr}"
        );
    }
}

/// stash / タグ / reflog が 1 件も無い場合、TUI を起動せずに終了することを確認する。
#[test]
fn stash_tag_and_reflog_report_when_there_is_nothing_to_select() {
    let dir = empty_repository("nothing-to-select-p3");

    for arguments in [
        vec!["stash", "push"],
        vec!["stash", "push", "--include-untracked"],
        vec!["stash", "apply"],
        vec!["stash", "pop"],
        vec!["stash", "drop"],
        vec!["tag"],
        vec!["tag", "--switch"],
        vec!["tag", "--diff"],
        vec!["reflog"],
        vec!["reflog", "--restore", "recovered"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when there is nothing to select"
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

/// 排他オプションを同時に指定した場合、選択を始める前に拒否されることを確認する。
#[test]
fn mutually_exclusive_options_are_rejected_before_anything_runs() {
    let dir = empty_repository("exclusive-options");

    let output = gz()
        .args(["tag", "--switch", "--diff"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz tag --switch --diff");

    assert!(
        !output.status.success(),
        "gz tag --switch --diff should be rejected"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("cannot be used with"),
        "the conflict should be explained:\n{stderr}"
    );
}

/// `gz merge` のマージ方式フラグが相互排他であることを確認する。
#[test]
fn the_merge_strategy_options_are_mutually_exclusive() {
    let dir = empty_repository("merge-exclusive-options");

    for flags in [
        ["--no-ff", "--squash"],
        ["--no-ff", "--ff-only"],
        ["--squash", "--ff-only"],
    ] {
        let output = gz()
            .args(["merge", flags[0], flags[1]])
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz merge");

        assert!(
            !output.status.success(),
            "gz merge {} {} should be rejected",
            flags[0],
            flags[1]
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("cannot be used with"),
            "the conflict should be explained:\n{stderr}"
        );
    }
}

/// `gz diff` の比較モードと `gz sync` の取り込み方式が相互排他であることを確認する。
#[test]
fn the_new_comparison_and_integration_options_are_mutually_exclusive() {
    let dir = empty_repository("new-exclusive-options");

    let combinations = [
        vec!["diff", "--staged", "--head"],
        vec!["diff", "--branch", "--commit"],
        vec!["diff", "--upstream", "--staged"],
        vec!["sync", "--rebase", "--merge"],
    ];

    for arguments in combinations {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should be rejected"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("cannot be used with"),
            "the conflict should be explained for gz {arguments:?}:\n{stderr}"
        );
    }
}

/// 変更が 1 件も無い場合、`gz commit` は TUI を起動せずに終了することを確認する。
#[test]
fn commit_reports_when_there_is_nothing_to_commit() {
    let dir = empty_repository("commit-nothing");

    for arguments in [vec!["commit"], vec!["commit", "--message", "空コミット"]] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when there is nothing to commit"
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

/// リモートが 1 つも無い場合、`gz push` は原因と次の操作を伝えて終了することを確認する。
#[test]
fn push_reports_when_no_remote_is_configured() {
    let dir = empty_repository("push-no-remote");

    for arguments in [vec!["push"], vec!["push", "-u"]] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero without a remote"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("git remote add"),
            "the next step should be suggested for gz {arguments:?}:\n{stderr}"
        );
    }
}

/// force push は fuzgit のスコープ外であり、オプション自体が存在しないことを確認する。
#[test]
fn push_rejects_force_options_because_they_are_out_of_scope() {
    let dir = empty_repository("push-force");

    for flag in ["--force", "--force-with-lease", "-f", "--force-if-includes"] {
        let output = gz()
            .args(["push", flag])
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz push");

        assert!(
            !output.status.success(),
            "gz push {flag} should be rejected"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("unexpected argument"),
            "the unknown flag should be reported:\n{stderr}"
        );
    }
}

/// 引数なしの `gz stash` は、どちらかの操作へ倒さずサブコマンド一覧のヘルプを表示することを確認する。
#[test]
fn bare_stash_shows_its_subcommands_instead_of_choosing_one() {
    let dir = empty_repository("stash-bare");

    let output = gz()
        .arg("stash")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz stash");

    assert!(
        !output.status.success(),
        "bare `gz stash` should not exit successfully"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing should be written to stdout for bare `gz stash`"
    );

    // clap の `arg_required_else_help` はヘルプを stderr へ出す
    let stderr = String::from_utf8(output.stderr).expect("help output should be utf-8");
    for subcommand in STASH_SUBCOMMANDS {
        assert!(
            stderr.contains(subcommand),
            "`{subcommand}` should appear in bare `gz stash` output:\n{stderr}"
        );
    }
}

/// `gz stash --help` にサブコマンド一覧が並ぶことを確認する。
#[test]
fn stash_help_lists_all_of_its_subcommands() {
    let output = gz()
        .args(["stash", "--help"])
        .output()
        .expect("failed to run gz stash --help");

    assert!(output.status.success(), "gz stash --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for subcommand in STASH_SUBCOMMANDS {
        assert!(
            stdout.contains(subcommand),
            "`{subcommand}` should appear in gz stash --help output:\n{stdout}"
        );
    }
}

/// `gz stash <サブコマンド> --help` がそれぞれのヘルプを持つことを確認する。
#[test]
fn every_stash_subcommand_has_its_own_help() {
    for subcommand in STASH_SUBCOMMANDS {
        let output = gz()
            .args(["stash", subcommand, "--help"])
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz stash {subcommand} --help: {err}"));

        assert!(
            output.status.success(),
            "gz stash {subcommand} --help should exit successfully"
        );

        let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
        assert!(
            stdout.contains(&format!("gz stash {subcommand}")),
            "usage line missing for stash {subcommand}:\n{stdout}"
        );
    }
}

/// `gz stash push` のオプションがヘルプに載っていることを確認する。
#[test]
fn stash_push_documents_its_message_and_untracked_options() {
    let output = gz()
        .args(["stash", "push", "--help"])
        .output()
        .expect("failed to run gz stash push --help");

    assert!(
        output.status.success(),
        "gz stash push --help should succeed"
    );

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for option in ["-m", "--message", "-u", "--include-untracked"] {
        assert!(
            stdout.contains(option),
            "`{option}` should be documented:\n{stdout}"
        );
    }
}

/// `FUZGIT_DEBUG=1` のとき、実行した git コマンドが標準エラーへ出ることを確認する。
///
/// 候補が 0 件になるパスでも候補一覧の読み取り（`git status`）は実行されるため、
/// TUI を起動せずに検証できる。
#[test]
fn the_debug_variable_logs_the_git_commands_to_stderr() {
    let dir = empty_repository("debug-log");

    let output = gz()
        .arg("add")
        .env("FUZGIT_DEBUG", "1")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz add with FUZGIT_DEBUG=1");

    let stderr = String::from_utf8(output.stderr).expect("debug output should be utf-8");
    assert!(
        stderr.contains(DEBUG_PREFIX),
        "the debug log should be written to stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("git status"),
        "the executed git command should be logged:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "the debug log must not pollute stdout"
    );
}

/// デバッグログは `FUZGIT_DEBUG=1` のときだけ出ることを確認する。
#[test]
fn other_values_of_the_debug_variable_stay_quiet() {
    let dir = empty_repository("debug-log-off");

    for value in [None, Some(""), Some("0"), Some("true")] {
        let mut command = gz();
        command.arg("add").current_dir(dir.path());
        match value {
            Some(value) => command.env("FUZGIT_DEBUG", value),
            // 呼び出し元の環境に設定が残っていても影響を受けないよう明示的に取り除く
            None => command.env_remove("FUZGIT_DEBUG"),
        };

        let output = command
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz add with {value:?}: {err}"));

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            !stderr.contains(DEBUG_PREFIX),
            "FUZGIT_DEBUG={value:?} must not enable the debug log:\n{stderr}"
        );
    }
}

/// ステージ済みの変更が無い場合、`gz fixup` は候補を選ばせる前に終了することを確認する。
///
/// 選択を終えてから git に失敗させるのは無駄な操作を強いるため、事前チェックで止める
/// （requirements.md FR-11）。コミットがある状態で確認するのは、コミット履歴の候補が
/// 空でないにもかかわらず TUI を起動しないことを検証するため。
#[test]
fn fixup_requires_staged_changes_before_offering_the_commits() {
    let dir = repository_with_an_unstaged_change("fixup-nothing-staged");

    for arguments in [vec!["fixup"], vec!["fixup", "--squash"]] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero without staged changes"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("ステージ済みの変更がありません"),
            "the cause should be explained for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            stderr.contains("gz add"),
            "the next step should be suggested for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// 変更が 1 件も無い場合、`gz status` は TUI を起動せず正常終了することを確認する。
///
/// クリーンな状態の確認も `gz status` の用途であるため、候補ゼロを「候補がない」エラーに
/// しない（requirements.md FR-16）。コミットの有無で経路が変わらないことも合わせて確認する。
#[test]
fn status_reports_a_clean_work_tree_and_exits_successfully() {
    let unborn = empty_repository("status-clean-unborn");
    let committed = empty_repository("status-clean-committed");
    std::fs::write(committed.path().join("a.txt"), "first\n").expect("failed to write a file");
    git_in(committed.path(), &["add", "--", "a.txt"]);
    git_in(
        committed.path(),
        &[
            "-c",
            "user.name=fuzgit test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "--message",
            "first commit",
        ],
    );

    for dir in [&unborn, &committed] {
        let output = gz()
            .arg("status")
            .current_dir(dir.path())
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .expect("failed to run gz status");

        assert!(
            output.status.success(),
            "gz status should succeed on a clean work tree in {:?}",
            dir.path()
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("変更はありません"),
            "the clean state should be reported:\n{stderr}"
        );
        assert!(
            stderr.contains("main") && stderr.contains("stash 0"),
            "the header information should be repeated on stderr:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "stdout is reserved for the paths of a selection"
        );
    }
}

/// 比較範囲に差分が無い場合、`gz diff` は TUI を起動せず正常終了することを確認する。
///
/// 「差分が無い」ことは正常な結果であり、エラーにしない（requirements.md FR-17）。
/// 引数なし（unstaged）と `--staged` / `--head` のいずれでも同じ扱いになることを確かめる。
#[test]
fn diff_reports_an_empty_comparison_and_exits_successfully() {
    let dir = empty_repository("diff-no-difference");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    for arguments in [
        vec!["diff"],
        vec!["diff", "--staged"],
        vec!["diff", "--head"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            output.status.success(),
            "gz {arguments:?} should succeed when there is no difference"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("差分はありません"),
            "the empty comparison should be reported for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "stdout is reserved for the diff itself for gz {arguments:?}"
        );
    }
}

/// `gz diff --upstream` は upstream が無い場合に、その原因を伝えて停止することを確認する。
///
/// 比較対象を決められない以上、暗黙に別の範囲（HEAD など）へ倒さない。
#[test]
fn diff_against_the_upstream_requires_one_to_be_configured() {
    let tracking = empty_repository("diff-upstream-missing");
    commit_in(tracking.path(), "a.txt", "first\n", "first commit");

    let output = gz()
        .args(["diff", "--upstream"])
        .current_dir(tracking.path())
        // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz diff --upstream");

    assert!(
        !output.status.success(),
        "gz diff --upstream should exit non-zero without an upstream"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("upstream が設定されていません"),
        "the missing upstream should be reported:\n{stderr}"
    );
    assert!(
        stderr.contains("--set-upstream-to"),
        "the next step should be suggested:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing should be written to stdout"
    );

    // detached HEAD にはそもそも upstream の設定が無く、原因が異なるため別のメッセージにする
    git_in(tracking.path(), &["switch", "--quiet", "--detach", "HEAD"]);
    let output = gz()
        .args(["diff", "--upstream"])
        .current_dir(tracking.path())
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz diff --upstream on a detached HEAD");

    assert!(
        !output.status.success(),
        "gz diff --upstream should exit non-zero on a detached HEAD"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("detached HEAD"),
        "the detached HEAD should be named as the cause:\n{stderr}"
    );
}

/// `gz diff --help` に比較モードが並び、比較対象を直接指定する引数が無いことを確認する。
///
/// 比較するリビジョン・ファイルは fuzzy finder で選ぶ設計であり、コマンドラインでは指定しない。
#[test]
fn diff_documents_its_comparison_modes_and_takes_no_revision_argument() {
    let output = gz()
        .args(["diff", "--help"])
        .output()
        .expect("failed to run gz diff --help");

    assert!(output.status.success(), "gz diff --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for option in ["--staged", "--head", "--upstream", "--branch", "--commit"] {
        assert!(
            stdout.contains(option),
            "`{option}` should be documented:\n{stdout}"
        );
    }

    let dir = empty_repository("diff-positional");
    let output = gz()
        .args(["diff", "main"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz diff main");
    assert!(
        !output.status.success(),
        "the revisions are chosen in the finder, not on the command line"
    );
}

/// `gz restore --source` に解決できないリビジョンを渡した場合、その名前を含むエラーになることを確認する。
#[test]
fn restore_reports_an_unknown_source_revision_by_name() {
    let dir = empty_repository("restore-unknown-source");

    let output = gz()
        .args(["restore", "--source", "no-such-revision"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz restore --source no-such-revision");

    assert!(
        !output.status.success(),
        "an unknown revision should exit non-zero"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("no-such-revision"),
        "the unknown revision should be named:\n{stderr}"
    );
}

/// リモートが 1 つも無い場合、`gz fetch` は原因と次の操作を伝えて終了することを確認する。
///
/// ネットワークを伴う経路（実際の取得）は自動テストの対象外とし、実機（ローカルの bare
/// リポジトリを remote に設定した使い捨てリポジトリ）で確認する。
#[test]
fn fetch_reports_when_no_remote_is_configured() {
    let dir = empty_repository("fetch-no-remote");

    for arguments in [vec!["fetch"], vec!["fetch", "--prune"]] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero without a remote"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("git remote add"),
            "the next step should be suggested for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// `gz fetch --help` に `--prune` が載っており、取得先の指定オプションが無いことを確認する。
///
/// 取得先は fuzzy finder で選ぶ設計であり、コマンドラインからは指定しない。
#[test]
fn fetch_documents_the_prune_option_and_takes_no_remote_argument() {
    let output = gz()
        .args(["fetch", "--help"])
        .output()
        .expect("failed to run gz fetch --help");

    assert!(output.status.success(), "gz fetch --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    assert!(
        stdout.contains("--prune"),
        "`--prune` should be documented:\n{stdout}"
    );

    let dir = empty_repository("fetch-positional");
    let output = gz()
        .args(["fetch", "origin"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz fetch origin");
    assert!(
        !output.status.success(),
        "the remote is chosen in the finder, not on the command line"
    );
}

/// `gz fetch --help` に `--siblings`（短縮形 `-s`）が載っていることを確認する（FR-23）。
#[test]
fn fetch_documents_the_siblings_option_in_both_spellings() {
    let output = gz()
        .args(["fetch", "--help"])
        .output()
        .expect("failed to run gz fetch --help");

    assert!(output.status.success(), "gz fetch --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for spelling in ["--siblings", "-s"] {
        assert!(
            stdout.contains(spelling),
            "`{spelling}` should be documented:\n{stdout}"
        );
    }
}

/// 走査範囲に fetch できるリポジトリが 1 つも無い場合、`gz fetch --siblings` は
/// finder を起動せずに理由を伝えて終了することを確認する（FR-23）。
///
/// ネットワークを伴う経路（実際の取得）は自動テストの対象外とし、実機（ローカルの bare
/// リポジトリを remote に設定した使い捨てリポジトリ）で確認する。
#[test]
fn fetch_siblings_reports_when_nothing_can_be_fetched() {
    // 走査するのはワークツリー root の親ディレクトリであるため、他のテストの一時
    // ディレクトリが兄弟として並ばないよう、専用の親ディレクトリを用意する
    let parent = TempDir::new("fetch-siblings-scope");
    let work = parent.path().join("work");
    std::fs::create_dir_all(&work).expect("failed to create the work tree");
    git_in(&work, &["init", "--quiet", "--initial-branch=main"]);

    for arguments in [
        vec!["fetch", "--siblings"],
        vec!["fetch", "-s"],
        vec!["fetch", "--prune", "--siblings"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(&work)
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when nothing can be fetched"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("fetch できるリポジトリがありません"),
            "the reason should be given for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// `--siblings` を指定しない限り兄弟リポジトリは走査対象にならないことを確認する（FR-23）。
///
/// 兄弟にリモート付きのリポジトリを置いても、既定の `gz fetch` は現在のリポジトリの
/// リモートだけを見て「リモートが無い」と伝えて終了する。
#[test]
fn fetch_without_the_siblings_flag_never_looks_at_the_neighbours() {
    let parent = TempDir::new("fetch-siblings-default");
    let work = parent.path().join("work");
    let neighbour = parent.path().join("neighbour");
    for directory in [&work, &neighbour] {
        std::fs::create_dir_all(directory).expect("failed to create a work tree");
        git_in(directory, &["init", "--quiet", "--initial-branch=main"]);
    }
    git_in(
        &neighbour,
        &["remote", "add", "origin", "https://example.invalid/o.git"],
    );

    let output = gz()
        .arg("fetch")
        .current_dir(&work)
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz fetch");

    assert!(
        !output.status.success(),
        "the current repository has no remote of its own"
    );
    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("git remote add"),
        "the neighbour must not be used as a fallback:\n{stderr}"
    );
}

/// `gz pull` は取り込めるブランチが無い場合、fetch する前に案内して停止することを確認する。
///
/// ネットワークを伴う経路（リモートごとの `git fetch`、ブランチごとの取り込み）は
/// 自動テストの対象外とし、実機（ローカルの bare リポジトリを remote に設定した
/// 使い捨てリポジトリ）で確認する（`gz fetch` / `gz sync` と同方針）。
/// ここで検証するのは、対象を推測して別のブランチ・リモートへ倒さないこと。
#[test]
fn pull_reports_when_no_branch_can_follow_an_upstream() {
    let dir = empty_repository("pull-no-candidate");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    let output = gz()
        .arg("pull")
        .current_dir(dir.path())
        // 事前チェックが失われた場合に TUI・ネットワークで待ち続けないよう上限を設ける
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz pull");

    assert!(
        !output.status.success(),
        "gz pull should exit non-zero without any candidate"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("gz push -u"),
        "the fuzgit way of setting an upstream should be suggested:\n{stderr}"
    );
    assert!(
        stderr.contains("--set-upstream-to"),
        "the plain git way should be suggested too:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing should be written to stdout"
    );
}

/// `gz pull` が取り込み方式のフラグも対象の位置引数も持たないことを確認する。
///
/// 方式は fast-forward 固定（方式を選ぶのは `gz sync`）、対象は選択で決める設計であり、
/// どちらもコマンドラインから指定させない。
#[test]
fn pull_documents_the_fast_forward_only_integration_and_takes_no_arguments() {
    let output = gz()
        .args(["pull", "--help"])
        .output()
        .expect("failed to run gz pull --help");

    assert!(output.status.success(), "gz pull --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    assert!(
        stdout.contains("fast-forward"),
        "the fixed integration method should be documented:\n{stdout}"
    );

    let dir = empty_repository("pull-arguments");
    for arguments in [
        vec!["pull", "--rebase"],
        vec!["pull", "--merge"],
        vec!["pull", "--siblings"],
        vec!["pull", "--prune"],
        vec!["pull", "main"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should be rejected by the command line definition"
        );
    }
}

/// `gz sync` は upstream が定まらない場合、fetch する前に原因を伝えて停止することを確認する。
///
/// ネットワークを伴う経路（実際の取得・取り込み）は自動テストの対象外とし、実機
/// （ローカルの bare リポジトリを remote に設定した使い捨てリポジトリ）で確認する。
/// ここで検証するのは、対象を推測して別のリモート・ブランチへ倒さないこと。
#[test]
fn sync_requires_an_upstream_before_reaching_the_network() {
    let dir = empty_repository("sync-upstream-missing");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    for arguments in [
        vec!["sync"],
        vec!["sync", "--rebase"],
        vec!["sync", "--merge"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            // 事前チェックが失われた場合に TUI・ネットワークで待ち続けないよう上限を設ける
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero without an upstream"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("upstream が設定されていません"),
            "the missing upstream should be reported for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            stderr.contains("--set-upstream-to"),
            "the next step should be suggested for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }

    // detached HEAD にはそもそも upstream の設定が無く、原因が異なるため別のメッセージにする
    git_in(dir.path(), &["switch", "--quiet", "--detach", "HEAD"]);
    let output = gz()
        .arg("sync")
        .current_dir(dir.path())
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz sync on a detached HEAD");

    assert!(
        !output.status.success(),
        "gz sync should exit non-zero on a detached HEAD"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("detached HEAD"),
        "the detached HEAD should be named as the cause:\n{stderr}"
    );
}

/// `gz sync` の取り込み方式がフラグで、取得先を引数で指定できないことを確認する。
///
/// 対象は現在ブランチの upstream に固定する設計であり、コマンドラインから選ばせない。
#[test]
fn sync_documents_its_integration_modes_and_takes_no_target_argument() {
    let output = gz()
        .args(["sync", "--help"])
        .output()
        .expect("failed to run gz sync --help");

    assert!(output.status.success(), "gz sync --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for option in ["--rebase", "--merge"] {
        assert!(
            stdout.contains(option),
            "`{option}` should be documented:\n{stdout}"
        );
    }

    let dir = empty_repository("sync-positional");
    let output = gz()
        .args(["sync", "origin"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz sync origin");
    assert!(
        !output.status.success(),
        "the target is the upstream, not a command line argument"
    );
}

/// 整理対象が無い場合、`gz worktree prune` はその旨を伝えて正常終了することを確認する。
///
/// 対象が無いのは正常な状態であり、エラーにしない（requirements.md FR-21）。
#[test]
fn worktree_prune_reports_when_there_is_nothing_to_prune() {
    let dir = empty_repository("worktree-prune-nothing");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    let output = gz()
        .args(["worktree", "prune"])
        .current_dir(dir.path())
        // 確認プロンプトへ進んでしまった場合に待ち続けないよう上限を設ける
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz worktree prune");

    assert!(
        output.status.success(),
        "gz worktree prune should succeed when there is nothing to prune"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("整理する worktree はありません"),
        "the empty result should be reported:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "stdout is reserved for the path of a selection"
    );
}

/// linked worktree が無い場合、`gz worktree remove` は候補を出さずに理由を伝えることを確認する。
///
/// main worktree は `git worktree remove` の対象外であり、候補に含めない。
#[test]
fn worktree_remove_never_offers_the_main_worktree() {
    let dir = empty_repository("worktree-remove-nothing");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    let output = gz()
        .args(["worktree", "remove"])
        .current_dir(dir.path())
        // 事前チェックが失われた場合に TUI で待ち続けないよう上限を設ける
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz worktree remove");

    assert!(
        !output.status.success(),
        "gz worktree remove should exit non-zero without a linked worktree"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("削除できる worktree がありません"),
        "the reason should be explained:\n{stderr}"
    );
    assert!(
        stderr.contains("main"),
        "the excluded main worktree should be named:\n{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing should be written to stdout"
    );
}

/// `--into` に存在しないブランチを渡した場合、選択を始める前に停止することを確認する。
///
/// `git branch --merged=<base>` の値は `--` で保護できないため、候補一覧との照合を
/// 通らない名前が git へ渡らないことを非対話パスで確かめる（design.md セキュリティ設計）。
#[test]
fn branch_management_rejects_an_unknown_merge_base_before_selecting() {
    let dir = repository_with_an_unmerged_branch("branch-into-unknown");

    for arguments in [
        vec!["branch", "delete", "--into", "no-such-branch"],
        vec!["branch", "cleanup", "--into", "no-such-branch"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            // 検証が失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero for an unknown base"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("no-such-branch") && stderr.contains("--into"),
            "the option and the name should be named for gz {arguments:?}:\n{stderr}"
        );
    }
}

/// 削除・整理の候補が 1 件も無い場合、TUI を起動せずに理由を伝えることを確認する。
#[test]
fn branch_management_reports_when_there_is_nothing_to_delete() {
    let only_main = empty_repository("branch-delete-nothing");
    commit_in(only_main.path(), "a.txt", "first\n", "first commit");
    let unmerged_only = repository_with_an_unmerged_branch("branch-cleanup-nothing");

    for (dir, arguments, expected) in [
        // 現在のブランチしか無いリポジトリでは削除できるブランチが無い
        (&only_main, vec!["branch", "delete"], "削除できるブランチ"),
        // merged なブランチが無いリポジトリでは整理する対象が無い
        (&unmerged_only, vec!["branch", "cleanup"], "merged"),
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when there is nothing to select"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains(expected),
            "the reason should be explained for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// 管理サブコマンドを実装しても、切替（FR-1）の経路が変わっていないことを確認する。
///
/// 実装済みの管理サブコマンドがあると、引数なしの `gz branch` がそちらへ吸われる余地が
/// 生まれる。ブランチが複数ある実リポジトリで、切替だけが TUI（＝候補選択）へ進むことを
/// 「候補ゼロで止まらないこと」ではなく、削除側が別経路で止まることと対比して確かめる。
#[test]
fn branch_without_a_subcommand_is_unaffected_by_the_management_subcommands() {
    let dir = empty_repository("branch-switch-regression");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    // 切替のヘルプは従来どおり `--all` を持ち、管理サブコマンドも並ぶ
    let output = gz()
        .args(["branch", "--help"])
        .output()
        .expect("failed to run gz branch --help");
    assert!(output.status.success(), "gz branch --help should succeed");
    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    assert!(
        stdout.contains("--all"),
        "`--all` should stay documented:\n{stdout}"
    );
    for subcommand in BRANCH_SUBCOMMANDS {
        assert!(
            stdout.contains(subcommand),
            "`{subcommand}` should be listed:\n{stdout}"
        );
    }

    // 切替用のフラグとサブコマンドの併用は clap の段階で拒否されたまま
    for arguments in [
        vec!["branch", "--all", "delete"],
        vec!["branch", "-a", "cleanup"],
    ] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));
        assert!(
            !output.status.success(),
            "gz {arguments:?} must not be accepted"
        );
    }

    // 管理サブコマンドは切替とは別の経路（削除候補ゼロ）で止まる
    let output = gz()
        .args(["branch", "delete"])
        .current_dir(dir.path())
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz branch delete");
    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("削除できるブランチ"),
        "the delete path should be taken:\n{stderr}"
    );
}

/// `gz cherry-pick --branch` に存在しないブランチを渡した場合、その名前を含むエラーになることを確認する。
#[test]
fn cherry_pick_reports_an_unknown_branch_by_name() {
    let dir = empty_repository("cherry-pick-unknown-branch");

    let output = gz()
        .args(["cherry-pick", "--branch", "no-such-branch"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz cherry-pick --branch no-such-branch");

    assert!(
        !output.status.success(),
        "an unknown branch should exit non-zero"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("no-such-branch"),
        "the unknown branch should be named:\n{stderr}"
    );
}
