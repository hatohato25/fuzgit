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
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use skim::prelude::*;

use crate::error::{Error, Result};
use crate::git::exec::capture_git;

/// ファイル内容のプレビューで読み込む最大バイト数。
///
/// 巨大なファイルを選択した際にプレビュー生成で待たされないよう上限を設ける。
const FILE_PREVIEW_LIMIT: usize = 64 * 1024;

/// 内容が上限で打ち切られたことを示す注記。
const TRUNCATION_NOTICE: &str = "\n… （以降は省略しました）";

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
    /// ローカルファイルの内容をそのまま表示する。
    ///
    /// 未追跡ファイルは git の管理下に無く差分を取れないため、内容を直接読んで表示する。
    /// カレントディレクトリに依存しないよう絶対パスを渡すこと。
    File(PathBuf),
}

/// ファイルの先頭を [`FILE_PREVIEW_LIMIT`] バイトまで読み、表示用の文字列を返す。
///
/// 内容が UTF-8 でない場合（バイナリファイル等）も選択操作を止めたくないため、
/// ここではロッシー変換を行う。表示のみに使う値であり、git へ渡す値ではない。
fn read_preview(path: &Path) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    // 打ち切りを検出するために上限より 1 バイト多く読む
    std::fs::File::open(path)?
        .take(FILE_PREVIEW_LIMIT as u64 + 1)
        .read_to_end(&mut buffer)?;

    let truncated = buffer.len() > FILE_PREVIEW_LIMIT;
    buffer.truncate(FILE_PREVIEW_LIMIT);

    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if truncated {
        text.push_str(TRUNCATION_NOTICE);
    }
    Ok(text)
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
            PreviewSource::File(path) => match read_preview(path) {
                Ok(text) => ItemPreview::Text(text),
                Err(err) => ItemPreview::Text(format!(
                    "{path} を読み込めません: {err}",
                    path = path.display()
                )),
            },
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

/// finder の起動オプション。
///
/// 選択モードのみで足りる呼び出しには [`select_one`] / [`select_many`] を使い、
/// ヘッダーや事前選択が必要な場合にこの型を [`run_finder_with`] / [`select_many_with`] へ渡す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderOptions {
    /// 単一選択か複数選択か。
    pub mode: SelectionMode,
    /// 候補リストとは別に固定表示する見出し（操作説明・凡例など）。
    pub header: Option<String>,
    /// 起動時に選択済みにする候補の表示文字列（[`FinderItem`] の `display`）。
    ///
    /// 判定は表示文字列の**完全一致**であり、キー（`FinderItem::key`）ではない。
    /// また skim は複数選択モードでのみ事前選択を適用するため、
    /// [`SelectionMode::Single`] では無視される。
    /// 適用されるのは各候補につき 1 度だけで、ユーザーが外した選択は復活しない
    /// （[`InitialSelector`] を参照）。
    pub preselect: Vec<String>,
}

impl FinderOptions {
    /// 選択モードだけを指定したオプションを作る。
    #[must_use]
    pub fn new(mode: SelectionMode) -> Self {
        Self {
            mode,
            header: None,
            preselect: Vec::new(),
        }
    }

    /// 固定表示する見出しを設定する。
    #[must_use]
    pub fn with_header(mut self, header: String) -> Self {
        self.header = Some(header);
        self
    }

    /// 起動時に選択済みにする候補の表示文字列を設定する。
    #[must_use]
    pub fn with_preselect(mut self, preselect: Vec<String>) -> Self {
        self.preselect = preselect;
        self
    }
}

/// 各候補を一度だけ事前選択するセレクタ。
///
/// skim は候補の絞り込みが走るたびにセレクタを再適用する（`tui/item_list.rs`。
/// `SkimOptions.selector` を直接指定した場合、再適用の上限は `usize::MAX` = 実質無制限）。
/// 表示文字列の一致だけで判定すると、ユーザーが Tab で外した事前選択がクエリ入力のたびに
/// 復活し、外したはずのファイルがコミット対象に戻ってしまう（実機で確認済み）。
/// そのため同じ表示文字列に対しては最初の 1 回だけ真を返し、事前選択を「起動時の初期値」に留める。
struct InitialSelector {
    /// まだ事前選択していない候補の表示文字列。
    ///
    /// skim は単一スレッドから（`Rc<dyn Selector>` として）呼び出すため `RefCell` で足りる。
    remaining: RefCell<HashSet<String>>,
}

impl InitialSelector {
    /// 事前選択する表示文字列からセレクタを作る。
    fn new(preselect: &[String]) -> Self {
        Self {
            remaining: RefCell::new(preselect.iter().cloned().collect()),
        }
    }
}

impl Selector for InitialSelector {
    fn should_select(&self, _index: usize, item: &dyn SkimItem) -> bool {
        // 判定はキーではなく表示文字列で行う（skim が渡すのは `SkimItem::text()`）
        self.remaining.borrow_mut().remove(item.text().as_ref())
    }
}

