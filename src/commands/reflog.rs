//! `gz reflog` — HEAD の reflog を辿り、失われたコミットを取り出す（FR-8）。

use std::io::Write as _;

use anyhow::{Context as _, Result, anyhow};

use crate::commands::selection_header;
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_one_with};
use crate::git::exec::run_git;
use crate::git::read::{ReflogEntry, head_reflog};
use crate::i18n::{Language, Messages};

/// 一覧に表示する短縮ハッシュの桁数。
const SHORT_ID_LENGTH: usize = 7;

/// reflog エントリを 1 件選び、そのコミットを出力またはブランチとして復元する。
///
/// `branch` を指定した場合はその名前で新規ブランチを作成し、未指定の場合は
/// 選択したコミットのフルハッシュを標準出力へ書き出す。
///
/// # Errors
///
/// reflog の取得、選択（中断を含む）、標準出力への書き込み、`git branch` の実行に
/// 失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    branch: Option<&str>,
) -> Result<()> {
    let candidates = head_reflog(repository).context(messages.reflog().read_failed())?;

    let items = candidates
        .iter()
        .map(|entry| to_item(language, entry))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.reflog().header_subject(),
        &header_outcome(messages, branch),
    ));
    let selected = select_one_with(items, &options)?;

    let entry = candidates
        .iter()
        .find(|candidate| candidate.selector() == selected)
        .ok_or_else(|| anyhow!(messages.reflog().selection_not_found(&selected)))?;

    match branch {
        Some(name) => {
            let arguments = branch_args(name, &entry.id);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            run_git(language, &arguments)
                .with_context(|| messages.reflog().branch_creation_failed(name))?;

            // git branch は成功時に何も出力しないため、作成結果を標準エラーへ知らせる
            // （標準出力はパイプ利用のために空けておく）
            writeln!(
                std::io::stderr(),
                "{created}",
                created = messages.reflog().branch_created(name, &entry.id)
            )
            .context(messages.common().stderr_write_failed())?;
        }
        None => {
            // パイプ利用を想定し、stdout にはフルハッシュ以外を混ぜない。
            // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
            writeln!(std::io::stdout(), "{id}", id = entry.id)
                .context(messages.common().stdout_write_failed())?;
        }
    }

    Ok(())
}

/// 候補一覧のヘッダーで示す「決定すると何が起きるのか」。
///
/// `--restore` の有無で結果が変わる。既定は標準出力へ書くだけだが、指定された場合は
/// リポジトリにブランチが増えるため、作成される名前まで選択前に示す。
fn header_outcome(messages: &dyn Messages, branch: Option<&str>) -> String {
    match branch {
        Some(name) => messages.reflog().header_outcome_restore(name),
        None => messages.reflog().header_outcome_print().to_owned(),
    }
}

