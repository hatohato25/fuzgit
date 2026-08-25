//! 共通要件（requirements.md「共通要件」）のうち、非対話パスの統合テスト。
//!
//! TUI（skim）が起動する対話パスは自動テスト対象外とし、手動確認とする。
//!
//! # メッセージのアサーションと表示言語（FR-27）
//!
//! 日本語のメッセージをアサートしているテストは、[`gz`]（`FUZGIT_LANG=ja` 既定）で起動する
//! **ja のアサーション**である。既存のメッセージは要件そのものであるため削除せず、言語を
//! 明示する形で維持する。`fuzgit::error::Error` を訳す `describe` と `commands` 側の文言
//! （`.context(...)` / `bail!(...)`）はいずれも移行済みであり、`en` を選べば連鎖の全体が
//! 英語になる。ただし `clap` が組み立てるヘルプ（`Usage:` 等）だけは別であり、
//! それらをアサートしているテストは言語を問わず英語のままである。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use assert_cmd::Command;
use fuzgit::i18n::Language;
use fuzgit::i18n::messages::CliMessages;

/// 検証対象のバイナリ（パッケージ名 `fuzgit` に対する実行ファイル名は `gz`）。
const BIN_NAME: &str = "gz";

/// `gz` のすべてのサブコマンド名。ヘルプ出力の検証に用いる。
const SUBCOMMANDS: [&str; 17] = [
    "branch",
    "log",
    "cherry-pick",
    "restore",
    "add",
    "stash",
    "reflog",
    "commit",
    "fixup",
    "merge",
    "rebase",
    "revert",
    "status",
    "diff",
    "fetch",
    "pull",
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
///
/// **ja のアサーション**（`fuzgit::i18n::ja` の `describe` が返す文言）。
const UNBORN_HEAD_CAUSE: &str = "まだコミットがありません";

/// [`UNBORN_HEAD_CAUSE`] と同じ原因を伝える **en** の文言（`fuzgit::i18n::en` と対応）。
const UNBORN_HEAD_CAUSE_EN: &str = "has no commits yet";

/// unborn HEAD のエラーメッセージが伝えるべき次の操作。
///
/// 実際に打ち込むコマンドであり「翻訳しないもの」であるため、ja / en で共通。
const UNBORN_HEAD_NEXT_STEP: &str = "git commit";

/// 候補が 1 件も無い場合のエラーメッセージ（`fuzgit::error::Error::NoCandidates` と対応）。
///
/// **ja のアサーション**。
const NO_CANDIDATES: &str = "選択できる候補がありません";

/// [`NO_CANDIDATES`] と同じ内容を伝える **en** の文言。
const NO_CANDIDATES_EN: &str = "no candidates to select from";

/// リポジトリ外での実行を伝えるエラーメッセージ（`fuzgit::error::Error::NotARepository` と対応）。
///
/// **ja のアサーション**。
const NOT_A_REPOSITORY: &str = "git リポジトリではありません";

/// [`NOT_A_REPOSITORY`] と同じ内容を伝える **en** の文言。
const NOT_A_REPOSITORY_EN: &str = "Not a git repository";

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

/// 統合テストが `gz` を起動するときの既定の表示言語。
///
/// 既存のアサーションは日本語の文言を前提としているため `ja` を明示する
/// （実行環境のロケール設定に結果を左右させない）。
const DEFAULT_LANGUAGE: &str = "ja";

/// 表示言語の指定を受け取る環境変数名（`fuzgit::i18n::resolve::LANGUAGE_ENV` と対応）。
const LANGUAGE_ENV: &str = "FUZGIT_LANG";

/// 表示言語の解決（FR-25 の層 4）が参照するロケール環境変数。
///
/// テストを実行するマシンのロケールで結果が変わらないよう、すべて取り除いてから起動する。
const LOCALE_ENVS: [&str; 4] = ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"];

/// 表示言語を明示して `gz` を起動する。
///
/// 言語を引数に取るのは、同じ操作を ja / en の両方で検証できるようにするため。
fn gz_with(language: &str) -> Command {
    let mut command = Command::cargo_bin(BIN_NAME).expect("gz binary should be built");
    command.env(LANGUAGE_ENV, language);
    for key in LOCALE_ENVS {
        command.env_remove(key);
    }
    command
}

/// 既定の表示言語（[`DEFAULT_LANGUAGE`]）で `gz` を起動する。
fn gz() -> Command {
    gz_with(DEFAULT_LANGUAGE)
}

/// 文字列に日本語の文字が含まれるかどうかを判定する。
///
/// `en` を選んだ出力に日本語が混ざっていないことを確かめるために用いる。逆
/// （日本語の出力に英語が混ざらないこと）は検査しない。`git commit` のように
/// **翻訳しない語**が日本語の文言にも意図的に含まれるため。
fn contains_japanese(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(character,
            '\u{3000}'..='\u{303F}'   // 全角の記号・句読点
            | '\u{3040}'..='\u{309F}' // ひらがな
            | '\u{30A0}'..='\u{30FF}' // カタカナ
            | '\u{4E00}'..='\u{9FFF}' // 漢字
            | '\u{FF00}'..='\u{FFEF}' // 全角形
        )
    })
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

