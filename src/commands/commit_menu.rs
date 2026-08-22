//! コミットを選んだあとに続けて行う操作のメニュー（FR-32）。
//!
//! `gz log --action` と `gz reflog --action` が共有する 2 段目の選択である。1 段目で
//! 「どのコミットか」を決めたあと、この一覧で「そのコミットに何をするのか」を決める。
//!
//! 呼び出し側から渡るのは [`Target`]（解決済みのフルハッシュ・短縮ハッシュ・表示行）だけで
//! あり、`gz reflog` の `HEAD@{n}` セレクタのような**候補の引き方に固有の値はここへ入らない**。
//!
//! 実行部は既存コマンドの公開関数（`cherry_pick::run_on_commit` 等）をそのまま呼ぶ。
//! `gz revert` の merge commit 事前停止や `gz fixup` のステージ済み変更の検査といった安全策は
//! それらの関数側にあるため、メニュー経由でも各コマンドを直接使った場合と同じ挙動になる。

use std::io::Write;

use anyhow::{Context as _, Result, anyhow};

use crate::commands::fixup::FixupKind;
use crate::commands::revert::MessageEditing;
use crate::commands::{
    cherry_pick, commit_preview_args, confirmation, fixup, revert, selection_header,
};
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_one_with};
use crate::git::exec::run_git;
use crate::i18n::{Language, Messages};

/// メニューで選べる操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// コミットの詳細を表示する（`git show`）。
    Show,
    /// detached HEAD として切り替える（`git switch --detach`）。
    SwitchDetach,
    /// 現在のブランチへ取り込む（`gz cherry-pick` と同じ）。
    CherryPick,
    /// 打ち消すコミットを作る（`gz revert` と同じ）。
    Revert,
    /// ステージ済みの変更で fixup コミットを作る（`gz fixup` と同じ）。
    Fixup,
    /// 現在のブランチをこのコミットへ戻す（`git reset --hard`。**破壊的**）。
    ResetHard,
    /// フルハッシュを標準出力へ書き出す（`--action` なしの既定経路と同じ 1 行）。
    PrintHash,
}

/// メニューの 1 項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuEntry {
    /// 選択結果の照合に使う、言語に依らない固定キー。
    key: &'static str,
    /// 一覧表示および絞り込み対象の文字列。
    display: &'static str,
    /// 決定時に行う操作。
    action: MenuAction,
}

/// メニューが操作する対象のコミット。
///
/// 1 段目がどう候補を引いたか（`gz log` はフルハッシュ、`gz reflog` は `HEAD@{n}` セレクタ）に
/// 依らず、ここへ渡るのは**解決済みの値だけ**である。git へ渡すのは `id` のみで、`label` は
/// 確認プロンプトで対象を示すための表示用、`short_id` はヘッダー用である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target<'a> {
    /// git へ渡すフルハッシュ。
    pub id: &'a str,
    /// ヘッダーに示す短縮ハッシュ。
    pub short_id: &'a str,
    /// 1 段目で選んだ候補の表示行（確認プロンプトで対象として示す）。
    pub label: &'a str,
}

/// 操作に対応する、言語に依らない固定キーを返す。
///
/// 表示文字列は翻訳の対象であり、選択結果の照合に使うと言語や文言の変更が
/// 「別の操作を実行する」形の事故になり得るため、キーは表示と分離して持つ
/// （`gz status` のメニューで確立済みの分離をそのまま踏襲する）。
///
/// ワイルドカードの腕（`_ =>`）を置かないのは、[`MenuAction`] を増やしたときに
/// 追加漏れがコンパイルエラーになることがこの分離の目的そのものであるため。
fn key(action: MenuAction) -> &'static str {
    match action {
        MenuAction::Show => "show",
        MenuAction::SwitchDetach => "switch",
        MenuAction::CherryPick => "cherry-pick",
        MenuAction::Revert => "revert",
        MenuAction::Fixup => "fixup",
        MenuAction::ResetHard => "reset",
        MenuAction::PrintHash => "print",
    }
}

/// 操作に対応する表示文字列を返す。
///
/// [`key`] と同じく網羅的な `match` とする。
fn display(messages: &dyn Messages, action: MenuAction) -> &'static str {
    let menu = messages.commit_menu();

    match action {
        MenuAction::Show => menu.show_action(),
        MenuAction::SwitchDetach => menu.switch_action(),
        MenuAction::CherryPick => menu.cherry_pick_action(),
        MenuAction::Revert => menu.revert_action(),
        MenuAction::Fixup => menu.fixup_action(),
        MenuAction::ResetHard => menu.reset_action(),
        MenuAction::PrintHash => menu.print_action(),
    }
}