/// skim のオプションを組み立てる。
///
/// # Errors
///
/// ビルダーが必須項目を満たさない場合に [`Error::FinderFailed`] を返す。
fn build_options(options: &FinderOptions) -> Result<SkimOptions> {
    let mut builder = SkimOptionsBuilder::default();
    builder
        .multi(options.mode == SelectionMode::Multi)
        .reverse(true)
        // skim はプレビュー用のグローバルコマンドが未設定だとプレビュー枠自体を描画せず、
        // SkimItem::preview() も呼ばない。ここではアイテム側が常に AnsiText / Text を返すため、
        // グローバルコマンドは空文字（実行されないダミー）で足りる
        .preview("");

    if let Some(header) = &options.header {
        builder.header(header.clone());
    }

    let mut built = builder.build().map_err(|err| Error::FinderFailed {
        message: err.to_string(),
    })?;

    // skim は複数選択モードでしか事前選択を適用しない（`tui/item_list.rs`）。
    // 単一選択で渡された場合に別の意味へ倒すことはせず、そのまま無視する
    if options.mode == SelectionMode::Multi && !options.preselect.is_empty() {
        built.selector = Some(Rc::new(InitialSelector::new(&options.preselect)));
    }

    Ok(built)
}

/// fuzzy finder を起動し、選択された候補の [`FinderItem::key`] を返す。
///
/// # Errors
///
/// - 候補が空の場合は [`Error::NoCandidates`]
/// - Esc / Ctrl-C で中断された場合は [`Error::Cancelled`]（呼び出し側は git 操作を実行しないこと）
/// - skim の初期化・実行に失敗した場合は [`Error::FinderFailed`]
pub fn run_finder(items: Vec<FinderItem>, mode: SelectionMode) -> Result<Vec<String>> {
    run_finder_with(items, &FinderOptions::new(mode))
}

/// [`run_finder`] と同じだが、ヘッダー・事前選択を含む [`FinderOptions`] を受け取る。
///
/// # Errors
///
/// [`run_finder`] と同じ。
pub fn run_finder_with(items: Vec<FinderItem>, options: &FinderOptions) -> Result<Vec<String>> {
    if items.is_empty() {
        return Err(Error::NoCandidates);
    }

    let options = build_options(options)?;

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
    select_many_with(items, &FinderOptions::new(SelectionMode::Multi))
}