/// upstream を設定したローカルの bare リポジトリを追跡する git リポジトリを用意する。
///
/// 取得元をローカルに置くため、`gz pull` を**ネットワークへ出さずに**最後まで実行できる
/// （候補が 1 件だけになるため finder も起動しない）。戻り値の 1 つ目は取得元であり、
/// 破棄されると追跡先が消えるため呼び出し側で保持する。
fn repository_tracking_a_local_remote(label: &str) -> (TempDir, TempDir) {
    let remote = TempDir::new(&format!("{label}-remote"));
    git_in(
        remote.path(),
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let remote_path = remote
        .path()
        .to_str()
        .expect("the path should be utf-8")
        .to_owned();

    let work = empty_repository(label);
    commit_in(work.path(), "a.txt", "first\n", "first commit");
    git_in(work.path(), &["remote", "add", "origin", &remote_path]);
    git_in(
        work.path(),
        &["push", "--quiet", "--set-upstream", "origin", "main"],
    );

    (remote, work)
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

/// リポジトリ外での実行が専用のメッセージで停止することを確認する（**ja**）。
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
        stderr.contains(NOT_A_REPOSITORY),
        "unexpected stderr:\n{stderr}"
    );
}

/// 同じ経路が `en` では英語のメッセージになることを確認する（FR-27）。
///
/// この経路は `.context(...)` を挟まないため、連鎖全体が `describe` の英語だけで構成される。
/// 日本語が 1 文字も混ざらないことまで確かめられる数少ない経路である。
#[test]
fn running_outside_a_repository_is_reported_in_english() {
    let dir = TempDir::new("outside-en");

    let output = gz_with("en")
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
        stderr.contains(NOT_A_REPOSITORY_EN),
        "unexpected stderr:\n{stderr}"
    );
    assert!(
        !contains_japanese(&stderr),
        "the english output must not mix japanese:\n{stderr}"
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

/// `--help` を検証する対象と、そこへ出るべき `about` の組を並べる。
///
/// 20 のサブコマンドに加え、入れ子のサブコマンド（`branch` / `stash` / `worktree`）も
/// すべて並べる。ヘルプの差し替えは組み立て済みの `clap` の定義を**実行時**に書き換える
/// ため、呼び忘れがコンパイルエラーにならない。この網羅が唯一の担保である
/// （design.md「差し替え漏れはコンパイルエラーにならない」）。
fn help_targets(cli: &dyn CliMessages) -> Vec<(&'static [&'static str], &'static str)> {
    vec![
        (&[], cli.about()),
        (&["branch"], cli.branch_about()),
        (&["branch", "create"], cli.branch_create_about()),
        (&["branch", "delete"], cli.branch_delete_about()),
        (&["branch", "cleanup"], cli.branch_cleanup_about()),
        (&["log"], cli.log_about()),
        (&["cherry-pick"], cli.cherry_pick_about()),
        (&["restore"], cli.restore_about()),
        (&["add"], cli.add_about()),
        (&["stash"], cli.stash_about()),
        (&["stash", "push"], cli.stash_push_about()),
        (&["stash", "apply"], cli.stash_apply_about()),
        (&["stash", "pop"], cli.stash_pop_about()),
        (&["stash", "drop"], cli.stash_drop_about()),
        (&["reflog"], cli.reflog_about()),
        (&["commit"], cli.commit_about()),
        (&["fixup"], cli.fixup_about()),
        (&["merge"], cli.merge_about()),
        (&["rebase"], cli.rebase_about()),
        (&["revert"], cli.revert_about()),
        (&["status"], cli.status_about()),
        (&["diff"], cli.diff_about()),
        (&["fetch"], cli.fetch_about()),
        (&["pull"], cli.pull_about()),
        (&["worktree"], cli.worktree_about()),
        (&["worktree", "add"], cli.worktree_add_about()),
        (&["worktree", "remove"], cli.worktree_remove_about()),
        (&["worktree", "prune"], cli.worktree_prune_about()),
    ]
}

