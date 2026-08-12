//! `clap` derive によるコマンドライン定義。

use std::path::PathBuf;

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
    /// ブランチを選択して切り替える（サブコマンドで作成・削除・整理も行う）
    ///
    /// 引数なしの `gz branch` と `gz branch --all` は従来どおりの切替（FR-1）。
    /// `args_conflicts_with_subcommands` により、切替用のフラグと管理サブコマンドの
    /// 併用は clap の段階で拒否される（どちらの操作なのかが曖昧にならないようにするため）。
    #[command(args_conflicts_with_subcommands = true)]
    Branch {
        /// リモート追跡ブランチも候補に含める
        #[arg(short, long)]
        all: bool,

        /// 実行するブランチ管理の操作（省略時はブランチの切替）
        #[command(subcommand)]
        command: Option<BranchCommand>,
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

    /// コミットするファイルを選択してコミットする
    Commit {
        /// コミットメッセージ（省略時は git がエディタを起動する）
        #[arg(short, long, value_name = "MESSAGE")]
        message: Option<String>,
    },

    /// push 先（リモート × 現在ブランチ）を選択して push する
    ///
    /// force push（`--force` / `--force-with-lease`）は提供しない。
    Push {
        /// push 先を現在ブランチの upstream として設定する
        #[arg(short = 'u', long)]
        set_upstream: bool,
    },

    /// 修正対象のコミットを選択して fixup コミットを作成する
    Fixup {
        /// fixup ではなく squash コミット（メッセージを結合する）を作成する
        #[arg(long)]
        squash: bool,
    },

    /// ブランチを選択して merge する
    Merge {
        /// fast-forward できる場合でもマージコミットを作成する
        #[arg(long, conflicts_with_all = ["squash", "ff_only"])]
        no_ff: bool,

        /// マージ結果を作業ツリー・index へ反映するだけでコミットしない
        #[arg(long, conflicts_with_all = ["no_ff", "ff_only"])]
        squash: bool,

        /// fast-forward できる場合のみ merge する
        #[arg(long, conflicts_with_all = ["no_ff", "squash"])]
        ff_only: bool,
    },

    /// ブランチを選択して rebase する
    Rebase,

    /// コミットを選択して打ち消す（revert コミットを作成する）
    Revert {
        /// エディタを起動せず、git の既定メッセージのままコミットする
        #[arg(long)]
        no_edit: bool,
    },

    /// 変更ファイルの状態を一覧し、選択したファイルに操作を行う
    Status,

    /// 比較対象を選択して差分を表示する
    ///
    /// 引数なしは未ステージの変更（`git diff` と同じ）。比較モードは相互排他。
    Diff {
        /// ステージ済みの変更を対象にする（`git diff --staged` と同じ）
        #[arg(long, conflicts_with_all = ["head", "upstream", "branch", "commit"])]
        staged: bool,

        /// HEAD と作業ツリーを比較する（ステージ済みの変更を含む）
        #[arg(long, conflicts_with_all = ["staged", "upstream", "branch", "commit"])]
        head: bool,

        /// HEAD と upstream を比較する
        #[arg(long, conflicts_with_all = ["staged", "head", "branch", "commit"])]
        upstream: bool,

        /// ブランチを 2 回選択して比較する
        #[arg(long, conflicts_with_all = ["staged", "head", "upstream", "commit"])]
        branch: bool,

        /// コミットを 2 回選択して比較する
        #[arg(long, conflicts_with_all = ["staged", "head", "upstream", "branch"])]
        commit: bool,
    },

    /// fetch の対象を決めて取得する
    ///
    /// リモートが 1 つだけの場合は選択の余地が無いため、finder を起動せず即座に取得する。
    Fetch {
        /// リモートで削除されたブランチの追跡参照も掃除する
        #[arg(long)]
        prune: bool,

        /// 同じ階層に並ぶリポジトリも対象に含めて一括で取得する
        #[arg(long, short)]
        siblings: bool,
    },

    /// ブランチを選んで upstream へ追随させる（fast-forward のみ）
    ///
    /// 取り込み方式は fast-forward に固定し、`--rebase` / `--merge` は提供しない
    /// （方式を選んで 1 本だけ同期するのは `gz sync`）。対象は選択で決めるため
    /// 位置引数を取らず、`--siblings` / `--prune` も持たない。
    Pull,

    /// 現在のブランチを upstream と同期する
    ///
    /// 既定は fast-forward のみ（`--ff-only` 相当）。fast-forward できない場合は
    /// git のエラーをそのまま表示して停止し、暗黙に merge / rebase へ倒さない。
    Sync {
        /// upstream の上へ rebase して取り込む（履歴改変）
        #[arg(long, conflicts_with_all = ["merge"])]
        rebase: bool,

        /// upstream を merge して取り込む
        #[arg(long, conflicts_with_all = ["rebase"])]
        merge: bool,
    },

    /// worktree を一覧・管理する（引数なしは一覧からパスを標準出力へ出す）
    Worktree {
        /// 実行する worktree の操作（省略時は一覧からの選択）
        #[command(subcommand)]
        command: Option<WorktreeCommand>,
    },
}

