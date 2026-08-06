//! `skim` を用いた選択 UI の共通レイヤー。
//!
//! 各サブコマンドは候補を [`FinderItem`] へ整形し、[`run_finder`] で
//! 絞り込み・プレビュー・選択を行う。
//!
//! # テストについて
//!
//! [`run_finder`] は端末（TUI）を占有するため自動テストの対象外とし、手動確認とする。
//! 自動テストは [`FinderItem`] の整形・プレビュー引数組み立てなど、
//! 端末に依存しない純ロジックに限定する。

use std::borrow::Cow;

use skim::prelude::*;

use crate::error::{Error, Result};
use crate::git::exec::capture_git;

/// 候補ごとのプレビュー内容の生成方法。
///
/// skim のプレビューには「シェルコマンド文字列を渡す」方式（`ItemPreview::Command`）も
/// あるが、候補文字列由来のインジェクションを構造的に排除するため採用しない。
/// 代わりに git の引数配列を保持し、Rust 側で実行した結果を
/// [`ItemPreview::AnsiText`] として返す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewSource {
    /// プレビューを表示しない。
    None,
    /// `git <args>` の出力を ANSI カラー付きで表示する。
    Git(Vec<String>),
}

/// fuzzy finder に渡す汎用の候補アイテム。
#[derive(Debug, Clone)]
pub struct FinderItem {
    /// 一覧表示および絞り込み対象となる文字列。
    display: String,
    /// 決定時に呼び出し側へ返す値（ブランチ名・コミットハッシュ・パス等）。
    key: String,
    /// プレビュー生成方法。
    preview: PreviewSource,
}

impl FinderItem {
    /// 候補アイテムを生成する。
    #[must_use]
    pub fn new(display: String, key: String, preview: PreviewSource) -> Self {
        Self {
            display,
            key,
            preview,
        }
    }

    /// 決定時に返される値を取得する。
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl SkimItem for FinderItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.key)
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        match &self.preview {
            PreviewSource::None => ItemPreview::Text(String::new()),
            PreviewSource::Git(args) => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                match capture_git(&args) {
                    Ok(stdout) => {
                        ItemPreview::AnsiText(String::from_utf8_lossy(&stdout).into_owned())
                    }
                    // プレビュー失敗で選択操作全体を中断させたくないため、
                    // エラー内容をプレビュー領域に表示するに留める
                    Err(err) => ItemPreview::Text(err.to_string()),
                }
            }
        }
    }
}

/// 選択モード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// 1 件のみ選択する。
    Single,
    /// 複数件を選択できる（cherry-pick / add / restore 用）。
    Multi,
}

/// skim のオプションを組み立てる。
///
/// # Errors
///
/// ビルダーが必須項目を満たさない場合に [`Error::FinderFailed`] を返す。
fn build_options(mode: SelectionMode) -> Result<SkimOptions> {
    SkimOptionsBuilder::default()
        .multi(mode == SelectionMode::Multi)
        .reverse(true)
        // skim はプレビュー用のグローバルコマンドが未設定だとプレビュー枠自体を描画せず、
        // SkimItem::preview() も呼ばない。ここではアイテム側が常に AnsiText / Text を返すため、
        // グローバルコマンドは空文字（実行されないダミー）で足りる
        .preview("")
        .build()
        .map_err(|err| Error::FinderFailed {
            message: err.to_string(),
        })
}

/// fuzzy finder を起動し、選択された候補の [`FinderItem::key`] を返す。
///
/// # Errors
///
/// - 候補が空の場合は [`Error::NoCandidates`]
/// - Esc / Ctrl-C で中断された場合は [`Error::Cancelled`]（呼び出し側は git 操作を実行しないこと）
/// - skim の初期化・実行に失敗した場合は [`Error::FinderFailed`]
pub fn run_finder(items: Vec<FinderItem>, mode: SelectionMode) -> Result<Vec<String>> {
    if items.is_empty() {
        return Err(Error::NoCandidates);
    }

    let options = build_options(mode)?;

    let output = Skim::run_items(options, items).map_err(|err| Error::FinderFailed {
        message: err.to_string(),
    })?;

    if output.is_abort {
        return Err(Error::Cancelled);
    }

    Ok(output
        .selected_items
        .iter()
        .map(|matched| matched.item.output().into_owned())
        .collect())
}

