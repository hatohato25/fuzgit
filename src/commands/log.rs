//! `gz log` — コミット履歴を辿り、選択したコミットのフルハッシュを出力する（FR-2）。

use std::io::Write as _;

use anyhow::{Context as _, Result};

use anyhow::anyhow;

use crate::commands::commit_menu::{MenuAction, Target};
use crate::commands::{
    commit_highlights, commit_line, commit_menu, commit_preview_args, selection_header,
};
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_one_with};
use crate::git::read::{CommitInfo, CommitScope, commits};
use crate::i18n::{Language, Messages};

/// `gz log --action` のメニューに載せる操作（表示順）。
///
/// `git reset --hard` を**載せない**のは、`gz log` の候補が HEAD の祖先であり、そこでの
/// reset は「自分のコミットを捨てる」操作になるためである。同じ目的は非破壊の revert が
/// 満たしており、破壊的な方を並べる理由が無い（requirements.md「スコープ外」）。
const MENU: [MenuAction; 6] = [
    MenuAction::Show,
    MenuAction::SwitchDetach,
    MenuAction::CherryPick,
    MenuAction::Revert,
    MenuAction::Fixup,
    MenuAction::PrintHash,
];

/// 候補を決定したときに何をするか。
///
/// 同じ候補一覧でもフラグで結果が変わるため、選択前のヘッダーと決定後の挙動を
/// 1 つの値から導く（[`crate::commands::selection_header`] の規約）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// フルハッシュを標準出力へ書き出す（既定）。
    Print,
    /// アクションメニューを開く（`--action`）。
    Menu,
}

impl Decision {
    /// `--action` の有無から決定内容を導く。
    #[must_use]
    pub fn from_flag(action: bool) -> Self {
        if action { Self::Menu } else { Self::Print }
    }

    /// 候補一覧のヘッダーに示す「決定すると何が起きるのか」。
    ///
    /// ワイルドカードの腕を置かないのは、決定内容を増やしたときにヘッダーの
    /// 追加漏れがコンパイルエラーになるようにするため。
    fn header_outcome(self, messages: &dyn Messages) -> &'static str {
        match self {
            Self::Print => messages.log().header_outcome_print(),
            Self::Menu => messages.log().header_outcome_menu(),
        }
    }
}

/// コミット履歴から 1 件選び、`decision` に応じてハッシュを出力するかメニューを開く。
///
/// # Errors
///
/// コミット履歴の取得、選択（中断を含む）、標準出力への書き込み、メニューから実行した
/// 操作に失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    limit: usize,
    decision: Decision,
) -> Result<()> {
    let candidates = commits(repository, CommitScope::Head, limit)
        .context(messages.common().commit_history_read_failed())?;

    let items = candidates
        .iter()
        .map(|commit| to_item(language, commit))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.log().header_subject(),
        decision.header_outcome(messages),
    ));
    let selected = select_one_with(items, &options)?;

    // 選択結果が候補一覧に含まれることを確かめてから使う（対象を取り違えたまま
    // git 操作を実行しないため）。既定の出力経路でも同じ照合を通す
    let commit = candidates
        .iter()
        .find(|candidate| candidate.id == selected)
        .ok_or_else(|| anyhow!(messages.log().selection_not_found(&selected)))?;

    match decision {
        Decision::Print => {
            // パイプ利用を想定し、stdout にはフルハッシュ以外を混ぜない。
            // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
            writeln!(std::io::stdout(), "{id}", id = commit.id)
                .context(messages.common().stdout_write_failed())?;
        }
        Decision::Menu => {
            let target = Target {
                id: &commit.id,
                short_id: &commit.short_id,
                label: &display_line(commit),
            };
            commit_menu::run(language, messages, repository, &target, &MENU)?;
        }
    }

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
    commit_preview_args(&commit.id)
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
    fn the_menu_is_opt_in() {
        assert_eq!(Decision::from_flag(false), Decision::Print);
        assert_eq!(Decision::from_flag(true), Decision::Menu);
    }

    #[test]
    fn each_decision_announces_a_different_outcome() {
        // 選択前に見えている説明と決定後の挙動が食い違ってはならない
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();

            assert_ne!(
                Decision::Print.header_outcome(messages),
                Decision::Menu.header_outcome(messages),
                "{language:?} must tell the two apart"
            );
        }
    }

    #[test]
    fn the_menu_never_offers_a_hard_reset() {
        // `gz log` の候補は HEAD の祖先であり、そこでの reset は「自分のコミットを捨てる」
        // 操作になる。同じ目的は非破壊の revert が満たす（requirements.md「スコープ外」）
        assert!(!MENU.contains(&MenuAction::ResetHard));
        assert!(MENU.contains(&MenuAction::Revert));
    }

    #[test]
    fn the_menu_can_still_print_the_hash() {
        // メニューを開いてもパイプ用途へ戻れる経路を残す
        assert!(MENU.contains(&MenuAction::PrintHash));
    }

    #[test]
    fn a_commit_outside_the_candidates_is_reported() {
        // 対象を取り違えたまま git 操作を実行しないよう、暗黙に読み飛ばさない
        for language in [Language::Japanese, Language::English] {
            let message = language.messages().log().selection_not_found("deadbeef");

            assert!(
                message.contains("deadbeef"),
                "{language:?} must mention the picked commit: {message}"
            );
        }
    }

    #[test]
    fn every_log_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let log = language.messages().log();

            for text in [
                log.header_subject(),
                log.header_outcome_print(),
                log.header_outcome_menu(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            assert!(!log.selection_not_found("deadbeef").trim().is_empty());
        }
    }

    #[test]
    fn the_log_wording_is_translated() {
        let japanese = Language::Japanese.messages().log();
        let english = Language::English.messages().log();

        assert_ne!(japanese.header_subject(), english.header_subject());
        assert_ne!(
            japanese.header_outcome_print(),
            english.header_outcome_print()
        );
        assert_ne!(
            japanese.header_outcome_menu(),
            english.header_outcome_menu()
        );
        assert_ne!(
            japanese.selection_not_found("deadbeef"),
            english.selection_not_found("deadbeef")
        );
    }
}