/// 検証対象の全コマンドについて、`--help` が選んだ言語の説明を出すことを確認する（FR-27）。
///
/// 併せて `--lang` の説明も検査する。`--lang` はグローバル引数であり、差し替えた定義が
/// 各サブコマンドへ伝播していることはこのアサーションでしか確かめられない。
#[test]
fn every_help_is_shown_in_the_selected_language() {
    for language in [Language::Japanese, Language::English] {
        let cli = language.messages().cli();

        for (path, about) in help_targets(cli) {
            let output = gz_with(language.code())
                .args(path)
                .arg("--help")
                .output()
                .unwrap_or_else(|err| panic!("failed to run gz {path:?} --help: {err}"));

            assert!(
                output.status.success(),
                "gz {path:?} --help should exit successfully"
            );

            let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
            assert!(
                stdout.contains(about),
                "the description of gz {path:?} is not shown in {language:?}:\n{stdout}"
            );
            assert!(
                stdout.contains(cli.lang_help()),
                "the description of --lang is not shown in {language:?} for gz {path:?}:\n{stdout}"
            );

            // 日本語のヘルプには `Usage:` などの clap 自身の英語が必ず混ざるため、
            // 「別の言語が混ざっていないこと」を確かめられるのは en だけ
            if language == Language::English {
                assert!(
                    !contains_japanese(&stdout),
                    "the english help must not mix japanese for gz {path:?}:\n{stdout}"
                );
            }
        }
    }
}

/// 差し替え済みの定義を再帰的に辿り、説明の欠落と差し替え漏れが無いことを確かめる。
///
/// `clap` が自ら足す `-h` / `--help` / `-V` / `--version` と `help` サブコマンドは
/// `clap` 自身の文言（requirements.md のスコープ外）であるため対象から除く。
fn assert_descriptions_are_localized(command: &clap::Command, path: &str, language: Language) {
    let about = command
        .get_about()
        .unwrap_or_else(|| panic!("`{path}` has no about"))
        .to_string();
    assert_is_written_in(&about, language, &format!("the about of `{path}`"));

    for argument in command.get_arguments() {
        let id = argument.get_id().as_str();
        if id == "help" || id == "version" {
            continue;
        }

        let help = argument
            .get_help()
            .unwrap_or_else(|| panic!("`{path}` has no help for `{id}`"))
            .to_string();
        assert_is_written_in(&help, language, &format!("the help of `{path}` `{id}`"));
    }

    for subcommand in command.get_subcommands() {
        let name = subcommand.get_name();
        if name == "help" {
            continue;
        }

        assert_descriptions_are_localized(subcommand, &format!("{path} {name}"), language);
    }
}

/// 文言が空でなく、その言語で書かれていることを確かめる。
///
/// derive のリテラル（＝英語のフォールバック）が日本語を含まないことは
/// `fuzgit::cli` の単体テストが固定しているため、日本語の文字が現れることが
/// **差し替えが行われた証拠**になる。
fn assert_is_written_in(text: &str, language: Language, label: &str) {
    assert!(!text.trim().is_empty(), "{label} is empty");

    match language {
        Language::Japanese => assert!(
            contains_japanese(text),
            "{label} was left untranslated: {text}"
        ),
        Language::English => assert!(
            !contains_japanese(text),
            "{label} must stay in english: {text}"
        ),
    }
}

/// 全サブコマンド・全オプションに、選んだ言語の説明が付いていることを確認する。
///
/// [`every_help_is_shown_in_the_selected_language`] が `about` を実プロセスで確かめるのに
/// 対し、こちらは**すべての引数**まで含めて網羅する。オプションを 1 つ足して
/// `localized_command` の差し替えを書き忘れると、ここが英語のまま残っていることを検出する。
#[test]
fn every_description_of_the_localized_definition_is_translated() {
    for language in [Language::Japanese, Language::English] {
        let command = fuzgit::cli::localized_command(language.messages());

        assert_descriptions_are_localized(&command, "gz", language);
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

/// コミットがまだ 1 件も無いリポジトリでは、候補ゼロではなくその原因を伝えることを確認する（**ja**）。
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
            !stderr.contains(NO_CANDIDATES),
            "the generic message should not be used for gz {subcommand}:\n{stderr}"
        );
    }
}

/// 同じ原因と次の操作が `en` でも伝わることを確認する（FR-27）。
///
/// この経路は `commands` 側の文脈（`.context(...)`）を挟むため、連鎖の先頭と
/// `describe` が返す部分の双方が英語になっていなければならない。
#[test]
fn an_unborn_head_is_reported_in_english() {
    let dir = empty_repository("unborn-head-en");

    let output = gz_with("en")
        .arg("log")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz log");

    assert!(
        !output.status.success(),
        "gz log should exit non-zero when there are no commits"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains(UNBORN_HEAD_CAUSE_EN),
        "the cause should be explained in english:\n{stderr}"
    );
    assert!(
        stderr.contains(UNBORN_HEAD_NEXT_STEP),
        "the next step should be suggested:\n{stderr}"
    );
    assert!(
        !stderr.contains(UNBORN_HEAD_CAUSE),
        "the japanese description must not be used:\n{stderr}"
    );
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

/// 変更が 1 件も無い場合、`gz restore` / `gz add` は TUI を起動せずに終了することを確認する（**ja**）。
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
            stderr.contains(NO_CANDIDATES),
            "unexpected stderr for gz {arguments:?}:\n{stderr}"
        );
    }
}