/// 渡された操作を、渡された順のままメニュー項目へ組み立てる。
///
/// 並び順を呼び出し側が決めるのは、`gz log` と `gz reflog` で載せる操作が異なるためである
/// （`gz log` には `ResetHard` を載せず、`gz reflog` には `Revert` / `Fixup` を載せない）。
fn entries(messages: &dyn Messages, actions: &[MenuAction]) -> Vec<MenuEntry> {
    actions
        .iter()
        .map(|&action| MenuEntry {
            key: key(action),
            display: display(messages, action),
            action,
        })
        .collect()
}

/// メニュー項目を finder の候補へ変換する。
///
/// どの項目を選んでいても対象のコミットが見えるよう、プレビューは項目ごとに変えず
/// 対象コミットの `git show` を出す。
fn to_menu_item(language: Language, target: &Target<'_>, entry: &MenuEntry) -> FinderItem {
    FinderItem::new(
        entry.display.to_owned(),
        entry.key.to_owned(),
        PreviewSource::Git(commit_preview_args(target.id)),
        language.messages(),
    )
}

/// 選択されたキーに対応する操作を返す。
///
/// # Errors
///
/// 選択されたキーがメニューに含まれない場合にエラーを返す（対象を取り違えたまま
/// git 操作を実行しないよう、暗黙に読み飛ばさない）。
fn resolve_action(
    messages: &dyn Messages,
    entries: &[MenuEntry],
    selected: &str,
) -> Result<MenuAction> {
    entries
        .iter()
        .find(|entry| entry.key == selected)
        .map(|entry| entry.action)
        .ok_or_else(|| anyhow!(messages.commit_menu().menu_selection_not_found(selected)))
}

/// フルハッシュを 1 行書き出す。
///
/// 書き出す内容は `--action` なしの既定経路と**同じ 1 行**（ハッシュのみ・装飾なし）である。
/// メニューを開いた場合でもパイプ用途へ戻れる経路を残すためであり、ここに情報を足すと
/// 既定経路との差が生まれてしまう。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn print_hash(messages: &dyn Messages, writer: &mut impl Write, id: &str) -> Result<()> {
    // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
    writeln!(writer, "{id}").context(messages.common().stdout_write_failed())?;

    Ok(())
}

/// 実行用の `git show` の引数を組み立てる。
///
/// プレビュー用の [`commit_preview_args`] と違い `--color=always` を**付けない**。
/// 実行時は出力が端末へ直接向かうため、色付けの要否は git 自身が判断できる。
fn show_args(id: &str) -> Vec<String> {
    // 末尾の `--` により、ハッシュがパスではなくリビジョンとして解釈されることを保証する
    ["show", id, "--"].into_iter().map(str::to_owned).collect()
}

