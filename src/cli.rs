//! `clap` derive によるコマンドライン定義と、ヘルプ文言の実行時差し替え。
//!
//! # derive のリテラルを英語にする理由（FR-25 / FR-27）
//!
//! derive に書くリテラル（`about` / doc comment）は `&'static str` のコンパイル時定数であり、
//! 実行時に切り替えられない。表示言語ごとの文言は [`localized_command`] が
//! [`CliMessages`](crate::i18n::messages::CliMessages) の値で差し替えて与え、**リテラルには
//! 英語を置く**。差し替えが漏れたときに出るのが英語＝フォールバック言語であり、規定動作と
//! 整合するため（design.md「derive のリテラルを英語にする」）。日本語をリテラルに残すと、
//! 漏れたときに `en` の利用者へ日本語が出てしまう。
//!
//! 設計上の理由づけは doc comment ではなく `//` のコメントで書く。doc comment の 2 段落目
//! 以降を `clap` が `long_about` / `long_help` として `--help` へ出してしまい、**実行時に
//! 差し替えない文言がヘルプに増える**ため（この方針により `-h` と `--help` の内容も一致する）。

use std::path::PathBuf;

use clap::{Command as ClapCommand, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::i18n::Messages;
use crate::i18n::messages::CliMessages;

/// コミット候補の既定取得件数。
///
/// コミット数の多いリポジトリでも初期表示の応答性を確保するための上限で、
/// `gz log --limit` の既定値と `gz cherry-pick` の候補数上限に用いる。
pub const DEFAULT_COMMIT_LIMIT: usize = 1000;

/// `--lang` が受理する値（FR-25 の層 1）。
///
/// 綴りは [`crate::i18n::resolve`] の層 1〜3 が受理する値（`ja` / `en` / `auto`）と
/// 一致させる。`clap` の `value_enum` は既定でバリアント名を小文字化した綴りを使うため、
/// ここでの改名は行わない。[`LangOption::Ja`] は日本語、[`LangOption::En`] は英語、
/// [`LangOption::Auto`] は環境（ロケール環境変数）からの自動判定を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LangOption {
    // バリアントに doc comment を付けない。`clap` はそれを候補値の説明として `--help` へ
    // 出すが、`PossibleValue` の説明は組み立て済みの `Command` から差し替えられず、
    // 表示言語に追随できないため（説明は上の enum の doc comment に置いてある）
    Ja,
    En,
    Auto,
}

/// fuzzy finder で「選ぶ」「探す」「辿る」git 操作 CLI。
#[derive(Debug, Parser)]
#[command(
    name = "gz",
    version,
    about = "A git CLI that lets you pick, search, and trace with a fuzzy finder",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Display language (falls back to FUZGIT_LANG / git config fuzgit.lang / the locale)
    //
    // **この値は権威ではない。**言語は clap のパースより前に
    // `crate::i18n::resolve::scan_lang_flag` による先読みで決まっており（ヘルプ・
    // パーサエラーの文言を選ぶために先読みが必要）、ここに現れるのは同じ指定の
    // パース結果である。両者が一致することは単体テストで固定する。
    // 短縮形（`-l` 等）は設けない（requirements.md FR-25）。
    #[arg(long, global = true, value_enum, value_name = "LANG")]
    pub lang: Option<LangOption>,

    /// 実行するサブコマンド。
    #[command(subcommand)]
    pub command: Command,
}