/// 候補が 1 件も無いことが `en` では英語で伝わることを確認する（FR-27）。
///
/// `gz add` の候補ゼロは `.context(...)` を挟まずに `Error::NoCandidates` が返る経路であり、
/// 連鎖全体が `describe` の英語だけで構成される。
#[test]
fn nothing_to_select_is_reported_in_english() {
    let dir = empty_repository("nothing-to-select-en");

    let output = gz_with("en")
        .arg("add")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz add");

    assert!(
        !output.status.success(),
        "gz add should exit non-zero when there is nothing to select"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains(NO_CANDIDATES_EN),
        "unexpected stderr:\n{stderr}"
    );
    assert!(
        !contains_japanese(&stderr),
        "the english output must not mix japanese:\n{stderr}"
    );
}

/// stash / reflog が 1 件も無い場合、TUI を起動せずに終了することを確認する（**ja**）。
#[test]
fn stash_and_reflog_report_when_there_is_nothing_to_select() {
    let dir = empty_repository("nothing-to-select-p3");

    for arguments in [
        vec!["stash", "push"],
        vec!["stash", "push", "--include-untracked"],
        vec!["stash", "apply"],
        vec!["stash", "pop"],
        vec!["stash", "drop"],
        vec!["reflog"],
        vec!["reflog", "--restore", "recovered"],
        vec!["reflog", "--action"],
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
            stderr.contains(NO_CANDIDATES),
            "unexpected stderr for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// `--action` を付けない `gz log` / `gz reflog` の非対話パスが従来どおりであることを固定する。
///
/// **本要件（FR-32）で最も壊してはいけない性質**は、既定の標準出力が 1 バイトも変わらない
/// ことである（`$(gz log)` によるコマンド置換を壊さない）。候補が 1 件も無いリポジトリでは
/// TUI を起動せずに終了するため、対話なしでこの経路を検査できる。
#[test]
fn the_default_output_of_log_and_reflog_is_unchanged_by_the_action_flag() {
    let dir = empty_repository("action-flag-keeps-the-default");

    for arguments in [vec!["log"], vec!["reflog"]] {
        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {arguments:?}: {err}"));

        // 候補が無い場合は選択を始めず、標準出力は空のまま終了する
        assert!(
            !output.status.success(),
            "gz {arguments:?} should exit non-zero when there is nothing to select"
        );
        assert!(
            output.stdout.is_empty(),
            "gz {arguments:?} must not write anything to stdout"
        );

        // `--action` を足しても、候補が無い段階での挙動は変わらない
        let mut with_flag = arguments.clone();
        with_flag.push("--action");
        let menu = gz()
            .args(&with_flag)
            .current_dir(dir.path())
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz {with_flag:?}: {err}"));

        assert_eq!(
            output.status.code(),
            menu.status.code(),
            "gz {with_flag:?} must exit like gz {arguments:?}"
        );
        assert_eq!(
            output.stdout, menu.stdout,
            "gz {with_flag:?} must not change what reaches stdout"
        );
        assert_eq!(
            output.stderr, menu.stderr,
            "gz {with_flag:?} must not change what reaches stderr"
        );
    }
}

/// `gz reflog --restore` と `--action` が同時に指定できないことを確認する。
#[test]
fn the_reflog_restore_and_action_options_are_mutually_exclusive() {
    let dir = empty_repository("reflog-exclusive-options");

    let output = gz()
        .args(["reflog", "--restore", "recovered", "--action"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz reflog --restore recovered --action");

    assert!(
        !output.status.success(),
        "gz reflog --restore recovered --action should be rejected"
    );

    // clap 由来の英語の文言であり、表示言語の影響を受けない
    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("cannot be used with"),
        "the conflict should be explained:\n{stderr}"
    );

    assert!(
        output.stdout.is_empty(),
        "nothing should be written to stdout"
    );
}

/// 排他オプションを同時に指定した場合、選択を始める前に拒否されることを確認する。
#[test]
fn mutually_exclusive_options_are_rejected_before_anything_runs() {
    let dir = empty_repository("exclusive-options");

    let output = gz()
        .args(["diff", "--staged", "--head"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz diff --staged --head");

    assert!(
        !output.status.success(),
        "gz diff --staged --head should be rejected"
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
        vec!["pull", "--rebase", "--merge"],
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
            stderr.contains(NO_CANDIDATES),
            "unexpected stderr for gz {arguments:?}:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing should be written to stdout for gz {arguments:?}"
        );
    }
}

/// `gz push` は提供しないことを確認する。
///
/// fuzzy finder で「選ぶ」価値のある軸が無いため、素の `git push` に委ねる
/// （requirements.md「スコープ外」）。
#[test]
fn push_is_not_offered_as_a_subcommand() {
    let dir = empty_repository("push-removed");

    let output = gz()
        .arg("push")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz push");

    assert!(
        !output.status.success(),
        "gz push should be rejected as an unknown subcommand"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("unrecognized subcommand"),
        "the unknown subcommand should be reported:\n{stderr}"
    );
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

/// デバッグログが (A)(B) の分類と、設定したロケール環境変数を併記することを確認する。
///
/// 候補一覧の読み取りは fuzgit が出力をパースする (A) 系であり、メッセージ言語を
/// `LC_MESSAGES=C` に固定したことがログから追えなければならない（FR-26）。
/// (B) 系は TUI を起動する経路にしか現れないため、行の組み立ての検証は
/// `fuzgit::git::exec` の単体テストが受け持つ。
#[test]
fn the_debug_log_annotates_the_classification_and_the_locale() {
    let dir = empty_repository("debug-log-locale");

    let output = gz()
        .arg("add")
        .env("FUZGIT_DEBUG", "1")
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz add with FUZGIT_DEBUG=1");

    let stderr = String::from_utf8(output.stderr).expect("debug output should be utf-8");
    let logged = stderr
        .lines()
        .find(|line| line.starts_with(DEBUG_PREFIX) && line.contains("git status"))
        .unwrap_or_else(|| panic!("the candidate listing should be logged:\n{stderr}"));

    assert!(
        logged.contains("(A)"),
        "the parsed classification should be logged: {logged}"
    );
    assert!(
        logged.contains("LC_MESSAGES=C"),
        "the pinned message locale should be logged: {logged}"
    );
}

/// `FUZGIT_DEBUG=1` のとき、解決された表示言語と決め手になった層が出ることを確認する
/// （requirements.md FR-25）。
///
/// 層 1（`--lang`）と層 2（`FUZGIT_LANG`）は実行環境に左右されずに再現できるため、
/// 統合テストではこの 2 つを確かめる。層 4・5 は開発者の `~/.gitconfig` に依存しうるので
/// 行の組み立ての検証は `fuzgit::i18n::resolve` の単体テストが受け持つ。
#[test]
fn the_debug_log_reports_the_resolved_language_and_its_layer() {
    let dir = empty_repository("debug-log-language");

    for (mut command, expected) in [
        (gz_with("ja"), "language=ja (source: FUZGIT_LANG)"),
        (
            {
                let mut command = gz_with("ja");
                command.args(["--lang", "en"]);
                command
            },
            "language=en (source: --lang)",
        ),
    ] {
        let output = command
            .arg("add")
            .env("FUZGIT_DEBUG", "1")
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz add with FUZGIT_DEBUG=1");

        let stderr = String::from_utf8(output.stderr).expect("debug output should be utf-8");
        assert!(
            stderr
                .lines()
                .any(|line| line == format!("{DEBUG_PREFIX} {expected}")),
            "the resolved language should be logged as `{expected}`:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "the debug log must not pollute stdout"
        );
    }
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
///
/// 失敗した読み取り操作は `fuzgit::git::read::ReadOperation` という値であり、文へ組み立てるのは
/// 言語ごとの `describe` である。言語を明示して起動し、リビジョン名（翻訳しないもの）と
/// 操作の説明（翻訳するもの）の双方が出ることを確かめる。
#[test]
fn restore_reports_an_unknown_source_revision_by_name() {
    // (表示言語, 操作の説明)
    for (language, operation) in [
        ("ja", "リビジョン `no-such-revision` の解決"),
        ("en", "resolving the revision `no-such-revision`"),
    ] {
        let dir = empty_repository(&format!("restore-unknown-source-{language}"));

        let output = gz_with(language)
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
        assert!(
            stderr.contains(operation),
            "{language} should describe the failed operation:\n{stderr}"
        );
    }
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

/// `fuzgit.fetchJobs` が不正な場合、1 件も fetch せずに停止することを確認する（FR-28）。
///
/// 同時実行数は通信を始める前に解決するため、設定を直さない限りネットワークへは出ない。
/// 暗黙に既定値へ倒さず、読み取った値を示して停止する（暗黙のフォールバック禁止）。
#[test]
fn fetch_siblings_stops_before_fetching_when_the_job_count_is_invalid() {
    // 他のテストの一時ディレクトリが兄弟として並ばないよう、専用の親を用意する
    let parent = TempDir::new("fetch-siblings-jobs");
    let work = parent.path().join("work");
    std::fs::create_dir_all(&work).expect("failed to create the work tree");
    git_in(&work, &["init", "--quiet", "--initial-branch=main"]);
    // 候補として選ばれるにはリモートが要る。到達不能な URL にしておけば、設定の検証を
    // 素通りした場合にだけ通信を試みることになり、「通信前に止まる」ことを検証できる
    git_in(
        &work,
        &["remote", "add", "origin", "https://example.invalid/o.git"],
    );

    for value in ["0", "many"] {
        git_in(&work, &["config", "fuzgit.fetchJobs", value]);

        let output = gz()
            .args(["fetch", "--siblings"])
            .current_dir(&work)
            .env("FUZGIT_DEBUG", "1")
            // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
            .timeout(FINDER_GUARD_TIMEOUT)
            .output()
            .unwrap_or_else(|err| panic!("failed to run gz fetch --siblings: {err}"));

        assert!(
            !output.status.success(),
            "`fuzgit.fetchJobs = {value}` should stop the run"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("fuzgit.fetchJobs"),
            "the setting at fault should be named for `{value}`:\n{stderr}"
        );
        assert!(
            !stderr.contains("git fetch"),
            "no repository should be fetched for `{value}`:\n{stderr}"
        );
    }
}

/// `fuzgit.notify` が不正な場合、1 件も fetch せずに停止することを確認する（FR-29）。
///
/// 通知の設定は通信を始める前に解決する。既定（通知しない）へ黙って倒すと、有効に
/// したつもりの設定が効かないまま実行が終わってしまう（暗黙のフォールバック禁止）。
#[test]
fn fetch_siblings_stops_before_fetching_when_the_notification_setting_is_invalid() {
    // 他のテストの一時ディレクトリが兄弟として並ばないよう、専用の親を用意する
    let parent = TempDir::new("fetch-siblings-notify-invalid");
    let work = parent.path().join("work");
    std::fs::create_dir_all(&work).expect("failed to create the work tree");
    git_in(&work, &["init", "--quiet", "--initial-branch=main"]);
    // 候補として選ばれるにはリモートが要る。到達不能な URL にしておけば、設定の検証を
    // 素通りした場合にだけ通信を試みることになり、「通信前に止まる」ことを検証できる
    git_in(
        &work,
        &["remote", "add", "origin", "https://example.invalid/o.git"],
    );
    git_in(&work, &["config", "fuzgit.notify", "sometimes"]);

    let output = gz()
        .args(["fetch", "--siblings"])
        .current_dir(&work)
        .env("FUZGIT_DEBUG", "1")
        // 事前チェックが失われた場合に TUI で待ち続けないよう、上限を設けて打ち切る
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz fetch --siblings");

    assert!(
        !output.status.success(),
        "an invalid `fuzgit.notify` should stop the run"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("fuzgit.notify"),
        "the setting at fault should be named:\n{stderr}"
    );
    assert!(
        !stderr.contains("git fetch"),
        "no repository should be fetched:\n{stderr}"
    );
}

/// 通知を有効にしても `gz fetch --siblings` の終了コードが変わらないことを確認する（FR-29）。
///
/// 通知コマンドが存在しない環境（CI・最小構成の Linux）でも、通知の失敗は握り潰されて
/// 主処理の結果に影響しない。この実行は閾値より短いためそもそも通知は発火せず、ここで
/// 検証するのは「通知を有効にしただけで結果が変わらないこと」と「集計は必ず出ること」。
/// 通知が実際に表示されるかどうかは手動確認の対象とする。
#[test]
fn fetch_siblings_succeeds_with_the_notification_enabled() {
    // ネットワークへ出ないよう、取得元はローカルの bare リポジトリにする。
    // 走査対象の親ディレクトリの外へ置き、兄弟の候補に混ざらないようにする
    let remote = TempDir::new("fetch-notify-remote");
    git_in(
        remote.path(),
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let remote_path = remote.path().to_str().expect("the path should be utf-8");

    let parent = TempDir::new("fetch-notify-scope");
    let work = parent.path().join("work");
    std::fs::create_dir_all(&work).expect("failed to create the work tree");
    git_in(&work, &["init", "--quiet", "--initial-branch=main"]);
    commit_in(&work, "a.txt", "first\n", "first commit");
    git_in(&work, &["remote", "add", "origin", remote_path]);
    git_in(&work, &["config", "fuzgit.notify", "true"]);

    let output = gz()
        .args(["fetch", "--siblings"])
        .current_dir(&work)
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz fetch --siblings");

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        output.status.success(),
        "a failing notification must not change the exit code:\n{stderr}"
    );
    assert!(
        stderr.contains("成功 1 件 / 失敗 0 件"),
        "the summary must be written whether or not the notification appears:\n{stderr}"
    );
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
        stderr.contains("git push -u <remote> <branch>"),
        "the way to push a new branch should be suggested:\n{stderr}"
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

/// 通知を有効にしても `gz pull` の終了コードが変わらないことを確認する（FR-29）。
///
/// `gz pull` は直列実行のままであり、通知の発火条件は並列化（FR-28）の有無に依存しない。
/// この実行は閾値より短いため通知は発火せず、ここで検証するのは「通知を有効にしただけで
/// 結果が変わらないこと」と「集計は必ず出ること」。
#[test]
fn pull_succeeds_with_the_notification_enabled() {
    let (_remote, work) = repository_tracking_a_local_remote("pull-notify");
    git_in(work.path(), &["config", "fuzgit.notify", "true"]);

    let output = gz()
        .arg("pull")
        .current_dir(work.path())
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz pull");

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        output.status.success(),
        "a failing notification must not change the exit code:\n{stderr}"
    );
    assert!(
        stderr.contains("成功 1 件 / 失敗 0 件"),
        "the summary must be written whether or not the notification appears:\n{stderr}"
    );
}

/// `fuzgit.notify` が不正な場合、`gz pull` も 1 件も fetch せずに停止することを確認する（FR-29）。
#[test]
fn pull_stops_before_fetching_when_the_notification_setting_is_invalid() {
    let (_remote, work) = repository_tracking_a_local_remote("pull-notify-invalid");
    git_in(work.path(), &["config", "fuzgit.notify", "sometimes"]);

    let output = gz()
        .arg("pull")
        .current_dir(work.path())
        .env("FUZGIT_DEBUG", "1")
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz pull");

    assert!(
        !output.status.success(),
        "an invalid `fuzgit.notify` should stop the run"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("fuzgit.notify"),
        "the setting at fault should be named:\n{stderr}"
    );
    assert!(
        !stderr.contains("git fetch"),
        "no remote should be fetched:\n{stderr}"
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
fn pull_with_a_mode_requires_an_upstream_before_reaching_the_network() {
    let dir = empty_repository("pull-upstream-missing");
    commit_in(dir.path(), "a.txt", "first\n", "first commit");

    for arguments in [vec!["pull", "--rebase"], vec!["pull", "--merge"]] {
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
        .args(["pull", "--rebase"])
        .current_dir(dir.path())
        .timeout(FINDER_GUARD_TIMEOUT)
        .output()
        .expect("failed to run gz pull --rebase on a detached HEAD");

    assert!(
        !output.status.success(),
        "gz pull --rebase should exit non-zero on a detached HEAD"
    );

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        stderr.contains("detached HEAD"),
        "the detached HEAD should be named as the cause:\n{stderr}"
    );
}

/// `gz pull` の取り込み方式がフラグで、取得先を引数で指定できないことを確認する。
///
/// `--rebase` / `--merge` の対象は現在ブランチの upstream に固定する設計であり、
/// コマンドラインから選ばせない。
#[test]
fn pull_documents_its_integration_modes_and_takes_no_target_argument() {
    let output = gz()
        .args(["pull", "--help"])
        .output()
        .expect("failed to run gz pull --help");

    assert!(output.status.success(), "gz pull --help should succeed");

    let stdout = String::from_utf8(output.stdout).expect("help output should be utf-8");
    for option in ["--rebase", "--merge"] {
        assert!(
            stdout.contains(option),
            "`{option}` should be documented:\n{stdout}"
        );
    }

    let dir = empty_repository("pull-positional");
    let output = gz()
        .args(["pull", "origin"])
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz pull origin");
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
///
/// [`restore_reports_an_unknown_source_revision_by_name`] と同じく、言語を明示して
/// ブランチ名（翻訳しないもの）と操作の説明（翻訳するもの）の双方を確かめる。
#[test]
fn cherry_pick_reports_an_unknown_branch_by_name() {
    // (表示言語, 操作の説明)
    for (language, operation) in [
        ("ja", "ブランチ `no-such-branch` の解決"),
        ("en", "resolving the branch `no-such-branch`"),
    ] {
        let dir = empty_repository(&format!("cherry-pick-unknown-branch-{language}"));

        let output = gz_with(language)
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
        assert!(
            stderr.contains(operation),
            "{language} should describe the failed operation:\n{stderr}"
        );
    }
}

/// `gh` が PATH 上に無い場合、`gz pr` だけが専用エラーで停止する。
///
/// `gh` は**必須依存ではない**（制約条件が前提とする外部コマンドは `git` のみ）。
/// このテストはその境界を固定する: `gz pr` は止まるが、`gh` を通らない他のコマンドは
/// 同じ環境で従来どおり動く。
///
/// `PATH` から `gh` を取り除くために、`git` だけを含む一時ディレクトリを `PATH` にする。
#[test]
fn pr_stops_with_a_dedicated_error_when_gh_is_missing() {
    // (表示言語, エラーに含まれるべき語)
    for (language, expected) in [("ja", "gh"), ("en", "gh")] {
        let dir = empty_repository(&format!("pr-without-gh-{language}"));
        let path = git_only_path(&format!("pr-without-gh-path-{language}"));

        let output = gz_with(language)
            .arg("pr")
            .env("PATH", path.path())
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz pr");

        assert!(
            !output.status.success(),
            "gh が無ければ gz pr は非ゼロ終了すること"
        );

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains(expected),
            "{language} のエラーは gh の不在を名指しすること:\n{stderr}"
        );
        assert!(
            stderr.contains("cli.github.com"),
            "{language} のエラーは導入先を案内すること:\n{stderr}"
        );
    }
}

/// `gh` が無くても、`gh` を通らないコマンドは従来どおり動く。
///
/// 上のテストと対になっており、「`gh` の不在が `gz pr` だけを止める」ことを両側から固定する。
#[test]
fn the_other_commands_keep_working_without_gh() {
    let dir = empty_repository("without-gh-other-commands");
    let path = git_only_path("without-gh-other-commands-path");

    let output = gz()
        .arg("status")
        .env("PATH", path.path())
        .current_dir(dir.path())
        .output()
        .expect("failed to run gz status");

    let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
    assert!(
        !stderr.contains("gh"),
        "gh の不在が他のコマンドへ波及しないこと:\n{stderr}"
    );
    assert!(
        output.status.success(),
        "変更の無い作業ツリーでの gz status は成功すること:\n{stderr}"
    );
}

/// `git` だけを含み `gh` を含まない `PATH` 用のディレクトリを用意する。
///
/// `PATH` を空にすると `git` まで失われ、`gh` の不在ではなく `git` の不在を
/// 見ていることになる。確かめたいのは `gh` の不在だけであるため、`git` は残す。
fn git_only_path(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    let git = which_git();
    std::os::unix::fs::symlink(&git, dir.path().join("git"))
        .expect("git へのシンボリックリンクを作れること");
    dir
}

/// 実行環境の `git` の絶対パスを求める。
fn which_git() -> std::path::PathBuf {
    let output = std::process::Command::new("/usr/bin/env")
        .args(["which", "git"])
        .output()
        .expect("which git should run");
    let path = String::from_utf8(output.stdout).expect("which output should be utf-8");
    std::path::PathBuf::from(path.trim())
}

/// `-b` 無しで候補が 0 件のとき、行き止まりにせず `-b` を案内する。
///
/// ローカルブランチが `main` 1 本で本体が使用中、というごく普通の状態で候補が
/// ゼロになるのが FR-31 の出発点である。**暗黙に `-b` の動作へ倒さない**ことも
/// 併せて確かめる（案内するだけで、勝手にブランチを作らない）。
#[test]
fn worktree_add_without_a_branch_points_at_the_new_branch_flag() {
    // (表示言語, 案内に含まれるべき語)
    for (language, expected) in [
        ("ja", "新しいブランチを作って"),
        ("en", "create a new branch"),
    ] {
        let dir = repository_with_one_commit(&format!("worktree-add-dead-end-{language}"));

        let output = gz_with(language)
            .args(["worktree", "add", "wt"])
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz worktree add");

        assert!(!output.status.success(), "候補ゼロでは非ゼロ終了すること");

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains("-b"),
            "{language} は `-b` を名指しで案内すること:\n{stderr}"
        );
        assert!(
            stderr.contains(expected),
            "{language} は `-b` で何ができるのかを示すこと:\n{stderr}"
        );
        assert!(
            !dir.path().parent().is_some_and(|p| p.join("wt").exists()),
            "案内するだけで、暗黙に worktree を作ってはならない"
        );
    }
}

/// `-b` に既存のローカルブランチ名を渡すと、finder を開く前に停止する。
///
/// 選ばせたあとで「その名前は使えません」と告げない。`-b` を外せばよいことも案内する。
#[test]
fn worktree_add_rejects_a_new_branch_name_that_already_exists() {
    for (language, expected) in [("ja", "既に存在します"), ("en", "already exists")] {
        let dir = repository_with_one_commit(&format!("worktree-add-dup-branch-{language}"));

        let output = gz_with(language)
            .args(["worktree", "add", "-b", "main", "wt"])
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz worktree add -b");

        assert!(!output.status.success(), "衝突する名前は非ゼロ終了すること");

        let stderr = String::from_utf8(output.stderr).expect("error output should be utf-8");
        assert!(
            stderr.contains(expected),
            "{language} は衝突を伝えること:\n{stderr}"
        );
        assert!(
            stderr.contains("-b"),
            "{language} は `-b` を外せばよいことを案内すること:\n{stderr}"
        );
    }
}

/// worktree の名前にパス区切りを含むものは、`-b` の有無に関わらず拒否する。
///
/// 受け取るのは**名前であってパスではない**。読み替えずに理由を示して止める。
#[test]
fn worktree_add_refuses_a_name_that_is_a_path() {
    for arguments in [
        vec!["worktree", "add", "../escape"],
        vec!["worktree", "add", "-b", "feature", "../escape"],
    ] {
        let dir = repository_with_one_commit("worktree-add-path-name");

        let output = gz()
            .args(&arguments)
            .current_dir(dir.path())
            .output()
            .expect("failed to run gz worktree add");

        assert!(
            !output.status.success(),
            "パスを名前として受け取ってはならない: {arguments:?}"
        );
    }
}

/// コミットを 1 件だけ持つ git リポジトリを用意する。
///
/// `-b` の作成元候補は「コミットまたはタグ」であるため、コミットが 1 件要る。
/// 一方で本体が `main` を使用中になるため、`-b` 無しの候補は 0 件になる
/// ——これが FR-31 が塞ぐ穴そのものである。
fn repository_with_one_commit(label: &str) -> TempDir {
    let dir = empty_repository(label);
    let status = std::process::Command::new("git")
        .args(["commit", "--quiet", "--allow-empty", "-m", "initial"])
        .current_dir(dir.path())
        .status()
        .expect("git commit should run");
    assert!(status.success(), "git commit should succeed");
    dir
}
