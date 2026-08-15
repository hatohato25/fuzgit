//! 表示言語の決定と、言語ごとの文言（FR-25〜FR-27）。
//!
//! 対応言語は `ja` / `en` の 2 つで、フォールバックは `en`。表示言語は
//! [`resolve::resolve`] が 1 つの規則で決め、決まった [`Language`] を値として
//! 引き回す（グローバル状態にしない理由は [`messages::Messages`] を参照）。
//!
//! # 構成
//!
//! - [`Language`][]: 表示言語そのもの
//! - [`messages`][]: 言語ごとの文言を提供する trait
//! - [`ja`][] / [`en`][]: その実装（フィールドを持たない ZST）
//! - [`resolve`][]: 層 1〜5 の解決規則（純関数）と、環境・git 設定を読む薄い取得層

pub mod en;
pub mod ja;
pub mod messages;
pub mod resolve;

pub use messages::Messages;
pub use resolve::{
    LanguageError, LanguageInputs, LanguageSource, resolve, resolve_from_environment,
};

/// fuzgit が表示に用いる言語。
///
/// 対応するのは 2 つだけで、いずれにも解決できない場合のフォールバックは
/// [`Language::English`]（FR-25 の層 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// 日本語。
    Japanese,
    /// 英語（フォールバック言語）。
    English,
}