/// `gz` のサブコマンド。
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Pick a branch and switch to it (subcommands also create, delete and tidy up)
    //
    // 引数なしの `gz branch` と `gz branch --all` は従来どおりの切替（FR-1）。
    // `args_conflicts_with_subcommands` により、切替用のフラグと管理サブコマンドの
    // 併用は clap の段階で拒否される（どちらの操作なのかが曖昧にならないようにするため）。
    #[command(args_conflicts_with_subcommands = true)]
    Branch {
        /// Include remote-tracking branches in the candidates
        #[arg(short, long)]
        all: bool,

        /// 実行するブランチ管理の操作（省略時はブランチの切替）。
        #[command(subcommand)]
        command: Option<BranchCommand>,
    },

    /// Trace the commit history and print the full hash of the picked commit
    Log {
        /// Maximum number of commits to read
        #[arg(short = 'n', long, value_name = "N", default_value_t = DEFAULT_COMMIT_LIMIT)]
        limit: usize,
    },

    /// Pick a commit and cherry-pick it
    CherryPick {
        /// Target branch (without it, the commits of every branch are offered)
        #[arg(short, long, value_name = "BRANCH")]
        branch: Option<String>,
    },

    /// Pick files and restore them with git restore
    Restore {
        /// Revision to restore from
        #[arg(short, long, value_name = "REV")]
        source: Option<String>,

        /// Unstage the staged changes
        //
        // `-S` は `git restore -S` と同じ綴り。同コマンドの `-s`（`--source`）との
        // 大文字小文字の組み合わせまで git と一致する
        #[arg(short = 'S', long)]
        staged: bool,
    },

    /// Pick unstaged and untracked files and stage them with git add
    Add,

    /// Stash changes away, then search the stashes to apply or drop them
    #[command(subcommand_required = true, arg_required_else_help = true)]
    Stash {
        /// 実行する stash の操作。
        #[command(subcommand)]
        command: StashCommand,
    },

    /// Pick a tag (prints the tag name by default)
    Tag {
        /// Switch to the picked tag as a detached HEAD
        #[arg(long, conflicts_with = "diff")]
        switch: bool,

        /// Show the diff between the picked tag and HEAD
        #[arg(long)]
        diff: bool,
    },

    /// Trace the reflog of HEAD and print the hash of the picked commit
    Reflog {
        /// Create a new branch with the given name from the picked commit
        #[arg(long, value_name = "NAME")]
        restore: Option<String>,
    },

    /// Pick the files to commit and commit them
    Commit {
        /// Commit message (without it, git starts an editor)
        #[arg(short, long, value_name = "MESSAGE")]
        message: Option<String>,
    },

    /// Pick the commit to amend and create a fixup commit
    Fixup {
        /// Create a squash commit (which joins the messages) instead of a fixup one
        #[arg(long)]
        squash: bool,
    },

    /// Pick a branch and merge it
    Merge {
        /// Create a merge commit even when a fast-forward is possible
        #[arg(long, conflicts_with_all = ["squash", "ff_only"])]
        no_ff: bool,

        /// Apply the merge result to the working tree and the index without committing
        #[arg(long, conflicts_with_all = ["no_ff", "ff_only"])]
        squash: bool,

        /// Merge only when a fast-forward is possible
        #[arg(long, conflicts_with_all = ["no_ff", "squash"])]
        ff_only: bool,
    },

    /// Pick a branch and rebase onto it
    Rebase,

    /// Pick a commit and undo it (creates a revert commit)
    Revert {
        /// Commit with the default message of git without starting an editor
        #[arg(long)]
        no_edit: bool,
    },

    /// List the state of the changed files and act on the picked ones
    Status,

    /// Pick what to compare and show the diff
    //
    // 引数なしは未ステージの変更（`git diff` と同じ）。比較モードは相互排他。
    Diff {
        /// Target the staged changes (same as `git diff --staged`)
        #[arg(long, conflicts_with_all = ["head", "upstream", "branch", "commit"])]
        staged: bool,

        /// Compare HEAD with the working tree (including the staged changes)
        #[arg(long, conflicts_with_all = ["staged", "upstream", "branch", "commit"])]
        head: bool,

        /// Compare HEAD with the upstream
        #[arg(long, conflicts_with_all = ["staged", "head", "branch", "commit"])]
        upstream: bool,

        /// Pick two branches and compare them
        #[arg(long, conflicts_with_all = ["staged", "head", "upstream", "commit"])]
        branch: bool,

        /// Pick two commits and compare them
        #[arg(long, conflicts_with_all = ["staged", "head", "upstream", "branch"])]
        commit: bool,
    },

    /// Decide what to fetch and fetch it
    //
    // リモートが 1 つだけの場合は選択の余地が無いため、finder を起動せず即座に取得する。
    Fetch {
        /// Also clean up the tracking refs of branches deleted on the remote
        //
        // `-p` は `git fetch -p` と同じ綴り
        #[arg(short, long)]
        prune: bool,

        /// Include the repositories next to this one and fetch them all at once
        #[arg(long, short)]
        siblings: bool,
    },

    /// Pick branches and make them follow their upstream (fast-forward only)
    //
    // 取り込み方式は fast-forward に固定し、`--rebase` / `--merge` は提供しない
    // （方式を選んで 1 本だけ同期するのは `gz sync`）。対象は選択で決めるため
    // 位置引数を取らず、`--siblings` / `--prune` も持たない。
    Pull,

    /// Synchronize the current branch with its upstream
    //
    // 既定は fast-forward のみ（`--ff-only` 相当）。fast-forward できない場合は
    // git のエラーをそのまま表示して停止し、暗黙に merge / rebase へ倒さない。
    Sync {
        /// Integrate by rebasing onto the upstream (rewrites history)
        //
        // `-r` は `git pull -r` と同じ綴り。`--merge` に短縮形を付けないのは、
        // `git pull` に対応する綴りが無く、git 全体では `-m` が `--message` を指すため
        #[arg(short, long, conflicts_with_all = ["merge"])]
        rebase: bool,

        /// Integrate by merging the upstream
        #[arg(long, conflicts_with_all = ["rebase"])]
        merge: bool,
    },

    /// List and manage worktrees (without arguments, prints the path picked from the list)
    Worktree {
        /// 実行する worktree の操作（省略時は一覧からの選択）。
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
    /// Pick a starting point and create a new branch
    Create {
        /// Name of the branch to create
        name: String,

        /// Switch to the branch after creating it
        #[arg(long)]
        switch: bool,
    },

    /// Pick a branch and delete it
    Delete {
        /// Delete a branch even when it is not merged (`git branch -D`)
        //
        // `-f` は `git branch -f` と同じ綴り
        #[arg(short, long)]
        force: bool,

        /// Branch that `merged` is judged against (defaults to HEAD)
        #[arg(long, value_name = "BRANCH")]
        into: Option<String>,
    },

    /// Delete every merged branch at once
    Cleanup {
        /// Branch that `merged` is judged against (defaults to HEAD)
        #[arg(long, value_name = "BRANCH")]
        into: Option<String>,
    },
}

