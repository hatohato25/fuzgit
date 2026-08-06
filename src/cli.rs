//! `clap` derive によるコマンドライン定義。

use clap::{Parser, Subcommand};

/// コミット候補の既定取得件数。
///
/// コミット数の多いリポジトリでも初期表示の応答性を確保するための上限で、
/// `gz log --limit` の既定値と `gz cherry-pick` の候補数上限に用いる。
pub const DEFAULT_COMMIT_LIMIT: usize = 1000;

/// fuzzy finder で「選ぶ」「探す」「辿る」git 操作 CLI。
#[derive(Debug, Parser)]
#[command(
    name = "gz",
    version,
    about = "fuzzy finder で選んで操作する git CLI",
    arg_required_else_help = true
)]
pub struct Cli {
    /// 実行するサブコマンド。
    #[command(subcommand)]
    pub command: Command,
}

/// `gz` のサブコマンド。
#[derive(Debug, Subcommand)]
pub enum Command {
    /// ブランチを選択して切り替える
    Branch {
        /// リモート追跡ブランチも候補に含める
        #[arg(short, long)]
        all: bool,
    },

    /// コミット履歴を辿り、選択したコミットのフルハッシュを標準出力へ出す
    Log {
        /// 取得するコミットの最大件数
        #[arg(short = 'n', long, value_name = "N", default_value_t = DEFAULT_COMMIT_LIMIT)]
        limit: usize,
    },

    /// コミットを選択して cherry-pick する
    CherryPick {
        /// 対象ブランチ（未指定時は全ブランチのコミットを候補にする）
        #[arg(short, long, value_name = "BRANCH")]
        branch: Option<String>,
    },

    /// ファイルを選択して git restore する
    Restore {
        /// 復元元のリビジョン
        #[arg(short, long, value_name = "REV")]
        source: Option<String>,

        /// ステージ済みの変更をアンステージする
        #[arg(long)]
        staged: bool,
    },

    /// 未ステージ・未追跡ファイルを選択して git add する
    Add,

    /// stash を検索して適用する
    Stash {
        /// apply ではなく pop する
        #[arg(long, conflicts_with = "drop")]
        pop: bool,

        /// 選択した stash を破棄する
        #[arg(long)]
        drop: bool,
    },

    /// タグを選択する（既定はタグ名を標準出力へ出す）
    Tag {
        /// 選択したタグへ detached HEAD で切り替える
        #[arg(long, conflicts_with = "diff")]
        switch: bool,

        /// 選択したタグと HEAD の差分を表示する
        #[arg(long)]
        diff: bool,
    },

    /// HEAD の reflog を辿り、選択したコミットのハッシュを標準出力へ出す
    Reflog {
        /// 選択したコミットから指定名の新規ブランチを作成する
        #[arg(long, value_name = "NAME")]
        restore: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn cherry_pick_is_exposed_as_kebab_case() {
        let cli = Cli::try_parse_from(["gz", "cherry-pick", "--branch", "feature"])
            .expect("cherry-pick should parse");

        match cli.command {
            Command::CherryPick { branch } => assert_eq!(branch.as_deref(), Some("feature")),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn log_limit_defaults_to_the_documented_value() {
        let cli = Cli::try_parse_from(["gz", "log"]).expect("log should parse without options");

        match cli.command {
            Command::Log { limit } => assert_eq!(limit, DEFAULT_COMMIT_LIMIT),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn stash_pop_and_drop_are_mutually_exclusive() {
        Cli::try_parse_from(["gz", "stash", "--pop", "--drop"])
            .expect_err("--pop and --drop must not be combined");
    }

    #[test]
    fn no_arguments_is_rejected_so_that_help_is_shown() {
        let err = Cli::try_parse_from(["gz"]).expect_err("bare `gz` must show help");

        assert!(
            err.to_string().contains("branch"),
            "help should list subcommands: {err}"
        );
    }
}