/// 表示用の短縮ハッシュ。
///
/// reflog は gc で失われたコミットも指し得るため、一意性のためにオブジェクトデータベースを
/// 引く `gix` の短縮（`Id::shorten()`）は使わず、先頭の固定桁数をそのまま表示する。
/// 桁数に満たない場合は全体を返す（表示専用の値であり、git へ渡すことはない）。
fn short_id(id: &str) -> &str {
    match id.char_indices().nth(SHORT_ID_LENGTH) {
        Some((offset, _)) => &id[..offset],
        None => id,
    }
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// `git reflog` と同じ `<短縮ハッシュ> HEAD@{n}: <メッセージ>` 形式で表示する。
fn display_line(entry: &ReflogEntry) -> String {
    format!(
        "{short} {selector}: {message}",
        short = short_id(&entry.id),
        selector = entry.selector(),
        message = entry.message
    )
}

/// プレビュー用の `git show` の引数を組み立てる。
fn preview_args(entry: &ReflogEntry) -> Vec<String> {
    // 末尾の `--` により、ハッシュがパスではなくリビジョンとして解釈されることを保証する
    ["show", "--color=always", &entry.id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git branch -- <name> <hash>` の引数を組み立てる。
///
/// ブランチ名はユーザー入力のため `--` の後ろへ置き、オプションとして解釈される余地を排除する。
fn branch_args(name: &str, id: &str) -> Vec<String> {
    ["branch", "--", name, id]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// reflog エントリを finder の候補へ変換する。
fn to_item(language: Language, entry: &ReflogEntry) -> FinderItem {
    FinderItem::new(
        display_line(entry),
        entry.selector(),
        PreviewSource::Git(preview_args(entry)),
        language.messages(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT_ID: &str = "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345";

    fn entry(index: usize, message: &str) -> ReflogEntry {
        ReflogEntry {
            index,
            id: COMMIT_ID.to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn a_line_shows_the_short_hash_selector_and_message() {
        assert_eq!(
            display_line(&entry(3, "checkout: moving from main to feature")),
            "1f0c9a4 HEAD@{3}: checkout: moving from main to feature"
        );
    }

    #[test]
    fn a_short_hash_keeps_the_leading_characters_of_the_full_hash() {
        assert_eq!(short_id(COMMIT_ID), "1f0c9a4");
        assert!(COMMIT_ID.starts_with(short_id(COMMIT_ID)));
    }

    #[test]
    fn a_hash_shorter_than_the_display_width_is_shown_as_is() {
        assert_eq!(short_id("1f0c9"), "1f0c9");
        assert_eq!(short_id(""), "");
    }

    #[test]
    fn the_preview_shows_the_commit_and_ends_with_a_path_separator() {
        assert_eq!(
            preview_args(&entry(0, "commit: first")),
            ["show", "--color=always", COMMIT_ID, "--"]
        );
    }

    #[test]
    fn a_branch_name_is_placed_after_the_separator() {
        assert_eq!(
            branch_args("--not-an-option", COMMIT_ID),
            ["branch", "--", "--not-an-option", COMMIT_ID]
        );
    }

    #[test]
    fn a_branch_is_created_at_the_selected_commit() {
        assert_eq!(
            branch_args("recovered", COMMIT_ID),
            ["branch", "--", "recovered", COMMIT_ID]
        );
    }

    #[test]
    fn an_item_keeps_the_selector_as_its_key() {
        // 同じコミット・同じメッセージのエントリが並び得るため、キーには位置を使う
        assert_eq!(
            to_item(Language::Japanese, &entry(12, "commit: first")).key(),
            "HEAD@{12}"
        );
    }

    #[test]
    fn the_header_names_the_branch_that_restore_would_create() {
        // `--restore` はリポジトリに参照を増やすため、既定と同じ文言では嘘になる
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();
            let restore = header_outcome(messages, Some("recovered"));

            assert!(
                restore.contains("recovered"),
                "{language:?} must name the branch: {restore}"
            );
            assert_ne!(
                restore,
                header_outcome(messages, None),
                "{language:?} must tell the two apart"
            );
        }
    }

    #[test]
    fn every_reflog_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let reflog = language.messages().reflog();

            for text in [
                reflog.header_subject(),
                reflog.header_outcome_print(),
                reflog.read_failed(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            assert!(
                reflog
                    .header_outcome_restore("recovered")
                    .contains("recovered"),
                "{language:?} must name the branch that would be created"
            );
            assert!(
                reflog.selection_not_found("HEAD@{3}").contains("HEAD@{3}"),
                "{language:?} must mention the selected entry"
            );
            assert!(
                reflog
                    .branch_creation_failed("recovered")
                    .contains("recovered"),
                "{language:?} must mention the branch"
            );

            let created = reflog.branch_created("recovered", COMMIT_ID);
            assert!(
                created.contains("recovered") && created.contains(COMMIT_ID),
                "{language:?} must mention the branch and the commit: {created}"
            );
        }
    }

    #[test]
    fn the_reflog_wording_is_translated() {
        let japanese = Language::Japanese.messages().reflog();
        let english = Language::English.messages().reflog();

        assert_ne!(japanese.header_subject(), english.header_subject());
        assert_ne!(
            japanese.header_outcome_print(),
            english.header_outcome_print()
        );
        assert_ne!(
            japanese.header_outcome_restore("recovered"),
            english.header_outcome_restore("recovered")
        );
        assert_ne!(japanese.read_failed(), english.read_failed());
        assert_ne!(
            japanese.selection_not_found("HEAD@{0}"),
            english.selection_not_found("HEAD@{0}")
        );
        assert_ne!(
            japanese.branch_creation_failed("recovered"),
            english.branch_creation_failed("recovered")
        );
        assert_ne!(
            japanese.branch_created("recovered", COMMIT_ID),
            english.branch_created("recovered", COMMIT_ID)
        );
    }
}