/// `gz worktree` のサブコマンド（FR-21）。
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum WorktreeCommand {
    /// Pick a branch and create a new worktree for it
    Add {
        /// Path of the worktree to create (no directory name is suggested)
        path: PathBuf,
    },

    /// Pick a worktree and remove it (the main worktree is not offered)
    Remove,

    /// Tidy up the bookkeeping of worktrees whose directory is gone
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
    /// Pick changed files and stash them away
    Push {
        /// Message to attach to the stash
        #[arg(short, long, value_name = "MESSAGE")]
        message: Option<String>,

        /// Include untracked files in the candidates (tracked changes only by default)
        #[arg(short = 'u', long)]
        include_untracked: bool,
    },

    /// Pick a stash and apply it (the stash is kept)
    Apply,

    /// Pick a stash, apply it and drop that stash
    Pop,

    /// Pick a stash and drop it
    Drop,
}

/// 表示言語の文言でヘルプを差し替えた `clap` のコマンド定義を組み立てる。
///
/// `--lang` は `clap` のパースより前に先読み（`crate::i18n::resolve::scan_lang_flag`）で
/// 解決されており、`messages` はその解決結果である。この順序があるからこそ、`--help` と
/// パーサエラーを解決済みの言語で出せる（design.md「起動シーケンス」）。
///
/// 差し替えの対象は `about`（`gz` 自身と各サブコマンド）と `help`（各引数）に限る。
/// derive のリテラルを 1 行に保っているため `long_about` / `long_help` は存在せず、
/// `-h` と `--help` はどちらも差し替え後の文言を出す。`Usage:` / `Options:` といった
/// `clap` 自身の見出しとパーサエラーは英語のまま（requirements.md のスコープ外）。
///
/// # Panics
///
/// `clap` の `mut_arg` / `mut_subcommand` は、指定した ID・名前が定義に無いと panic する。
/// 起動直後に必ず通る経路であり、綴りの誤りはすべてのテストが即座に落ちて検出されるため、
/// ここで握りつぶさず `clap` の挙動に委ねる。
pub fn localized_command(messages: &dyn Messages) -> ClapCommand {
    let cli = messages.cli();

    // `mut_subcommand` は対象を末尾へ移動するため、**定義順に呼ぶ**ことでヘルプの並び順が
    // 元のまま保たれる（1 つでも飛ばすとそのサブコマンドだけ先頭へ残り順序が変わる）
    Cli::command()
        .about(cli.about())
        .mut_arg("lang", |argument| argument.help(cli.lang_help()))
        .mut_subcommand("branch", |command| localize_branch(command, cli))
        .mut_subcommand("log", |command| {
            command
                .about(cli.log_about())
                .mut_arg("limit", |argument| argument.help(cli.log_limit_help()))
        })
        .mut_subcommand("cherry-pick", |command| {
            command
                .about(cli.cherry_pick_about())
                .mut_arg("branch", |argument| {
                    argument.help(cli.cherry_pick_branch_help())
                })
        })
        .mut_subcommand("restore", |command| {
            command
                .about(cli.restore_about())
                .mut_arg("source", |argument| {
                    argument.help(cli.restore_source_help())
                })
                .mut_arg("staged", |argument| {
                    argument.help(cli.restore_staged_help())
                })
        })
        .mut_subcommand("add", |command| command.about(cli.add_about()))
        .mut_subcommand("stash", |command| localize_stash(command, cli))
        .mut_subcommand("tag", |command| {
            command
                .about(cli.tag_about())
                .mut_arg("switch", |argument| argument.help(cli.tag_switch_help()))
                .mut_arg("diff", |argument| argument.help(cli.tag_diff_help()))
        })
        .mut_subcommand("reflog", |command| {
            command
                .about(cli.reflog_about())
                .mut_arg("restore", |argument| {
                    argument.help(cli.reflog_restore_help())
                })
        })
        .mut_subcommand("commit", |command| {
            command
                .about(cli.commit_about())
                .mut_arg("message", |argument| {
                    argument.help(cli.commit_message_help())
                })
        })
        .mut_subcommand("fixup", |command| {
            command
                .about(cli.fixup_about())
                .mut_arg("squash", |argument| argument.help(cli.fixup_squash_help()))
        })
        .mut_subcommand("merge", |command| {
            command
                .about(cli.merge_about())
                .mut_arg("no_ff", |argument| argument.help(cli.merge_no_ff_help()))
                .mut_arg("squash", |argument| argument.help(cli.merge_squash_help()))
                .mut_arg("ff_only", |argument| {
                    argument.help(cli.merge_ff_only_help())
                })
        })
        .mut_subcommand("rebase", |command| command.about(cli.rebase_about()))
        .mut_subcommand("revert", |command| {
            command
                .about(cli.revert_about())
                .mut_arg("no_edit", |argument| {
                    argument.help(cli.revert_no_edit_help())
                })
        })
        .mut_subcommand("status", |command| command.about(cli.status_about()))
        .mut_subcommand("diff", |command| {
            command
                .about(cli.diff_about())
                .mut_arg("staged", |argument| argument.help(cli.diff_staged_help()))
                .mut_arg("head", |argument| argument.help(cli.diff_head_help()))
                .mut_arg("upstream", |argument| {
                    argument.help(cli.diff_upstream_help())
                })
                .mut_arg("branch", |argument| argument.help(cli.diff_branch_help()))
                .mut_arg("commit", |argument| argument.help(cli.diff_commit_help()))
        })
        .mut_subcommand("fetch", |command| {
            command
                .about(cli.fetch_about())
                .mut_arg("prune", |argument| argument.help(cli.fetch_prune_help()))
                .mut_arg("siblings", |argument| {
                    argument.help(cli.fetch_siblings_help())
                })
        })
        .mut_subcommand("pull", |command| command.about(cli.pull_about()))
        .mut_subcommand("sync", |command| {
            command
                .about(cli.sync_about())
                .mut_arg("rebase", |argument| argument.help(cli.sync_rebase_help()))
                .mut_arg("merge", |argument| argument.help(cli.sync_merge_help()))
        })
        .mut_subcommand("worktree", |command| localize_worktree(command, cli))
}