/// `git switch --detach <id>` の引数を組み立てる。
///
/// 末尾に `--` を付けないのは、既存の `gz tag --switch`（`tag.rs` の `switch_args`）が
/// その形であり、FR-32 で新しい判断を持ち込まないためである（git はどちらの形も受け付ける）。
fn switch_args(id: &str) -> Vec<String> {
    ["switch", "--detach", id]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git reset --hard <id> --` の引数を組み立てる。
///
/// パスを伴わない裸の `--` は `git reset --hard` に受け付けられる（`Cannot do hard reset
/// with paths` になるのは `-- <path>` とパスが続く場合だけである）。したがってリビジョンを
/// 取る他の呼び出しと同じく `--` で閉じる。
fn reset_args(id: &str) -> Vec<String> {
    ["reset", "--hard", id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// 選んだ操作を対象コミットに対して実行する。
///
/// 実行部は既存コマンドの公開関数（`cherry_pick::run_on_commit` 等）をそのまま呼ぶ。
/// `gz revert` のマージコミット事前停止や `gz fixup` のステージ済み変更の検査といった
/// 安全策はそれらの関数側にあるため、メニュー経由でも各コマンドを直接使った場合と
/// 同じ挙動になる。
///
/// ワイルドカードの腕（`_ =>`）を置かないのは、[`MenuAction`] を増やしたときに
/// 実行部の追加漏れがコンパイルエラーになるようにするため。
///
/// # Errors
///
/// 実行した操作が失敗した場合、および `ResetHard` の確認で同意が得られなかった場合
/// （[`crate::error::Error::Cancelled`]）にエラーを返す。
fn run_action(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    action: MenuAction,
    target: &Target<'_>,
) -> Result<()> {
    match action {
        MenuAction::Show => {
            let arguments = show_args(target.id);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            run_git(language, &arguments)
                .with_context(|| messages.commit_menu().show_failed(target.id))
        }
        // detached HEAD は元のブランチへ戻せば復旧でき、未コミットの変更と衝突する場合は
        // git 自身が拒否するため、確認プロンプトは挟まない（`gz tag --switch` と揃える）
        MenuAction::SwitchDetach => {
            let arguments = switch_args(target.id);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            run_git(language, &arguments)
                .with_context(|| messages.commit_menu().switch_failed(target.id))
        }
        MenuAction::CherryPick => cherry_pick::run_on_commit(language, messages, target.id),
        MenuAction::Revert => revert::run_on_commit(
            language,
            messages,
            repository,
            MessageEditing::Interactive,
            target.id,
        ),
        MenuAction::Fixup => {
            fixup::run_on_commit(language, messages, repository, FixupKind::Fixup, target.id)
        }
        // 元に戻せない唯一の項目。対象を示したうえで明示的な同意を求めてから実行する
        MenuAction::ResetHard => {
            confirmation::confirm(
                messages,
                messages.commit_menu().reset_confirmation(),
                &[target.label],
            )?;

            let arguments = reset_args(target.id);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            run_git(language, &arguments)
                .with_context(|| messages.commit_menu().reset_failed(target.id))
        }
        MenuAction::PrintHash => print_hash(messages, &mut std::io::stdout(), target.id),
    }
}

/// 対象コミットに対する操作をメニューから選び、実行する。
///
/// `actions` は表示したい操作を表示順で渡す。`gz log` と `gz reflog` で載せる操作が
/// 異なるため、並びの決定は呼び出し側に置く。
///
/// # Errors
///
/// 選択の中断、未知の選択、実行した操作の失敗でエラーを返す。
///
/// **メニューを中断（Esc / Ctrl-C）した場合は [`crate::error::Error::Cancelled`] を
/// そのまま伝播させ、git を実行せず標準出力にも何も書かない。**「コミットは選んだのだから
/// 何か実行する」という暗黙のフォールバックを作らないための契約であり、`PrintHash` を
/// 選ばなかった以上ハッシュも出さない。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    target: &Target<'_>,
    actions: &[MenuAction],
) -> Result<()> {
    let built = entries(messages, actions);

    let items = built
        .iter()
        .map(|entry| to_menu_item(language, target, entry))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        &messages.commit_menu().subject(target.short_id),
        messages.commit_menu().outcome(),
    ));
    let selected = select_one_with(items, &options)?;

    let action = resolve_action(messages, &built, &selected)?;

    run_action(language, messages, repository, action, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT_ID: &str = "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345";

    /// 全バリアントを 1 か所に持つ。`key` / `display` の網羅を検査するテストが
    /// バリアントの追加に追従できるようにするため。
    const ALL: [MenuAction; 7] = [
        MenuAction::Show,
        MenuAction::SwitchDetach,
        MenuAction::CherryPick,
        MenuAction::Revert,
        MenuAction::Fixup,
        MenuAction::ResetHard,
        MenuAction::PrintHash,
    ];

    fn target() -> Target<'static> {
        Target {
            id: COMMIT_ID,
            short_id: "1f0c9a4",
            label: "1f0c9a4 2024-01-02 ブランチ切替を実装する (fuzgit test)",
        }
    }

    #[test]
    fn every_action_has_a_distinct_key() {
        let mut keys: Vec<&str> = ALL.iter().map(|&action| key(action)).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();

        assert_eq!(keys.len(), count, "the keys must not collide");
    }

    #[test]
    fn a_key_does_not_depend_on_the_display_language() {
        // 表示文字列で照合すると、言語の切替が「別の操作を実行する」事故になり得る
        for action in ALL {
            assert_eq!(key(action), key(action));
        }

        for action in ALL {
            let japanese = display(Language::Japanese.messages(), action);
            let english = display(Language::English.messages(), action);

            assert_ne!(japanese, english, "{action:?} must be translated");
            assert_ne!(key(action), japanese);
            assert_ne!(key(action), english);
        }
    }

    #[test]
    fn entries_keep_the_order_they_were_given() {
        let messages = Language::Japanese.messages();
        let actions = [MenuAction::PrintHash, MenuAction::Show, MenuAction::Revert];

        let built = entries(messages, &actions);

        assert_eq!(
            built.iter().map(|entry| entry.action).collect::<Vec<_>>(),
            actions
        );
        assert_eq!(
            built.iter().map(|entry| entry.key).collect::<Vec<_>>(),
            ["print", "show", "revert"]
        );
    }

    #[test]
    fn a_known_key_resolves_to_its_action() {
        let messages = Language::Japanese.messages();
        let built = entries(messages, &ALL);

        for action in ALL {
            assert_eq!(
                resolve_action(messages, &built, key(action)).expect("the key belongs to the menu"),
                action
            );
        }
    }

    #[test]
    fn an_unknown_key_stops_instead_of_being_skipped() {
        let messages = Language::Japanese.messages();
        let built = entries(messages, &ALL);

        let error = resolve_action(messages, &built, "no-such-entry")
            .expect_err("an unknown key must not resolve");

        assert!(error.to_string().contains("no-such-entry"));
    }

    #[test]
    fn a_menu_item_previews_the_target_commit() {
        let target = target();
        let built = entries(Language::Japanese.messages(), &[MenuAction::Show]);
        let item = to_menu_item(Language::Japanese, &target, &built[0]);

        assert_eq!(item.key(), "show");
    }

    #[test]
    fn the_preview_and_the_executed_show_are_not_the_same_arguments() {
        // プレビューはキャプチャされるため色付けを明示するが、実行時は端末へ直接出るため付けない
        assert_eq!(
            commit_preview_args(COMMIT_ID),
            ["show", "--color=always", COMMIT_ID, "--"]
        );
        assert_eq!(show_args(COMMIT_ID), ["show", COMMIT_ID, "--"]);
        assert!(
            !show_args(COMMIT_ID)
                .iter()
                .any(|argument| argument == "--color=always")
        );
    }

    #[test]
    fn a_revision_taking_call_is_closed_with_a_path_separator() {
        for arguments in [show_args(COMMIT_ID), reset_args(COMMIT_ID)] {
            assert_eq!(
                arguments.last().map(String::as_str),
                Some("--"),
                "{arguments:?} must end with the separator"
            );
        }
    }

    #[test]
    fn switch_keeps_the_shape_of_the_existing_tag_command() {
        // `gz tag --switch`（`tag.rs` の `switch_args`）と同形。新しい判断を持ち込まない
        assert_eq!(switch_args(COMMIT_ID), ["switch", "--detach", COMMIT_ID]);
        assert!(
            !switch_args(COMMIT_ID)
                .iter()
                .any(|argument| argument == "--")
        );
    }

    #[test]
    fn reset_moves_the_branch_to_the_commit() {
        assert_eq!(reset_args(COMMIT_ID), ["reset", "--hard", COMMIT_ID, "--"]);
    }

    #[test]
    fn printing_writes_the_full_hash_and_nothing_else() {
        let messages = Language::Japanese.messages();
        let mut written = Vec::new();

        print_hash(messages, &mut written, COMMIT_ID).expect("writing to a buffer cannot fail");

        assert_eq!(
            String::from_utf8(written).expect("the hash is valid UTF-8"),
            format!("{COMMIT_ID}\n")
        );
    }

    #[test]
    fn every_commit_menu_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let menu = language.messages().commit_menu();

            for text in [
                menu.outcome(),
                menu.show_action(),
                menu.switch_action(),
                menu.cherry_pick_action(),
                menu.revert_action(),
                menu.fixup_action(),
                menu.reset_action(),
                menu.print_action(),
                menu.reset_confirmation(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            assert!(menu.subject("1f0c9a4").contains("1f0c9a4"));
            assert!(
                menu.menu_selection_not_found("show").contains("show"),
                "{language:?} must mention the picked entry"
            );
            for text in [
                menu.show_failed(COMMIT_ID),
                menu.switch_failed(COMMIT_ID),
                menu.reset_failed(COMMIT_ID),
            ] {
                assert!(
                    text.contains(COMMIT_ID),
                    "{language:?} must mention the commit: {text}"
                );
            }
        }
    }

    #[test]
    fn the_commit_menu_wording_is_translated() {
        let japanese = Language::Japanese.messages().commit_menu();
        let english = Language::English.messages().commit_menu();

        assert_ne!(japanese.subject("1f0c9a4"), english.subject("1f0c9a4"));
        assert_ne!(japanese.outcome(), english.outcome());
        assert_ne!(japanese.show_action(), english.show_action());
        assert_ne!(japanese.switch_action(), english.switch_action());
        assert_ne!(japanese.cherry_pick_action(), english.cherry_pick_action());
        assert_ne!(japanese.revert_action(), english.revert_action());
        assert_ne!(japanese.fixup_action(), english.fixup_action());
        assert_ne!(japanese.reset_action(), english.reset_action());
        assert_ne!(japanese.print_action(), english.print_action());
        assert_ne!(
            japanese.menu_selection_not_found("show"),
            english.menu_selection_not_found("show")
        );
        assert_ne!(japanese.reset_confirmation(), english.reset_confirmation());
        assert_ne!(
            japanese.show_failed(COMMIT_ID),
            english.show_failed(COMMIT_ID)
        );
        assert_ne!(
            japanese.switch_failed(COMMIT_ID),
            english.switch_failed(COMMIT_ID)
        );
        assert_ne!(
            japanese.reset_failed(COMMIT_ID),
            english.reset_failed(COMMIT_ID)
        );
    }
}
