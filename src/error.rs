//! fuzgit のドメインエラー定義。
//!
//! ライブラリ層（`git` / `finder`）はここで定義した [`Error`] を返し、
//! アプリ層（`commands` / `main`）は `anyhow` で集約する。

use thiserror::Error;

/// fuzgit のライブラリ層で発生するエラー。
#[derive(Debug, Error)]
pub enum Error {
    /// カレントディレクトリ（および親ディレクトリ）に git リポジトリが見つからない。
    #[error("git リポジトリではありません。git リポジトリ内で実行してください")]
    NotARepository {
        /// `gix` の探索エラー。
        ///
        /// `gix::discover::Error` は列挙子が多く実体が大きいため、
        /// `Error` 全体が肥大化しないよう Box 化して保持する。
        #[source]
        source: Box<gix::discover::Error>,
    },

    /// `gix` によるリポジトリ情報の読み取りに失敗した。
    #[error("リポジトリ情報の読み取りに失敗しました（{operation}）")]
    RepositoryReadFailed {
        /// 失敗した読み取り操作の説明（例: `ローカルブランチの列挙`）。
        operation: String,
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
    #[error("git コマンドが失敗しました: git {args}{}", stderr_suffix(.stderr))]
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
    #[error("{command} が{}", exit_status_text(.code))]
    GitRunFailed {
        /// 表示用のコマンド名（例: `git commit`）。
        command: String,
        /// プロセスの終了コード。シグナルで終了した場合は `None`。
        code: Option<i32>,
    },

    /// `git` 実行ファイルが PATH 上に存在しない。
    #[error("git コマンドが見つかりません。git をインストールして PATH を通してください")]
    GitNotFound,

    /// `git` の起動そのものに失敗した（権限不足など、実行ファイル不在以外の理由）。
    #[error("git コマンドの起動に失敗しました: git {args}")]
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
    #[error(
        "現在のブランチ `{branch}` にはまだコミットがありません。\
         `git commit` で最初のコミットを作成してから実行してください"
    )]
    UnbornHead {
        /// HEAD が指している（まだ実体の無い）ブランチ名。
        branch: String,
    },

    /// HEAD がブランチではなくコミットを直接指している（detached HEAD）。
    ///
    /// `gz push` は「現在のブランチ」を push 対象として固定するため、
    /// ブランチが定まらない状態では候補を作れない。
    #[error(
        "HEAD がブランチを指していません（detached HEAD）。\
         push するブランチが定まらないため、`git switch <ブランチ>` で切り替えてから実行してください"
    )]
    DetachedHead,

    /// 作業ツリーを持たない（bare）リポジトリで、作業ツリーを前提とする操作を行おうとした。
    #[error("作業ツリーがありません。bare リポジトリでは実行できない操作です")]
    NoWorktree,

    /// ユーザーが fuzzy finder を中断した（Esc / Ctrl-C）。
    #[error("選択が中断されました")]
    Cancelled,

    /// fuzzy finder に渡す候補が 1 件も無い。
    #[error("選択できる候補がありません")]
    NoCandidates,

    /// fuzzy finder の初期化・実行に失敗した。
    ///
    /// skim は `eyre::Report` を返すが `std::error::Error` を実装しないため、
    /// メッセージへ変換して保持する。
    #[error("fuzzy finder の実行に失敗しました: {message}")]
    FinderFailed {
        /// skim から得られたエラーメッセージ。
        message: String,
    },
}

/// `git` の標準エラー出力をエラーメッセージ末尾へ追記するための整形を行う。
///
/// 継承 stdio で実行した場合は stderr を保持できず空文字列になるため、
/// その場合に空行だけが残らないようにする。
fn stderr_suffix(stderr: &str) -> String {
    let trimmed = stderr.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("\n{trimmed}")
    }
}

/// プロセスの終了状況をエラーメッセージ末尾の述部として整形する。
///
/// シグナルで終了した場合は終了コードが得られないため、コードを取り繕わず理由の方を示す。
fn exit_status_text(code: &Option<i32>) -> String {
    match code {
        Some(code) => format!("終了コード {code} で終了しました"),
        None => "シグナルにより終了しました".to_owned(),
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

    #[test]
    fn git_command_failed_appends_stderr() {
        let err = Error::GitCommandFailed {
            args: "switch nope".to_string(),
            stderr: "fatal: invalid reference: nope\n".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "git コマンドが失敗しました: git switch nope\nfatal: invalid reference: nope"
        );
    }

    #[test]
    fn git_command_failed_without_stderr_has_no_trailing_newline() {
        let err = Error::GitCommandFailed {
            args: "switch nope".to_string(),
            stderr: String::new(),
        };
        assert_eq!(
            err.to_string(),
            "git コマンドが失敗しました: git switch nope"
        );
    }

    #[test]
    fn git_run_failed_names_the_command_and_the_exit_code() {
        let err = Error::GitRunFailed {
            command: "git commit".to_string(),
            code: Some(1),
        };
        assert_eq!(err.to_string(), "git commit が終了コード 1 で終了しました");
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
        assert_eq!(err.to_string(), "git push がシグナルにより終了しました");
    }

    #[test]
    fn stderr_suffix_ignores_whitespace_only_input() {
        assert_eq!(stderr_suffix("  \n\n"), "");
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