/// `gz branch` と、その管理サブコマンド（[`BranchCommand`]）のヘルプを差し替える。
fn localize_branch(command: ClapCommand, cli: &dyn CliMessages) -> ClapCommand {
    command
        .about(cli.branch_about())
        .mut_arg("all", |argument| argument.help(cli.branch_all_help()))
        .mut_subcommand("create", |create| {
            create
                .about(cli.branch_create_about())
                .mut_arg("name", |argument| {
                    argument.help(cli.branch_create_name_help())
                })
                .mut_arg("switch", |argument| {
                    argument.help(cli.branch_create_switch_help())
                })
        })
        .mut_subcommand("delete", |delete| {
            delete
                .about(cli.branch_delete_about())
                .mut_arg("force", |argument| {
                    argument.help(cli.branch_delete_force_help())
                })
                .mut_arg("into", |argument| {
                    argument.help(cli.branch_delete_into_help())
                })
        })
        .mut_subcommand("cleanup", |cleanup| {
            cleanup
                .about(cli.branch_cleanup_about())
                .mut_arg("into", |argument| {
                    argument.help(cli.branch_cleanup_into_help())
                })
        })
}

/// `gz stash` と、その操作サブコマンド（[`StashCommand`]）のヘルプを差し替える。
fn localize_stash(command: ClapCommand, cli: &dyn CliMessages) -> ClapCommand {
    command
        .about(cli.stash_about())
        .mut_subcommand("push", |push| {
            push.about(cli.stash_push_about())
                .mut_arg("message", |argument| {
                    argument.help(cli.stash_push_message_help())
                })
                .mut_arg("include_untracked", |argument| {
                    argument.help(cli.stash_push_include_untracked_help())
                })
        })
        .mut_subcommand("apply", |apply| apply.about(cli.stash_apply_about()))
        .mut_subcommand("pop", |pop| pop.about(cli.stash_pop_about()))
        .mut_subcommand("drop", |drop| drop.about(cli.stash_drop_about()))
}

