//! `gz log` — コミット履歴を辿り、選択したコミットのフルハッシュを出力する（FR-2）。

use std::io::Write as _;

use anyhow::{Context as _, Result};

use crate::commands::{commit_highlights, commit_line, selection_header};
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_one_with};
use crate::git::read::{CommitInfo, CommitScope, commits};
use crate::i18n::{Language, Messages};

/// コミット履歴から 1 件選び、そのフルハッシュを標準出力へ書き出す。
///
/// # Errors
///
/// コミット履歴の取得、選択（中断を含む）、標準出力への書き込みに失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    limit: usize,
) -> Result<()> {
    let candidates = commits(repository, CommitScope::Head, limit)
        .context(messages.common().commit_history_read_failed())?;

    let items = candidates
        .iter()
        .map(|commit| to_item(language, commit))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.log().header_subject(),
        messages.log().header_outcome(),
    ));
    let selected = select_one_with(items, &options)?;

    // パイプ利用を想定し、stdout にはフルハッシュ以外を混ぜない。
    // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
    writeln!(std::io::stdout(), "{selected}").context(messages.common().stdout_write_failed())?;

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// コミットメッセージでの絞り込みを主用途とするため、サマリを作者より前に置く。
fn display_line(commit: &CommitInfo) -> String {
    commit_line(commit)
}

/// プレビュー用の `git show` の引数を組み立てる。
fn preview_args(commit: &CommitInfo) -> Vec<String> {
    // 末尾の `--` により、ハッシュがパスではなくリビジョンとして解釈されることを保証する
    ["show", "--color=always", &commit.id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// コミットを finder の候補へ変換する。
fn to_item(language: Language, commit: &CommitInfo) -> FinderItem {
    FinderItem::new(
        display_line(commit),
        commit.id.clone(),
        PreviewSource::Git(preview_args(commit)),
        language.messages(),
    )
    .with_highlights(commit_highlights(commit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit() -> CommitInfo {
        CommitInfo {
            id: "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345".to_owned(),
            short_id: "1f0c9a4".to_owned(),
            summary: "ブランチ切替を実装する".to_owned(),
            author: "fuzgit test".to_owned(),
            time: "2024-01-02".to_owned(),
        }
    }

    #[test]
    fn a_line_shows_the_short_hash_date_summary_and_author() {
        assert_eq!(
            display_line(&commit()),
            "1f0c9a4 2024-01-02 ブランチ切替を実装する (fuzgit test)"
        );
    }

    #[test]
    fn the_preview_shows_the_commit_and_ends_with_a_path_separator() {
        assert_eq!(
            preview_args(&commit()),
            [
                "show",
                "--color=always",
                "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345",
                "--"
            ]
        );
    }

    #[test]
    fn an_item_keeps_the_full_hash_as_its_key() {
        let item = to_item(Language::Japanese, &commit());

        assert_eq!(item.key(), "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345");
    }

    #[test]
    fn every_log_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let log = language.messages().log();

            for text in [log.header_subject(), log.header_outcome()] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
        }
    }

    #[test]
    fn the_log_wording_is_translated() {
        let japanese = Language::Japanese.messages().log();
        let english = Language::English.messages().log();

        assert_ne!(japanese.header_subject(), english.header_subject());
        assert_ne!(japanese.header_outcome(), english.header_outcome());
    }
}
