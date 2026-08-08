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

    /// 変更を stash へ退避し、stash を検索して適用・破棄する
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Stash {
        /// 実行する stash の操作。
        #[command(subcommand)]
        command: StashCommand,
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

/// `gz stash` のサブコマンド。
///
/// `push` が選ぶのは作業ツリーの「ファイル」で、`apply` / `pop` / `drop` が選ぶのは既存の「stash」。
/// 選択対象そのものが異なるため、オプションではなくサブコマンドで分ける。
/// 引数なしの `gz stash` はヘルプを表示する（git 慣習の `push` と従来の `apply` が食い違うため、
/// 暗黙にどちらかへ倒さない）。
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum StashCommand {
    /// 変更ファイルを選択して stash へ退避する
    Push {
        /// stash に付けるメッセージ
        #[arg(short, long, value_name = "MESSAGE")]
        message: Option<String>,

        /// 未追跡ファイルも候補に含める（既定は追跡済みの変更のみ）
        #[arg(short = 'u', long)]
        include_untracked: bool,
    },

    /// stash を選択して適用する（stash は残す）
    Apply,

    /// stash を選択して適用し、その stash を取り除く
    Pop,

    /// stash を選択して破棄する
    Drop,
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
    fn stash_requires_a_subcommand_so_that_help_is_shown() {
        let err = Cli::try_parse_from(["gz", "stash"]).expect_err("bare `gz stash` must show help");

        let message = err.to_string();
        for subcommand in ["push", "apply", "pop", "drop"] {
            assert!(
                message.contains(subcommand),
                "help should list `{subcommand}`: {message}"
            );
        }
    }

    #[test]
    fn stash_push_takes_a_message_and_the_untracked_flag() {
        let cli = Cli::try_parse_from(["gz", "stash", "push", "-m", "作業中", "-u"])
            .expect("stash push should parse");

        match cli.command {
            Command::Stash {
                command:
                    StashCommand::Push {
                        message,
                        include_untracked,
                    },
            } => {
                assert_eq!(message.as_deref(), Some("作業中"));
                assert!(include_untracked);
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn stash_push_defaults_to_tracked_changes_without_a_message() {
        let cli =
            Cli::try_parse_from(["gz", "stash", "push"]).expect("stash push should parse bare");

        match cli.command {
            Command::Stash {
                command:
                    StashCommand::Push {
                        message,
                        include_untracked,
                    },
            } => {
                assert_eq!(message, None);
                assert!(
                    !include_untracked,
                    "untracked files must be opt-in: they cannot be stashed without `-u`"
                );
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn stash_exposes_apply_pop_and_drop_as_subcommands() {
        for (argument, expected) in [
            ("apply", StashCommand::Apply),
            ("pop", StashCommand::Pop),
            ("drop", StashCommand::Drop),
        ] {
            let cli = Cli::try_parse_from(["gz", "stash", argument])
                .unwrap_or_else(|err| panic!("`gz stash {argument}` should parse: {err}"));

            match cli.command {
                Command::Stash { command } => assert_eq!(command, expected),
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }
    }

    #[test]
    fn the_replaced_stash_flags_are_no_longer_accepted() {
        for flag in ["--pop", "--drop"] {
            assert!(
                Cli::try_parse_from(["gz", "stash", flag]).is_err(),
                "`gz stash {flag}` must be rejected after the move to subcommands"
            );
        }
    }

    #[test]
    fn tag_switch_and_diff_are_mutually_exclusive() {
        Cli::try_parse_from(["gz", "tag", "--switch", "--diff"])
            .expect_err("--switch and --diff must not be combined");
    }

    #[test]
    fn reflog_restore_takes_the_new_branch_name() {
        let cli = Cli::try_parse_from(["gz", "reflog", "--restore", "recovered"])
            .expect("reflog should parse");

        match cli.command {
            Command::Reflog { restore } => assert_eq!(restore.as_deref(), Some("recovered")),
            other => panic!("unexpected subcommand: {other:?}"),
        }
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
