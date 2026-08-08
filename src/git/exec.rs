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

/// エラーメッセージ表示用のコマンド名（`git commit` 等）を組み立てる。
///
/// 継承 stdio 実行では git 自身のメッセージが既に端末へ出ているため、引数を全て並べると
/// pathspec の列で本当の原因が埋もれる。そこでサブコマンド名までに切り詰める。
/// オプションや対象（リビジョン・パス）は、呼び出し側が `anyhow` の文脈で補う。
fn command_display(args: &[&str]) -> String {
    match args.first() {
        Some(subcommand) => format!("git {subcommand}"),
        None => "git".to_owned(),
    }
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
/// - 非ゼロ終了した場合は [`Error::GitRunFailed`]（失敗の詳細は git が端末へ直接出力済み）
pub fn run_git(args: &[&str]) -> Result<()> {
    log_command(None, args);

    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|source| map_spawn_error(source, args))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::GitRunFailed {
            command: command_display(args),
            code: status.code(),
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

/// `git` を `directory` で実行し、終了コードと標準出力を返す。
///
/// [`capture_git_in`] と違い、非ゼロ終了をエラーにしない。`git merge-tree --write-tree` の
/// ように「終了コード 1 ＝コンフリクトあり」を正常系として扱うコマンドや、
/// `git rev-parse --verify --quiet` のように「非ゼロ終了＝解決できなかった」を
/// 結果として受け取りたいコマンドのために用意する。
///
/// 標準エラーは呼び出し側へ返さない。このヘルパを使うのは終了コード自体が意味を持つ
/// コマンドに限られ、失敗理由の提示より終了コードによる分岐が目的であるため。
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - シグナルで終了させられ終了コードが得られない場合は [`Error::GitCommandFailed`]
pub fn capture_git_with_status_in(directory: &Path, args: &[&str]) -> Result<(i32, Vec<u8>)> {
    log_command(Some(directory), args);

    let output = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .current_dir(directory)
        .output()
        .map_err(|source| map_spawn_error(source, args))?;

    // シグナルによる終了では終了コードが得られない。終了コードで分岐する用途のヘルパである以上、
    // 判断できないまま続行せず失敗として扱う
    let Some(code) = output.status.code() else {
        return Err(Error::GitCommandFailed {
            args: display_args(args),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    };

    Ok((code, output.stdout))
}

/// `git merge-tree --write-tree` の終了コードが表す結果。
///
/// merge のドライランは「コンフリクトあり」を非ゼロ終了（1）で伝えるため、
/// 終了コードを一律にエラーとして扱えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeTreeOutcome {
    /// コンフリクトなくマージできる。
    Clean,
    /// コンフリクトが発生する。
    Conflicted,
    /// マージ判定そのものが失敗した（Git 2.38 未満・不正なリビジョンなど）。
    ///
    /// 予測は補助情報であり、この場合は予測表示を省略して主要動作を続行する。
    Failed,
}

impl MergeTreeOutcome {
    /// `git merge-tree --write-tree` の終了コードから結果を判定する。
    ///
    /// git の仕様は 0 ＝クリーン / 1 ＝コンフリクトあり / それ以外＝エラー。
    /// 負の値（終了コードとしては現れない）も判断できない値としてエラー側へ倒す。
    #[must_use]
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            0 => MergeTreeOutcome::Clean,
            1 => MergeTreeOutcome::Conflicted,
            _ => MergeTreeOutcome::Failed,
        }
    }
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
    fn run_git_reports_only_the_subcommand_on_failure() {
        // 未知のサブコマンドは何も変更せずに非ゼロ終了する（git 自身の説明は端末へ出る）
        let err = run_git(&["fuzgit-no-such-subcommand", "--", ":(top,literal)a[1].txt"])
            .expect_err("unknown subcommand must fail");

        match err {
            Error::GitRunFailed { command, code } => {
                assert_eq!(command, "git fuzgit-no-such-subcommand");
                assert_eq!(code, Some(1));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_command_display_stops_at_the_subcommand() {
        // pathspec の列を再掲すると、端末に出ている git 本来のメッセージが埋もれる
        assert_eq!(
            command_display(&["commit", "--", ":(top,literal)src/cli.rs"]),
            "git commit"
        );
    }

    #[test]
    fn a_command_display_omits_options_and_operands() {
        assert_eq!(
            command_display(&["push", "--set-upstream", "origin"]),
            "git push"
        );
        assert_eq!(command_display(&["stash", "push", "--"]), "git stash");
    }

    #[test]
    fn a_command_display_without_arguments_names_git_itself() {
        assert_eq!(command_display(&[]), "git");
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
    fn capture_git_with_status_in_returns_the_output_of_a_successful_command() {
        use crate::test_support::{TempDir, commit, init_repository};

        let dir = TempDir::new("exec-status-ok");
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");

        let (code, stdout) = capture_git_with_status_in(dir.path(), &["rev-parse", "HEAD"])
            .expect("rev-parse should run");

        assert_eq!(code, 0);
        let text = String::from_utf8(stdout).expect("rev-parse emits utf-8");
        assert_eq!(text.trim(), head);
    }

    #[test]
    fn capture_git_with_status_in_reports_a_non_zero_exit_without_failing() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-status-non-zero");
        init_repository(dir.path());

        // `--verify --quiet` は解決できない参照に対しメッセージ無しで非ゼロ終了する
        let (code, stdout) = capture_git_with_status_in(
            dir.path(),
            &["rev-parse", "--verify", "--quiet", "refs/heads/missing"],
        )
        .expect("a non-zero exit must not be an error");

        assert_ne!(code, 0, "a missing reference must not exit with 0");
        assert!(stdout.is_empty(), "unexpected output: {stdout:?}");
    }

    #[test]
    fn a_clean_merge_tree_exit_code_means_no_conflict() {
        assert_eq!(MergeTreeOutcome::from_exit_code(0), MergeTreeOutcome::Clean);
    }

    #[test]
    fn a_merge_tree_exit_code_of_one_means_conflicts() {
        assert_eq!(
            MergeTreeOutcome::from_exit_code(1),
            MergeTreeOutcome::Conflicted
        );
    }

    #[test]
    fn any_other_merge_tree_exit_code_means_the_prediction_failed() {
        // 2 以上は git 側のエラー（Git 2.38 未満の未知オプション 129、fatal の 128 を含む）
        for code in [2, 3, 42, 128, 129, i32::MAX] {
            assert_eq!(
                MergeTreeOutcome::from_exit_code(code),
                MergeTreeOutcome::Failed,
                "exit code {code} must be treated as a failure"
            );
        }
    }

    #[test]
    fn a_negative_merge_tree_exit_code_means_the_prediction_failed() {
        assert_eq!(
            MergeTreeOutcome::from_exit_code(-1),
            MergeTreeOutcome::Failed
        );
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
