//! fuzgit のドメインエラー定義。
//!
//! ライブラリ層（`git` / `finder`）はここで定義した [`Error`] を返し、
//! アプリ層（`commands` / `main`）は `anyhow` で集約する。
//!
//! # 表示の責務は [`Error`] にない（FR-27）
//!
//! `#[error("…")]` が与える `Display` は **英語の開発者向け表示**であり、
//! ユーザーへ見せる文言ではない。`std::error::Error` の `Display` は `anyhow` の
//! `#[source]` 連鎖と `Debug` 出力に必要である一方、そこへ表示言語の状態を持ち込めないため
//! （design.md「`error.rs` の日本語をどうするか」）。フォールバック言語が `en` である以上、
//! ここが英語であることは規定動作と整合する。
//!
//! ユーザー向けの表示は [`crate::i18n::messages::ErrorMessages::describe`] が担う。

use thiserror::Error;

use crate::git::read::{MalformedOutput, ReadOperation};
use crate::git::siblings::FilesystemOperation;

/// fuzgit のライブラリ層で発生するエラー。
///
/// バリアントを追加すると [`crate::i18n::ja`] / [`crate::i18n::en`] の網羅的な `match` が
/// 双方ともコンパイルエラーになる。翻訳漏れを実行時ではなくコンパイル時に検出するための
/// 設計であり、`describe` 側にワイルドカードの腕を足してはならない。
#[derive(Debug, Error)]
pub enum Error {
    /// カレントディレクトリ（および親ディレクトリ）に git リポジトリが見つからない。
    #[error("not a git repository")]
    NotARepository {
        /// `gix` の探索エラー。
        ///
        /// `gix::discover::Error` は列挙子が多く実体が大きいため、
        /// `Error` 全体が肥大化しないよう Box 化して保持する。
        #[source]
        source: Box<gix::discover::Error>,
    },

