//! `gz tag` — タグを選択して出力・切り替え・差分表示する（FR-7）。

use std::io::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::run_git;
use crate::git::read::{TagInfo, tags};

/// 選択したタグに対して行う操作。
///
/// `--switch` / `--diff` の真偽値を持ち回らず、3 通りの操作を型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagAction {
    /// タグ名を標準出力へ書き出す（既定）。
    Print,
    /// タグの指すコミットへ detached HEAD で切り替える（`--switch`）。
    Switch,
    /// タグと HEAD の差分を表示する（`--diff`）。
    Diff,
}

impl TagAction {
    /// `--switch` / `--diff` の指定から操作を決める。
    ///
    /// 排他性は `clap` の `conflicts_with` でも担保しているが、両方が立った状態を
    /// 暗黙にどちらか一方へ倒すことがないよう、ここでも明示的に拒否する。
    ///
    /// # Errors
    ///
    /// `--switch` と `--diff` が同時に指定された場合にエラーを返す。
    pub fn from_flags(switch: bool, diff: bool) -> Result<Self> {
        match (switch, diff) {
            (false, false) => Ok(Self::Print),
            (true, false) => Ok(Self::Switch),
            (false, true) => Ok(Self::Diff),
            (true, true) => bail!("`--switch` と `--diff` は同時に指定できません"),
        }
    }
}

/// タグを 1 件選び、[`TagAction`] に応じた処理を行う。
///
/// # Errors
///
/// タグ一覧の取得、選択（中断を含む）、標準出力への書き込み、`git` の実行に失敗した場合に
/// エラーを返す。
pub fn run(repository: &gix::Repository, action: TagAction) -> Result<()> {
    let candidates = tags(repository).context("タグ一覧の取得に失敗しました")?;

    let items = candidates.iter().map(to_item).collect();
    let selected = select_one(items)?;

    let tag = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!("選択されたタグ `{selected}` が候補に見つかりません"))?;

    match action {
        TagAction::Print => {
            // パイプ利用を想定し、stdout にはタグ名以外を混ぜない。
            // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
            writeln!(std::io::stdout(), "{name}", name = tag.name)
                .context("標準出力への書き込みに失敗しました")?;
        }
        TagAction::Switch => {
            let arguments = switch_args(tag);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            run_git(&arguments).with_context(|| {
                format!("タグ `{name}` への切り替えに失敗しました", name = tag.name)
            })?;
        }
        TagAction::Diff => {
            let arguments = diff_args(tag);
            let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
            run_git(&arguments).with_context(|| {
                format!("タグ `{name}` との差分表示に失敗しました", name = tag.name)
            })?;
        }
    }

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// 名前での絞り込みが主用途のため名前を先頭に置き、annotated tag はメッセージを添える。
fn display_line(tag: &TagInfo) -> String {
    match &tag.message {
        Some(message) => format!("{name}  {message}", name = tag.name),
        None => tag.name.clone(),
    }
}

/// プレビュー用の `git show` の引数を組み立てる。
///
/// 参照が直接指すオブジェクトの ID を渡す。annotated tag ではタグオブジェクトを指すため、
/// タグのメッセージに続けて対象コミットの情報と差分が表示される。
fn preview_args(tag: &TagInfo) -> Vec<String> {
    // 末尾の `--` により、ID がパスではなくリビジョンとして解釈されることを保証する
    ["show", "--color=always", &tag.id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git switch --detach <id>` の引数を組み立てる。
///
/// タグ名ではなく解決済みの ID を渡し、名前がオプションとして解釈される余地を排除する
/// （annotated tag のタグオブジェクトは git 側でコミットまで peel される）。
fn switch_args(tag: &TagInfo) -> Vec<String> {
    ["switch", "--detach", &tag.id]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git diff <id> HEAD --` の引数を組み立てる。
///
/// FR-7 の「HEAD との差分」を作業ツリーの状態に左右されず示すため、比較先を HEAD で明示する。
fn diff_args(tag: &TagInfo) -> Vec<String> {
    // 末尾の `--` により、ID がパスではなくリビジョンとして解釈されることを保証する
    ["diff", &tag.id, "HEAD", "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// タグを finder の候補へ変換する。
fn to_item(tag: &TagInfo) -> FinderItem {
    FinderItem::new(
        display_line(tag),
        tag.name.clone(),
        PreviewSource::Git(preview_args(tag)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG_OBJECT_ID: &str = "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345";

    fn lightweight() -> TagInfo {
        TagInfo {
            name: "v2.0".to_owned(),
            id: TAG_OBJECT_ID.to_owned(),
            message: None,
        }
    }

    fn annotated() -> TagInfo {
        TagInfo {
            message: Some("リリース v1.0".to_owned()),
            name: "v1.0".to_owned(),
            id: TAG_OBJECT_ID.to_owned(),
        }
    }

    #[test]
    fn no_flag_prints_the_tag_name() {
        assert_eq!(
            TagAction::from_flags(false, false).expect("no flag is always valid"),
            TagAction::Print
        );
    }

    #[test]
    fn the_switch_flag_selects_switch() {
        assert_eq!(
            TagAction::from_flags(true, false).expect("--switch alone is valid"),
            TagAction::Switch
        );
    }

    #[test]
    fn the_diff_flag_selects_diff() {
        assert_eq!(
            TagAction::from_flags(false, true).expect("--diff alone is valid"),
            TagAction::Diff
        );
    }

    #[test]
    fn combining_switch_and_diff_is_rejected_instead_of_favouring_one() {
        let err = TagAction::from_flags(true, true)
            .expect_err("--switch and --diff must not be combined silently");

        assert!(
            err.to_string().contains("--switch") && err.to_string().contains("--diff"),
            "both options should be named: {err:#}"
        );
    }

    #[test]
    fn a_lightweight_tag_is_shown_by_its_name_alone() {
        assert_eq!(display_line(&lightweight()), "v2.0");
    }

    #[test]
    fn an_annotated_tag_shows_its_message_after_the_name() {
        assert_eq!(display_line(&annotated()), "v1.0  リリース v1.0");
    }

    #[test]
    fn the_preview_shows_the_tagged_object_and_ends_with_a_path_separator() {
        assert_eq!(
            preview_args(&annotated()),
            ["show", "--color=always", TAG_OBJECT_ID, "--"]
        );
    }

    #[test]
    fn switching_detaches_head_at_the_resolved_id() {
        assert_eq!(
            switch_args(&annotated()),
            ["switch", "--detach", TAG_OBJECT_ID]
        );
    }

    #[test]
    fn the_diff_compares_the_tag_against_head() {
        assert_eq!(
            diff_args(&annotated()),
            ["diff", TAG_OBJECT_ID, "HEAD", "--"]
        );
    }

    #[test]
    fn an_item_keeps_the_tag_name_as_its_key() {
        assert_eq!(to_item(&annotated()).key(), "v1.0");
    }
}