/// fuzzy finder を単一選択モードで起動し、選択された 1 件のキーを返す。
///
/// # Errors
///
/// - 候補が空の場合は [`Error::NoCandidates`]
/// - 中断された場合、および 1 件も選ばれずに決定された場合は [`Error::Cancelled`]
/// - skim の初期化・実行に失敗した場合は [`Error::FinderFailed`]
pub fn select_one(items: Vec<FinderItem>) -> Result<String> {
    let mut selected = run_finder(items, SelectionMode::Single)?;

    // クエリに一致する候補が無い状態で Enter を押した場合、skim は中断扱いにせず
    // 空の選択結果を返す。何も選ばれていない以上、後続の git 操作は行わない
    if selected.is_empty() {
        return Err(Error::Cancelled);
    }

    Ok(selected.swap_remove(0))
}

/// fuzzy finder を複数選択モードで起動し、選択された 1 件以上のキーを返す。
///
/// # Errors
///
/// - 候補が空の場合は [`Error::NoCandidates`]
/// - 中断された場合、および 1 件も選ばれずに決定された場合は [`Error::Cancelled`]
/// - skim の初期化・実行に失敗した場合は [`Error::FinderFailed`]
pub fn select_many(items: Vec<FinderItem>) -> Result<Vec<String>> {
    let selected = run_finder(items, SelectionMode::Multi)?;

    // 単一選択と同様に、何も選ばれていない状態での決定は git 操作を行わずに終了する
    if selected.is_empty() {
        return Err(Error::Cancelled);
    }

    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> FinderItem {
        FinderItem::new(
            "* main".to_string(),
            "main".to_string(),
            PreviewSource::Git(vec!["log".to_string(), "--oneline".to_string()]),
        )
    }

    #[test]
    fn text_is_the_display_string_and_output_is_the_key() {
        let item = item();
        assert_eq!(item.text(), "* main");
        assert_eq!(item.output(), "main");
        assert_eq!(item.key(), "main");
    }

    #[test]
    fn preview_of_a_non_previewable_item_is_empty_text() {
        let item = FinderItem::new("main".to_string(), "main".to_string(), PreviewSource::None);
        let context = PreviewContext {
            query: "",
            cmd_query: "",
            width: 80,
            height: 24,
            current_index: 0,
            current_selection: "main",
            selected_indices: &[],
            selections: &[],
        };

        match item.preview(context) {
            ItemPreview::Text(text) => assert!(text.is_empty()),
            _ => panic!("PreviewSource::None must produce empty text"),
        }
    }

    #[test]
    fn options_enable_the_preview_pane_and_reflect_the_selection_mode() {
        let single = build_options(SelectionMode::Single).expect("options should build");
        assert!(!single.multi);
        // preview がセットされていないと skim は SkimItem::preview() を呼ばない
        assert!(single.preview.is_some());
        assert!(single.reverse);

        let multi = build_options(SelectionMode::Multi).expect("options should build");
        assert!(multi.multi);
    }

    #[test]
    fn run_finder_rejects_empty_candidates_without_starting_the_tui() {
        let err = run_finder(Vec::new(), SelectionMode::Single)
            .expect_err("empty candidate list must not start the finder");

        assert!(matches!(err, Error::NoCandidates));
    }

    #[test]
    fn select_one_rejects_empty_candidates_without_starting_the_tui() {
        let err =
            select_one(Vec::new()).expect_err("empty candidate list must not start the finder");

        assert!(matches!(err, Error::NoCandidates));
    }

    #[test]
    fn select_many_rejects_empty_candidates_without_starting_the_tui() {
        let err =
            select_many(Vec::new()).expect_err("empty candidate list must not start the finder");

        assert!(matches!(err, Error::NoCandidates));
    }
}