    /// `gix` によるリポジトリ情報の読み取りに失敗した。
    ///
    /// `{operation:?}` として `Debug` を用いるのは、[`ReadOperation`] に
    /// **言語に依存しない `Display` が存在しないから**である。ユーザー向けの表示は
    /// [`crate::i18n::messages::ErrorMessages::describe`] が言語ごとに組み立てる。
    #[error("failed to read repository information ({operation:?})")]
    RepositoryReadFailed {
        /// 失敗した読み取り操作。
        operation: ReadOperation,
        /// `gix` 側のエラー。
        ///
        /// `gix` の読み取り API はモジュールごとに異なるエラー型を返すため、
        /// それらを列挙せずに扱えるようトレイトオブジェクトとして保持する。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// 出力をキャプチャして実行した `git` コマンドが非ゼロ終了した。
    ///
    /// この経路では git のメッセージがユーザーの端末に出ていないため、
    /// 実行した引数とキャプチャした標準エラーを添えて原因を伝える。
    #[error("git command failed: git {args}{}", stderr_suffix(.stderr))]
    GitCommandFailed {
        /// 実行した引数（表示用にスペース区切りで連結したもの）。
        args: String,
        /// キャプチャできた標準エラー出力。git が何も出力しなかった場合は空文字列になる。
        stderr: String,
    },

    /// 標準入出力を継承して実行した `git` コマンドが非ゼロ終了した。
    ///
    /// git 自身のエラーメッセージは既にユーザーの端末へ出力済みであるため、
    /// 引数（パススペックの列を含む）を再掲すると本当の原因がその中に埋もれる。
    /// ここではコマンド名と終了状況だけを示し、詳細は git の出力に委ねる。
    #[error("{command} {}", exit_status_text(.code))]
    GitRunFailed {
        /// 表示用のコマンド名（例: `git commit`）。
        command: String,
        /// プロセスの終了コード。シグナルで終了した場合は `None`。
        code: Option<i32>,
    },

    /// `git` 実行ファイルが PATH 上に存在しない。
    #[error("git command not found")]
    GitNotFound,

    /// `git` の起動そのものに失敗した（権限不足など、実行ファイル不在以外の理由）。
    #[error("failed to start the git command: git {args}")]
    GitSpawnFailed {
        /// 実行しようとした引数（表示用にスペース区切りで連結したもの）。
        args: String,
        /// プロセス起動時の I/O エラー。
        #[source]
        source: std::io::Error,
    },

    /// HEAD が指すブランチにまだコミットが 1 件も存在しない（unborn HEAD）。
    ///
    /// 候補が 0 件になる原因のうち、`git init` 直後のように「まだコミットが無い」場合は
    /// [`Error::NoCandidates`] と区別して原因と次の操作を伝える。
    #[error("the current branch `{branch}` has no commits yet")]
    UnbornHead {
        /// HEAD が指している（まだ実体の無い）ブランチ名。
        branch: String,
    },

    /// 作業ツリーを持たない（bare）リポジトリで、作業ツリーを前提とする操作を行おうとした。
    #[error("no worktree: this operation is not available in a bare repository")]
    NoWorktree,

    /// 兄弟リポジトリの走査範囲（ワークツリー root の親ディレクトリ）を決められない。
    ///
    /// リポジトリがファイルシステムの root 直下にある場合に起こる。走査範囲が定まらない状態で
    /// 暗黙に現在のリポジトリだけへ倒すと、指定した `--siblings` が黙って無視されるため停止する。
    #[error(
        "cannot determine the scope for sibling repositories: \
         the worktree root `{}` has no parent directory",
        .workdir.display()
    )]
    NoSiblingScope {
        /// 親ディレクトリを取得できなかったワークツリーのルート（正規化済み）。
        workdir: std::path::PathBuf,
    },

    /// リポジトリ探索に伴うファイルシステムの読み取りに失敗した。
    ///
    /// `{operation:?}` を用いる理由は [`Error::RepositoryReadFailed`] と同じ。
    #[error("filesystem read failed ({operation:?}): {}", .path.display())]
    FilesystemReadFailed {
        /// 失敗した操作。
        operation: FilesystemOperation,
        /// 操作の対象パス。
        path: std::path::PathBuf,
        /// 標準ライブラリ側の I/O エラー。
        #[source]
        source: std::io::Error,
    },

    /// `git` の出力が想定した形式と異なる。
    ///
    /// [`Error::RepositoryReadFailed`] と分けているのは、**原因となる `source` が存在しない**
    /// ため。食い違いを見つけたのは外部のライブラリではなく fuzgit 自身であり、
    /// 何が想定と違ったかは [`MalformedOutput`] が値として保持する
    /// （表示済みの文字列を `source` へ詰めると、その分だけ表示言語を選べなくなる）。
    #[error("malformed git output ({operation:?}): {detail:?}")]
    GitOutputMalformed {
        /// 出力を読もうとしていた操作。
        operation: ReadOperation,
        /// 想定と食い違った内容。
        detail: MalformedOutput,
    },

    /// `git config fuzgit.fetchJobs` に、同時実行数として使えない値が設定されている。
    ///
    /// 0・負数・整数でない値がこれに当たる。既定値へ黙って倒さないのは、利用者が
    /// 指定したつもりの同時実行数と実際の動作が食い違ったまま通信が始まるためである
    /// （`fuzgit.lang` の明示指定が不正値で停止するのと同じ扱い。暗黙のフォールバック禁止）。
    ///
    /// 保持するのは**読み取った値そのもの**であり、表示済みの文へは組み立てない。
    /// 文の組み立ては [`crate::i18n::messages::ErrorMessages::describe`] が言語ごとに行う。
    #[error("invalid value for `fuzgit.fetchJobs`: `{value}` (expected an integer of 1 or more)")]
    InvalidFetchJobs {
        /// 設定から読み取った値。UTF-8 でない値はロッシー変換された文字列になる。
        value: String,
    },

    /// ユーザーが fuzzy finder を中断した（Esc / Ctrl-C）。
    #[error("the selection was cancelled")]
    Cancelled,

    /// fuzzy finder に渡す候補が 1 件も無い。
    #[error("no candidates to select from")]
    NoCandidates,

    /// fuzzy finder の初期化・実行に失敗した。
    ///
    /// skim は `eyre::Report` を返すが `std::error::Error` を実装しないため、
    /// メッセージへ変換して保持する。
    #[error("the fuzzy finder failed: {message}")]
    FinderFailed {
        /// skim から得られたエラーメッセージ。
        message: String,
    },
}

/// `git` の標準エラー出力をエラーメッセージ末尾へ追記するための整形を行う。
///
/// 継承 stdio で実行した場合は stderr を保持できず空文字列になるため、
/// その場合に空行だけが残らないようにする。
///
/// 整形するのは改行と空白だけであり言語に依存しないため、`Display`（英語）と
/// [`crate::i18n::messages::ErrorMessages::describe`]（ja / en）の双方から共有する。
pub(crate) fn stderr_suffix(stderr: &str) -> String {
    let trimmed = stderr.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n{trimmed}")
    }
}