/// `gz worktree` と、その操作サブコマンド（[`WorktreeCommand`]）のヘルプを差し替える。
fn localize_worktree(command: ClapCommand, cli: &dyn CliMessages) -> ClapCommand {
    command
        .about(cli.worktree_about())
        .mut_subcommand("add", |add| {
            add.about(cli.worktree_add_about())
                .mut_arg("path", |argument| {
                    argument.help(cli.worktree_add_path_help())
                })
        })
        .mut_subcommand("remove", |remove| remove.about(cli.worktree_remove_about()))
        .mut_subcommand("prune", |prune| prune.about(cli.worktree_prune_about()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::i18n::resolve::scan_lang_flag;
    use crate::test_support::contains_japanese;
    use std::ffi::OsString;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// [`localized_command`] が組み立てた定義も `clap` の整合性検査を通ることを固定する。
    ///
    /// 差し替えは `mut_arg` / `mut_subcommand` で組み立て済みの定義を書き換えるため、
    /// ID の綴り誤りや構造の破壊はコンパイルエラーにならない。両言語で検査しておくと
    /// 差し替えが構造を壊した場合にここで落ちる。
    #[test]
    fn the_localized_definition_is_valid_in_every_language() {
        for language in [Language::Japanese, Language::English] {
            localized_command(language.messages()).debug_assert();
        }
    }

    /// `clap` の定義を再帰的に辿り、`about` と各引数の `help` を順に返す。
    ///
    /// `clap` が自ら足す `-h` / `--help` / `-V` / `--version` と `help` サブコマンドは
    /// `clap` 自身の文言であり、fuzgit が差し替える対象ではないため除く。
    fn descriptions(command: &ClapCommand) -> Vec<String> {
        let mut collected = Vec::new();

        if let Some(about) = command.get_about() {
            collected.push(about.to_string());
        }

        for argument in command.get_arguments() {
            let id = argument.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            if let Some(help) = argument.get_help() {
                collected.push(help.to_string());
            }
        }

        for subcommand in command.get_subcommands() {
            if subcommand.get_name() == "help" {
                continue;
            }
            collected.extend(descriptions(subcommand));
        }

        collected
    }

    /// derive のリテラルが英語のままであることを固定する（フォールバック言語との整合）。
    ///
    /// 差し替えが漏れたときに出るのはここに書かれたリテラルであり、日本語が混ざっていると
    /// `en` を選んだ利用者へ日本語が出てしまう。
    #[test]
    fn the_derive_literals_stay_in_english() {
        for description in descriptions(&Cli::command()) {
            assert!(
                !contains_japanese(&description),
                "the derive literals must stay in english: {description}"
            );
        }
    }

    /// 差し替え後の定義でも、パース結果が derive のパースと一致することを固定する。
    ///
    /// `main` は `Cli::parse()` ではなく `localized_command(...).get_matches_from(...)` →
    /// `Cli::from_arg_matches(...)` を通る。ヘルプの差し替えが引数の解釈まで変えていない
    /// ことを確かめる。
    #[test]
    fn the_localized_command_parses_the_same_arguments() {
        use clap::FromArgMatches as _;

        for argv in [
            vec!["gz", "branch", "--all"],
            vec!["gz", "log", "--limit", "5"],
            vec!["gz", "stash", "push", "-m", "作業中", "-u"],
            vec!["gz", "worktree", "add", "../feature"],
            vec!["gz", "--lang", "en", "diff", "--staged"],
        ] {
            let matches = localized_command(Language::Japanese.messages())
                .try_get_matches_from(&argv)
                .unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"));
            let localized = Cli::from_arg_matches(&matches)
                .unwrap_or_else(|err| panic!("{argv:?} should be extracted: {err}"));

            let derived = Cli::try_parse_from(&argv)
                .unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"));

            assert_eq!(
                format!("{localized:?}"),
                format!("{derived:?}"),
                "the localized definition must parse {argv:?} the same way"
            );
        }
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
    fn push_is_not_offered() {
        // push は fuzzy finder で選ぶ価値のある軸が無いため提供しない
        // （requirements.md「スコープ外」）。素の `git push` に委ねる
        assert!(
            Cli::try_parse_from(["gz", "push"]).is_err(),
            "`gz push` must not be a subcommand"
        );
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
    fn the_short_options_follow_git() {
        // 短縮形は git 本体に同じ意味の綴りがあるものだけに付ける。
        // `gz restore` は `-s`（`--source`）と `-S`（`--staged`）の大文字小文字の
        // 組み合わせまで `git restore` と一致する
        let cli =
            Cli::try_parse_from(["gz", "restore", "-S"]).expect("`gz restore -S` should parse");
        match cli.command {
            Command::Restore { staged, source } => {
                assert!(staged, "-S should mean --staged");
                assert_eq!(source, None, "-S must not be mistaken for --source");
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }

        let cli = Cli::try_parse_from(["gz", "restore", "-s", "HEAD~1"])
            .expect("`gz restore -s` should still take a value");
        match cli.command {
            Command::Restore { staged, source } => {
                assert_eq!(source.as_deref(), Some("HEAD~1"));
                assert!(!staged, "-s must not enable --staged");
            }
            other => panic!("unexpected subcommand: {other:?}"),
        }

        // `git pull -r` と同じ綴り
        let cli = Cli::try_parse_from(["gz", "sync", "-r"]).expect("`gz sync -r` should parse");
        match cli.command {
            Command::Sync { rebase, merge } => assert_eq!((rebase, merge), (true, false)),
            other => panic!("unexpected subcommand: {other:?}"),
        }

        // `git branch -f` と同じ綴り
        let cli = Cli::try_parse_from(["gz", "branch", "delete", "-f"])
            .expect("`gz branch delete -f` should parse");
        match cli.command {
            Command::Branch {
                command: Some(BranchCommand::Delete { force, .. }),
                ..
            } => assert!(force),
            other => panic!("unexpected subcommand: {other:?}"),
        }
    }

    #[test]
    fn the_options_without_a_git_counterpart_have_no_short_form() {
        // `git tag -d` はタグ削除、`git tag -s` は GPG 署名、`git commit -s` は signoff。
        // 同じ綴りに別の意味を与えると誤操作を招くため、これらには短縮形を付けない
        for arguments in [
            ["gz", "tag", "-d"],
            ["gz", "tag", "-s"],
            ["gz", "fixup", "-s"],
            ["gz", "sync", "-m"],
        ] {
            assert!(
                Cli::try_parse_from(arguments).is_err(),
                "{arguments:?} must not be accepted as a short form"
            );
        }
    }

    #[test]
    fn fetch_takes_the_prune_flag() {
        // 短縮形は `git fetch -p` と同じ綴り
        for argument in ["--prune", "-p"] {
            let cli = Cli::try_parse_from(["gz", "fetch", argument])
                .unwrap_or_else(|err| panic!("`gz fetch {argument}` should parse: {err}"));
            match cli.command {
                Command::Fetch { prune, .. } => assert!(prune, "`{argument}` should enable it"),
                other => panic!("unexpected subcommand: {other:?}"),
            }
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

    /// `--lang` の値をパース結果と先読み結果で比較するための綴りへ戻す。
    fn lang_value(option: LangOption) -> &'static str {
        match option {
            LangOption::Ja => "ja",
            LangOption::En => "en",
            LangOption::Auto => "auto",
        }
    }

    #[test]
    fn the_lang_flag_is_read_alike_by_the_prescan_and_by_clap() {
        // 先読み（層 1 の権威）と clap のパース結果が食い違うと、ヘルプだけが別の言語で
        // 出る事故になる。想定する綴りについて両者が一致することを固定する
        for argv in [
            vec!["gz", "--lang", "ja", "log"],
            vec!["gz", "--lang=ja", "log"],
            vec!["gz", "log", "--lang", "en"],
            vec!["gz", "log", "--lang=auto"],
        ] {
            let scanned = scan_lang_flag(&argv.iter().map(OsString::from).collect::<Vec<_>>());
            let parsed = Cli::try_parse_from(&argv)
                .unwrap_or_else(|err| panic!("{argv:?} should parse: {err}"))
                .lang;

            assert_eq!(
                scanned.as_deref(),
                parsed.map(lang_value),
                "the prescan and clap must agree on {argv:?}"
            );
        }
    }

    #[test]
    fn the_lang_flag_is_not_read_after_the_argument_terminator() {
        // `--` より後ろは先読みの対象外。clap も `--lang` をフラグとしては解釈しないため
        // （`gz log` は位置引数を取らず、この引数列はパーサエラーになる）、
        // 「先読みだけが拾ってしまう」乖離は生じない。乖離が起きるのは
        // clap のヘルプ・パーサエラーの言語だけであり、doc comment に明記してある
        let argv = ["gz", "log", "--", "--lang", "ja"];

        let scanned = scan_lang_flag(&argv.iter().map(OsString::from).collect::<Vec<_>>());

        assert_eq!(scanned, None);
        Cli::try_parse_from(argv).expect_err("`gz log` takes no operands");
    }

    #[test]
    fn the_lang_flag_is_available_on_every_subcommand() {
        // global = true であることの確認（サブコマンドごとに定義していない）
        for subcommand in ["branch", "status", "worktree"] {
            Cli::try_parse_from(["gz", subcommand, "--lang", "en"])
                .unwrap_or_else(|err| panic!("`gz {subcommand} --lang en` should parse: {err}"));
        }
    }
}