impl Language {
    /// 子プロセス `git` の `LANGUAGE` へ渡す言語コード（`ja` / `en`）。
    ///
    /// 子プロセスへ渡すのはこの固定文字列だけであり、ユーザー入力をそのまま
    /// 環境変数へ通すことはない（FR-26）。
    pub fn code(self) -> &'static str {
        match self {
            Self::Japanese => "ja",
            Self::English => "en",
        }
    }

    /// この言語の文言一式を返す。
    ///
    /// 実装はフィールドを持たない ZST であるため `&'static` 参照として返せる
    /// （複製しても実体は増えない）。
    pub fn messages(self) -> &'static dyn Messages {
        match self {
            Self::Japanese => &ja::JapaneseMessages,
            Self::English => &en::EnglishMessages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_returns_the_language_tag() {
        assert_eq!(Language::Japanese.code(), "ja");
        assert_eq!(Language::English.code(), "en");
    }

    #[test]
    fn messages_belong_to_the_language_that_returned_them() {
        assert_eq!(Language::Japanese.messages().language(), Language::Japanese);
        assert_eq!(Language::English.messages().language(), Language::English);
    }

    #[test]
    fn both_languages_can_be_used_side_by_side() {
        // 言語をグローバル状態に持たない設計であることの確認。
        // 同一プロセス内で両言語の文言を同時に扱える（並列テストが干渉しない）。
        let japanese = Language::Japanese.messages();
        let english = Language::English.messages();

        assert_ne!(japanese.language(), english.language());
    }

    /// `describe` の対比テストが対象とする `Error` と、その文言へ展開されるべき引数。
    ///
    /// `RepositoryReadFailed` / `FilesystemReadFailed` / `GitOutputMalformed` も対象に含める。
    /// 三者が持つ `operation`・`detail` は言語に依存しない enum であり、文へ組み立てるのは
    /// ja / en それぞれの `describe` であるため、他のバリアントと同じ規則で検証できる
    /// （`operation` が日本語の `String` だった頃の「`en` を選んでも日本語が混ざる」制約は
    /// 解消済み）。
    ///
    /// バリアントの追加漏れは ja / en の網羅的な `match` がコンパイルエラーで検出する。
    fn describable_errors() -> Vec<(crate::error::Error, Vec<&'static str>)> {
        use crate::error::Error;
        use crate::git::read::{MalformedOutput, ReadOperation};
        use crate::git::siblings::FilesystemOperation;

        let dir = crate::test_support::TempDir::new("i18n-describe");
        let not_a_repository = crate::git::repo::discover(dir.path())
            .expect_err("a temp dir must not be a git repository");

        vec![
            (not_a_repository, vec![]),
            (
                Error::RepositoryReadFailed {
                    operation: ReadOperation::BranchResolve {
                        branch: "feature/login".to_string(),
                    },
                    source: Box::new(std::io::Error::other("no such reference")),
                },
                vec!["feature/login"],
            ),
            (
                Error::GitOutputMalformed {
                    operation: ReadOperation::StashListOutputParse,
                    detail: MalformedOutput::StashSelectorFormat {
                        selector: "stash@{x}".to_string(),
                    },
                },
                vec!["stash@{x}"],
            ),
            (
                Error::FilesystemReadFailed {
                    operation: FilesystemOperation::DirectoryScan,
                    path: std::path::PathBuf::from("/work/repositories"),
                    source: std::io::Error::other("permission denied"),
                },
                vec!["/work/repositories"],
            ),
            (
                Error::GitCommandFailed {
                    args: "switch nope".to_string(),
                    stderr: "fatal: invalid reference: nope".to_string(),
                },
                vec!["switch nope", "fatal: invalid reference: nope"],
            ),
            (
                Error::GitRunFailed {
                    command: "git commit".to_string(),
                    code: Some(3),
                },
                vec!["git commit", "3"],
            ),
            (
                Error::GitRunFailed {
                    command: "git push".to_string(),
                    code: None,
                },
                vec!["git push"],
            ),
            (Error::GitNotFound, vec![]),
            (
                Error::GitSpawnFailed {
                    args: "status --short".to_string(),
                    source: std::io::Error::other("permission denied"),
                },
                vec!["status --short"],
            ),
            (
                Error::UnbornHead {
                    branch: "main".to_string(),
                },
                vec!["main", "git commit"],
            ),
            (Error::NoWorktree, vec![]),
            (
                Error::NoSiblingScope {
                    workdir: std::path::PathBuf::from("/work"),
                },
                vec!["/work"],
            ),
            (Error::Cancelled, vec![]),
            (Error::NoCandidates, vec![]),
            (
                Error::FinderFailed {
                    message: "skim could not start".to_string(),
                },
                vec!["skim could not start"],
            ),
        ]
    }

    #[test]
    fn describe_produces_a_message_in_both_languages() {
        for (error, _) in describable_errors() {
            for language in [Language::Japanese, Language::English] {
                let described = language.messages().errors().describe(&error);
                assert!(
                    !described.trim().is_empty(),
                    "{language:?} must describe {error:?}"
                );
            }
        }
    }

    #[test]
    fn describe_expands_the_arguments_of_the_error() {
        for (error, arguments) in describable_errors() {
            for language in [Language::Japanese, Language::English] {
                let described = language.messages().errors().describe(&error);
                for argument in &arguments {
                    assert!(
                        described.contains(argument),
                        "{language:?} must mention `{argument}`: {described}"
                    );
                }
            }
        }
    }

    #[test]
    fn describe_differs_between_japanese_and_english() {
        for (error, _) in describable_errors() {
            let japanese = Language::Japanese.messages().errors().describe(&error);
            let english = Language::English.messages().errors().describe(&error);

            assert_ne!(
                japanese, english,
                "the description must be translated for {error:?}"
            );
        }
    }

    #[test]
    fn the_error_prefix_is_translated_as_well() {
        // 連鎖の本体だけを訳してラベルが別言語で残ると、そこだけ言語が混ざる
        assert_eq!(Language::Japanese.messages().errors().prefix(), "エラー: ");
        assert_eq!(Language::English.messages().errors().prefix(), "error: ");
    }

    /// 共有語彙のうち、引数を取らないもの。
    fn common_texts(language: Language) -> Vec<&'static str> {
        let common = language.messages().common();

        vec![
            common.stdout_write_failed(),
            common.stderr_write_failed(),
            common.commit_history_read_failed(),
            common.changed_files_read_failed(),
            common.branch_list_read_failed(),
            common.stash_list_read_failed(),
            common.tag_list_read_failed(),
            common.worktree_list_read_failed(),
            common.remote_list_read_failed(),
            common.current_branch_read_failed(),
            common.detached_head_without_upstream(),
            common.history_rewrite_note(),
        ]
    }

    /// 共有語彙のうち引数を取るものと、その文言へ展開されるべき引数。
    fn common_texts_with_arguments(language: Language) -> Vec<(String, &'static str)> {
        let common = language.messages().common();

        vec![
            (common.commit_hash_parse_failed("1f0c9a4"), "1f0c9a4"),
            (common.commit_read_failed("1f0c9a4"), "1f0c9a4"),
            (common.upstream_read_failed("main"), "main"),
            (
                common.ahead_behind_failed("refs/remotes/origin/main"),
                "refs/remotes/origin/main",
            ),
            (common.upstream_not_configured("main"), "main"),
            (common.switch_failed("feature/login"), "feature/login"),
            (common.run_summary(3, 1), "3"),
            (common.failed_targets("alpha, zulu"), "alpha, zulu"),
        ]
    }

    #[test]
    fn no_shared_wording_is_empty() {
        for language in [Language::Japanese, Language::English] {
            for text in common_texts(language) {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
        }
    }

    #[test]
    fn the_shared_wording_is_translated() {
        for (japanese, english) in common_texts(Language::Japanese)
            .into_iter()
            .zip(common_texts(Language::English))
        {
            assert_ne!(japanese, english, "the shared wording must be translated");
        }
    }

    #[test]
    fn the_shared_wording_expands_its_arguments() {
        for language in [Language::Japanese, Language::English] {
            for (text, argument) in common_texts_with_arguments(language) {
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_shared_wording_with_arguments_is_translated() {
        for ((japanese, _), (english, _)) in common_texts_with_arguments(Language::Japanese)
            .into_iter()
            .zip(common_texts_with_arguments(Language::English))
        {
            assert_ne!(japanese, english, "the shared wording must be translated");
        }
    }

    #[test]
    fn a_failed_command_is_named_in_every_language() {
        for language in [Language::Japanese, Language::English] {
            let described = language.messages().common().command_run_failed("git add");

            // git のサブコマンド名は訳さないため、どの言語でもそのまま現れる
            assert!(
                described.contains("git add"),
                "{language:?} must mention the command: {described}"
            );
        }

        assert_ne!(
            Language::Japanese
                .messages()
                .common()
                .command_run_failed("git add"),
            Language::English
                .messages()
                .common()
                .command_run_failed("git add")
        );
    }
}