/// プロセスの終了状況を `Display`（英語）の述部として整形する。
///
/// シグナルで終了した場合は終了コードが得られないため、コードを取り繕わず理由の方を示す。
/// ユーザー向けの同等の表現は各言語の `describe` が持つ。
fn exit_status_text(code: &Option<i32>) -> String {
    match code {
        Some(code) => format!("exited with code {code}"),
        None => "was terminated by a signal".to_owned(),
    }
}

/// fuzgit のライブラリ層で用いる `Result` 別名。
pub type Result<T> = std::result::Result<T, Error>;

/// エラー連鎖のいずれかが [`Error::Cancelled`] かどうかを判定する。
///
/// `anyhow::Context` で文脈を付与された場合でも判定できるよう、連鎖全体を走査する。
#[must_use]
pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| matches!(cause.downcast_ref::<Error>(), Some(Error::Cancelled)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // `Display` はユーザー向けの表示ではなく英語の開発者向け表示であるため、
    // 期待値も英語で固定する（ユーザー向けの文言は `i18n::ja` / `i18n::en` のテストで検証する）。

    #[test]
    fn git_command_failed_appends_stderr() {
        let err = Error::GitCommandFailed {
            args: "switch nope".to_string(),
            stderr: "fatal: invalid reference: nope\n".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "git command failed: git switch nope\nfatal: invalid reference: nope"
        );
    }

    #[test]
    fn git_command_failed_without_stderr_has_no_trailing_newline() {
        let err = Error::GitCommandFailed {
            args: "switch nope".to_string(),
            stderr: String::new(),
        };
        assert_eq!(err.to_string(), "git command failed: git switch nope");
    }

    #[test]
    fn git_run_failed_names_the_command_and_the_exit_code() {
        let err = Error::GitRunFailed {
            command: "git commit".to_string(),
            code: Some(1),
        };
        assert_eq!(err.to_string(), "git commit exited with code 1");
    }

    #[test]
    fn git_run_failed_does_not_repeat_the_arguments() {
        // git 自身のメッセージは端末へ出力済みであり、pathspec の列を再掲すると原因が埋もれる
        let err = Error::GitRunFailed {
            command: "git commit".to_string(),
            code: Some(1),
        };
        assert!(
            !err.to_string().contains(":(top,literal)"),
            "the message must not repeat the command line: {err}"
        );
    }

    #[test]
    fn git_run_failed_reports_a_signal_termination_without_a_code() {
        let err = Error::GitRunFailed {
            command: "git push".to_string(),
            code: None,
        };
        assert_eq!(err.to_string(), "git push was terminated by a signal");
    }

    #[test]
    fn an_invalid_fetch_jobs_value_is_shown_verbatim() {
        // 何を直せばよいか分かるよう、読み取った値と期待する形の両方を示す
        let err = Error::InvalidFetchJobs {
            value: "0".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid value for `fuzgit.fetchJobs`: `0` (expected an integer of 1 or more)"
        );
    }

    #[test]
    fn stderr_suffix_ignores_whitespace_only_input() {
        assert_eq!(stderr_suffix("  \n\n"), "");
    }

    #[test]
    fn display_is_english_for_every_variant_that_takes_no_source() {
        // 表示言語を持ち込めない `Display` が英語で固定されていること（日本語が残っていないこと）を
        // 代表的なバリアントで確認する。ユーザー向けの日本語は `describe` が担う
        for error in [
            Error::GitNotFound,
            Error::NoWorktree,
            Error::Cancelled,
            Error::NoCandidates,
            Error::UnbornHead {
                branch: "main".to_string(),
            },
        ] {
            let message = error.to_string();
            assert!(
                message.is_ascii(),
                "the developer-facing Display must stay in English: {message}"
            );
        }
    }

    #[test]
    fn is_cancelled_detects_a_directly_returned_cancellation() {
        assert!(is_cancelled(&anyhow::Error::from(Error::Cancelled)));
    }

    #[test]
    fn is_cancelled_detects_a_cancellation_wrapped_in_context() {
        use anyhow::Context as _;

        let error = Err::<(), _>(Error::Cancelled)
            .context("ブランチの選択中")
            .expect_err("must be an error");

        assert!(is_cancelled(&error));
    }

    #[test]
    fn is_cancelled_is_false_for_other_errors() {
        assert!(!is_cancelled(&anyhow::Error::from(Error::NoCandidates)));
        assert!(!is_cancelled(&anyhow::anyhow!("未実装です")));
    }
}