/// `gz branch` のサブコマンド（FR-20）。
///
/// サブコマンドを省略した `gz branch` は従来どおりブランチの切替（FR-1）であり、
/// ここに並ぶのは切替以外のブランチ管理操作。既定の動作が確立している点が
/// [`StashCommand`]（既定を決められないためサブコマンド必須）との違い。
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum BranchCommand {
    /// 作成元を選択して新しいブランチを作成する
    Create {
        /// 作成するブランチ名
        name: String,

        /// 作成後にそのブランチへ切り替える
        #[arg(long)]
        switch: bool,
    },

    /// ブランチを選択して削除する
    Delete {
        /// merged でないブランチも削除する（`git branch -D`）
        #[arg(long)]
        force: bool,

        /// merged 判定の基準ブランチ（既定は HEAD）
        #[arg(long, value_name = "BRANCH")]
        into: Option<String>,
    },

    /// merged なブランチを一括で削除する
    Cleanup {
        /// merged 判定の基準ブランチ（既定は HEAD）
        #[arg(long, value_name = "BRANCH")]
        into: Option<String>,
    },
}

/// `gz worktree` のサブコマンド（FR-21）。
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum WorktreeCommand {
    /// ブランチを選択して新しい worktree を作成する
    Add {
        /// 作成する worktree のパス（ディレクトリ名の自動提案は行わない）
        path: PathBuf,
    },

    /// worktree を選択して削除する（main worktree は候補に含めない）
    Remove,

    /// 実体を失った worktree の管理情報を整理する
    Prune,
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
    fn commit_takes_an_optional_message() {
        let cli = Cli::try_parse_from(["gz", "commit", "-m", "作業を保存"])
            .expect("commit should parse with a message");
        match cli.command {
            Command::Commit { message } => assert_eq!(message.as_deref(), Some("作業を保存")),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "commit"]).expect("commit should parse bare");
        match cli.command {
            // メッセージ省略時は git がエディタを起動する
            Command::Commit { message } => assert_eq!(message, None),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn push_takes_the_set_upstream_flag() {
        for arguments in [["gz", "push", "-u"], ["gz", "push", "--set-upstream"]] {
            let cli = Cli::try_parse_from(arguments).expect("push should parse");
            match cli.command {
                Command::Push { set_upstream } => assert!(set_upstream),
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }

        let cli = Cli::try_parse_from(["gz", "push"]).expect("push should parse bare");
        match cli.command {
            Command::Push { set_upstream } => assert!(!set_upstream),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn push_does_not_offer_force_options() {
        // force push は fuzgit のスコープ外（requirements.md「スコープ外」）
        for flag in ["--force", "--force-with-lease", "-f"] {
            assert!(
                Cli::try_parse_from(["gz", "push", flag]).is_err(),
                "`gz push {flag}` must be rejected"
            );
        }
    }

    #[test]
    fn fixup_takes_the_squash_flag() {
        let cli = Cli::try_parse_from(["gz", "fixup", "--squash"]).expect("fixup should parse");
        match cli.command {
            Command::Fixup { squash } => assert!(squash),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "fixup"]).expect("fixup should parse bare");
        match cli.command {
            Command::Fixup { squash } => assert!(!squash),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn merge_accepts_each_strategy_flag_on_its_own() {
        for (flag, expected) in [
            ("--no-ff", (true, false, false)),
            ("--squash", (false, true, false)),
            ("--ff-only", (false, false, true)),
        ] {
            let cli = Cli::try_parse_from(["gz", "merge", flag])
                .unwrap_or_else(|err| panic!("`gz merge {flag}` should parse: {err}"));

            match cli.command {
                Command::Merge {
                    no_ff,
                    squash,
                    ff_only,
                } => assert_eq!((no_ff, squash, ff_only), expected),
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }
    }

    #[test]
    fn merge_strategy_flags_are_mutually_exclusive() {
        for flags in [
            ["--no-ff", "--squash"],
            ["--no-ff", "--ff-only"],
            ["--squash", "--ff-only"],
        ] {
            Cli::try_parse_from(["gz", "merge", flags[0], flags[1]]).unwrap_err();
        }
    }

    #[test]
    fn rebase_takes_no_options() {
        let cli = Cli::try_parse_from(["gz", "rebase"]).expect("rebase should parse");

        assert!(matches!(cli.command, Command::Rebase));
    }

    #[test]
    fn bare_branch_still_switches_branches() {
        // FR-1 の後方互換: サブコマンドを増やしても引数なしの `gz branch` は切替のまま
        let cli = Cli::try_parse_from(["gz", "branch"]).expect("bare `gz branch` should parse");

        match cli.command {
            Command::Branch { all, command } => {
                assert!(!all);
                assert_eq!(command, None, "サブコマンドなし＝切替");
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn branch_all_still_includes_remote_branches() {
        for arguments in [["gz", "branch", "-a"], ["gz", "branch", "--all"]] {
            let cli = Cli::try_parse_from(arguments).expect("`gz branch --all` should parse");

            match cli.command {
                Command::Branch { all, command } => {
                    assert!(all);
                    assert_eq!(command, None);
                }
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }
    }

    #[test]
    fn branch_exposes_the_management_subcommands() {
        let cli = Cli::try_parse_from(["gz", "branch", "create", "feature", "--switch"])
            .expect("`gz branch create` should parse");
        match cli.command {
            Command::Branch { all, command } => {
                assert!(!all);
                assert_eq!(
                    command,
                    Some(BranchCommand::Create {
                        name: "feature".to_string(),
                        switch: true,
                    })
                );
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "branch", "delete", "--force", "--into", "main"])
            .expect("`gz branch delete` should parse");
        match cli.command {
            Command::Branch { command, .. } => assert_eq!(
                command,
                Some(BranchCommand::Delete {
                    force: true,
                    into: Some("main".to_string()),
                })
            ),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["gz", "branch", "cleanup"]).expect("`gz branch cleanup` parses");
        match cli.command {
            Command::Branch { command, .. } => {
                assert_eq!(command, Some(BranchCommand::Cleanup { into: None }));
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn branch_create_requires_a_name() {
        Cli::try_parse_from(["gz", "branch", "create"])
            .expect_err("the new branch name is required");
    }

    #[test]
    fn the_switch_flags_cannot_be_combined_with_a_management_subcommand() {
        // 切替と管理操作のどちらを意図したのかが曖昧になるため、clap の段階で拒否する
        Cli::try_parse_from(["gz", "branch", "--all", "create", "feature"])
            .expect_err("`--all` and a subcommand must not be combined");
    }

    #[test]
    fn revert_takes_the_no_edit_flag() {
        let cli = Cli::try_parse_from(["gz", "revert", "--no-edit"]).expect("revert should parse");
        match cli.command {
            Command::Revert { no_edit } => assert!(no_edit),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "revert"]).expect("revert should parse bare");
        match cli.command {
            Command::Revert { no_edit } => assert!(!no_edit),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn status_takes_no_options() {
        let cli = Cli::try_parse_from(["gz", "status"]).expect("status should parse");

        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn diff_defaults_to_the_unstaged_comparison() {
        let cli = Cli::try_parse_from(["gz", "diff"]).expect("diff should parse bare");

        match cli.command {
            Command::Diff {
                staged,
                head,
                upstream,
                branch,
                commit,
            } => assert_eq!(
                (staged, head, upstream, branch, commit),
                (false, false, false, false, false)
            ),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn diff_accepts_each_comparison_mode_on_its_own() {
        for (flag, expected) in [
            ("--staged", (true, false, false, false, false)),
            ("--head", (false, true, false, false, false)),
            ("--upstream", (false, false, true, false, false)),
            ("--branch", (false, false, false, true, false)),
            ("--commit", (false, false, false, false, true)),
        ] {
            let cli = Cli::try_parse_from(["gz", "diff", flag])
                .unwrap_or_else(|err| panic!("`gz diff {flag}` should parse: {err}"));

            match cli.command {
                Command::Diff {
                    staged,
                    head,
                    upstream,
                    branch,
                    commit,
                } => assert_eq!((staged, head, upstream, branch, commit), expected),
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }
    }

    #[test]
    fn diff_comparison_modes_are_mutually_exclusive() {
        let modes = ["--staged", "--head", "--upstream", "--branch", "--commit"];
        for (index, first) in modes.iter().enumerate() {
            for second in &modes[index + 1..] {
                assert!(
                    Cli::try_parse_from(["gz", "diff", first, second]).is_err(),
                    "`gz diff {first} {second}` must be rejected"
                );
            }
        }
    }

    #[test]
    fn fetch_takes_the_prune_flag() {
        let cli = Cli::try_parse_from(["gz", "fetch", "--prune"]).expect("fetch should parse");
        match cli.command {
            Command::Fetch { prune, .. } => assert!(prune),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "fetch"]).expect("fetch should parse bare");
        match cli.command {
            Command::Fetch { prune, .. } => assert!(!prune),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn fetch_takes_the_siblings_flag_in_both_spellings() {
        for argument in ["--siblings", "-s"] {
            let cli = Cli::try_parse_from(["gz", "fetch", argument])
                .unwrap_or_else(|err| panic!("`gz fetch {argument}` should parse: {err}"));
            match cli.command {
                Command::Fetch { siblings, .. } => {
                    assert!(siblings, "`{argument}` should enable it")
                }
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }
    }

    #[test]
    fn fetch_targets_only_the_current_repository_by_default() {
        // 兄弟リポジトリへの通信はフラグの明示指定に限る（requirements.md FR-23）
        let cli = Cli::try_parse_from(["gz", "fetch"]).expect("fetch should parse bare");

        match cli.command {
            Command::Fetch { siblings, .. } => assert!(!siblings),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn fetch_accepts_pruning_together_with_siblings() {
        // 併用時は選択したすべてのリポジトリに `--prune` が適用される（排他ではない）
        let cli = Cli::try_parse_from(["gz", "fetch", "--prune", "--siblings"])
            .expect("`gz fetch --prune --siblings` should parse");

        match cli.command {
            Command::Fetch { prune, siblings } => assert_eq!((prune, siblings), (true, true)),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn pull_takes_no_options_and_no_target_argument() {
        let cli = Cli::try_parse_from(["gz", "pull"]).expect("pull should parse bare");
        assert!(matches!(cli.command, Command::Pull));

        // 取り込み方式は fast-forward 固定（方式を選ぶのは `gz sync`）であり、
        // 対象は選択で決めるため名前を打つ余地も設けない
        for arguments in [
            ["gz", "pull", "--rebase"],
            ["gz", "pull", "--merge"],
            ["gz", "pull", "--siblings"],
            ["gz", "pull", "--prune"],
            ["gz", "pull", "main"],
        ] {
            let parsed = Cli::try_parse_from(arguments);
            assert!(parsed.is_err(), "`{arguments:?}` must be rejected");
        }
    }

    #[test]
    fn sync_defaults_to_fast_forward_only() {
        let cli = Cli::try_parse_from(["gz", "sync"]).expect("sync should parse bare");

        match cli.command {
            // どちらのフラグも立っていない状態が ff-only（既定）
            Command::Sync { rebase, merge } => assert_eq!((rebase, merge), (false, false)),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn sync_integration_modes_are_mutually_exclusive() {
        let cli = Cli::try_parse_from(["gz", "sync", "--rebase"]).expect("sync --rebase parses");
        match cli.command {
            Command::Sync { rebase, merge } => assert_eq!((rebase, merge), (true, false)),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "sync", "--merge"]).expect("sync --merge parses");
        match cli.command {
            Command::Sync { rebase, merge } => assert_eq!((rebase, merge), (false, true)),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        Cli::try_parse_from(["gz", "sync", "--rebase", "--merge"])
            .expect_err("--rebase and --merge must not be combined");
    }

    #[test]
    fn bare_worktree_lists_instead_of_requiring_a_subcommand() {
        let cli = Cli::try_parse_from(["gz", "worktree"]).expect("bare `gz worktree` should parse");

        match cli.command {
            Command::Worktree { command } => assert_eq!(command, None),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn worktree_exposes_add_remove_and_prune() {
        let cli = Cli::try_parse_from(["gz", "worktree", "add", "../feature"])
            .expect("`gz worktree add` should parse");
        match cli.command {
            Command::Worktree { command } => assert_eq!(
                command,
                Some(WorktreeCommand::Add {
                    path: PathBuf::from("../feature"),
                })
            ),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        for (argument, expected) in [
            ("remove", WorktreeCommand::Remove),
            ("prune", WorktreeCommand::Prune),
        ] {
            let cli = Cli::try_parse_from(["gz", "worktree", argument])
                .unwrap_or_else(|err| panic!("`gz worktree {argument}` should parse: {err}"));

            match cli.command {
                Command::Worktree { command } => assert_eq!(command, Some(expected)),
                other => panic!("unexpected subcommand: {other:?}"),
            }
        }
    }

    #[test]
    fn worktree_add_requires_a_path() {
        Cli::try_parse_from(["gz", "worktree", "add"])
            .expect_err("the worktree path is required (no automatic suggestion)");
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