/// [`select_many`] と同じだが、ヘッダー・事前選択を含む [`FinderOptions`] を受け取る。
///
/// # Errors
///
/// [`select_many`] と同じ。
pub fn select_many_with(items: Vec<FinderItem>, options: &FinderOptions) -> Result<Vec<String>> {
    let selected = run_finder_with(items, options)?;

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

    /// プレビュー生成の呼び出しに必要な最小限のコンテキスト。
    fn preview_context() -> PreviewContext<'static> {
        PreviewContext {
            query: "",
            cmd_query: "",
            width: 80,
            height: 24,
            current_index: 0,
            current_selection: "",
            selected_indices: &[],
            selections: &[],
        }
    }

    #[test]
    fn preview_of_a_non_previewable_item_is_empty_text() {
        let item = FinderItem::new("main".to_string(), "main".to_string(), PreviewSource::None);

        match item.preview(preview_context()) {
            ItemPreview::Text(text) => assert!(text.is_empty()),
            _ => panic!("PreviewSource::None must produce empty text"),
        }
    }

    #[test]
    fn a_file_preview_shows_the_contents_of_the_file() {
        let dir = crate::test_support::TempDir::new("finder-file-preview");
        crate::test_support::write_file(dir.path(), "untracked.txt", "line 1\nline 2\n");

        let text = read_preview(&dir.path().join("untracked.txt"))
            .expect("an existing file should be readable");

        assert_eq!(text, "line 1\nline 2\n");
    }

    #[test]
    fn a_large_file_preview_is_truncated_with_a_notice() {
        let dir = crate::test_support::TempDir::new("finder-file-preview-large");
        let contents = "x".repeat(FILE_PREVIEW_LIMIT + 100);
        crate::test_support::write_file(dir.path(), "large.txt", &contents);

        let text =
            read_preview(&dir.path().join("large.txt")).expect("a large file should be readable");

        assert_eq!(text.len(), FILE_PREVIEW_LIMIT + TRUNCATION_NOTICE.len());
        assert!(text.ends_with(TRUNCATION_NOTICE), "the notice is missing");
    }

    #[test]
    fn a_missing_file_is_reported_in_the_preview_instead_of_failing() {
        let dir = crate::test_support::TempDir::new("finder-file-preview-missing");
        let missing = dir.path().join("missing.txt");
        let item = FinderItem::new(
            "missing.txt".to_string(),
            "missing.txt".to_string(),
            PreviewSource::File(missing.clone()),
        );

        match item.preview(preview_context()) {
            ItemPreview::Text(text) => assert!(
                text.contains(&missing.display().to_string()),
                "the path should be named: {text}"
            ),
            _ => panic!("a file preview must produce plain text"),
        }
    }

    #[test]
    fn options_enable_the_preview_pane_and_reflect_the_selection_mode() {
        let single = build_options(&FinderOptions::new(SelectionMode::Single))
            .expect("options should build");
        assert!(!single.multi);
        // preview がセットされていないと skim は SkimItem::preview() を呼ばない
        assert!(single.preview.is_some());
        assert!(single.reverse);

        let multi =
            build_options(&FinderOptions::new(SelectionMode::Multi)).expect("options should build");
        assert!(multi.multi);
    }

    #[test]
    fn options_have_no_header_and_no_selector_by_default() {
        let options =
            build_options(&FinderOptions::new(SelectionMode::Multi)).expect("options should build");

        assert!(options.header.is_none());
        assert!(options.selector.is_none());
    }

    #[test]
    fn a_header_is_passed_to_skim() {
        let options = build_options(
            &FinderOptions::new(SelectionMode::Multi).with_header("Tab で選択".to_string()),
        )
        .expect("options should build");

        assert_eq!(options.header.as_deref(), Some("Tab で選択"));
    }

    #[test]
    fn preselected_items_install_a_selector_in_multi_mode() {
        let options = build_options(
            &FinderOptions::new(SelectionMode::Multi)
                .with_preselect(vec!["M  src/main.rs".to_string()]),
        )
        .expect("options should build");

        let selector = options
            .selector
            .expect("multi mode should install a selector");
        let staged = FinderItem::new(
            "M  src/main.rs".to_string(),
            "src/main.rs".to_string(),
            PreviewSource::None,
        );
        let unstaged = FinderItem::new(
            " M src/main.rs".to_string(),
            "src/main.rs".to_string(),
            PreviewSource::None,
        );

        // 判定はキーではなく表示文字列の完全一致で行われる
        assert!(selector.should_select(0, &staged));
        assert!(!selector.should_select(1, &unstaged));
    }

    #[test]
    fn an_item_is_preselected_only_once() {
        // skim は絞り込みのたびにセレクタを再適用する。2 回目以降も真を返すと、
        // ユーザーが外した事前選択がクエリ入力のたびに復活してしまう
        let selector = InitialSelector::new(&["M  src/main.rs".to_string()]);
        let staged = FinderItem::new(
            "M  src/main.rs".to_string(),
            "src/main.rs".to_string(),
            PreviewSource::None,
        );

        assert!(selector.should_select(0, &staged));
        assert!(!selector.should_select(0, &staged));
        assert!(!selector.should_select(3, &staged), "位置が変わっても同じ");
    }

    #[test]
    fn an_item_outside_the_preselection_is_never_selected() {
        let selector = InitialSelector::new(&["M  src/main.rs".to_string()]);
        let other = FinderItem::new(
            "?? notes.txt".to_string(),
            "notes.txt".to_string(),
            PreviewSource::None,
        );

        assert!(!selector.should_select(0, &other));
        assert!(!selector.should_select(1, &other));
    }

    #[test]
    fn every_preselected_item_is_selected_regardless_of_the_order_they_arrive_in() {
        // 候補は複数回に分けて skim へ渡される場合があるため、位置ではなく表示文字列で判定する
        let selector = InitialSelector::new(&["a".to_string(), "b".to_string()]);
        let items = ["b", "a"].map(|display| {
            FinderItem::new(
                display.to_string(),
                display.to_string(),
                PreviewSource::None,
            )
        });

        assert!(selector.should_select(5, &items[0]));
        assert!(selector.should_select(9, &items[1]));
    }

    #[test]
    fn preselected_items_are_ignored_in_single_mode() {
        // skim 自体が単一選択モードでは事前選択を適用しないため、セレクタも渡さない
        let options = build_options(
            &FinderOptions::new(SelectionMode::Single).with_preselect(vec!["main".to_string()]),
        )
        .expect("options should build");

        assert!(options.selector.is_none());
    }

    #[test]
    fn run_finder_with_rejects_empty_candidates_without_starting_the_tui() {
        let err = run_finder_with(
            Vec::new(),
            &FinderOptions::new(SelectionMode::Multi).with_preselect(vec!["a".to_string()]),
        )
        .expect_err("empty candidate list must not start the finder");

        assert!(matches!(err, Error::NoCandidates));
    }

    #[test]
    fn select_many_with_rejects_empty_candidates_without_starting_the_tui() {
        let err = select_many_with(Vec::new(), &FinderOptions::new(SelectionMode::Multi))
            .expect_err("empty candidate list must not start the finder");

        assert!(matches!(err, Error::NoCandidates));
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
