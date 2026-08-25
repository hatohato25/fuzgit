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

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use skim::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::error::{Error, Result};
use crate::git::exec::{capture_git_display, capture_git_display_in};
use crate::i18n::Messages;

/// ファイル内容のプレビューで読み込む最大バイト数。
///
/// 巨大なファイルを選択した際にプレビュー生成で待たされないよう上限を設ける。
const FILE_PREVIEW_LIMIT: usize = 64 * 1024;

/// [`PreviewSource::Composite`] のセクション見出しの前置き。
const SECTION_HEADING_PREFIX: &str = "── ";

/// [`PreviewSource::Composite`] のセクション見出しの後置き。
const SECTION_HEADING_SUFFIX: &str = " ──";

/// [`PreviewSource::Composite`] のセクション同士の区切り（見出しの前に空行を 1 行入れる）。
const SECTION_SEPARATOR: &str = "\n\n";

/// 枠（[`PreviewPanel`]）を描くために最低限必要なプレビュー幅。
///
/// これを下回る幅では罫線も右寄せも意味を成さず、本文の表示領域を削るだけになるため
/// **枠を出さない**。幅が取れない端末で枠を優先しないという明示の決めであり、
/// 枠を半端に描くことも、幅を仮定することもしない。
const PANEL_MIN_WIDTH: usize = 20;

/// 主キー（候補の表示文字列）と右端の指標の間に最低限空ける桁数。
const PANEL_METRIC_GAP: usize = 1;

/// 指標を残すために主キーを削ってよい下限の表示幅。
///
/// これを下回るところまで主キーが削られる場合は、指標の方を落として主キーを残す。
/// 「いま何を見ているのか」が読めないプレビューには、指標だけあっても意味が無いため。
const PANEL_MIN_IDENT_WIDTH: usize = 8;

/// 幅に収まらなかった行の末尾に置く省略記号。
const PANEL_ELLIPSIS: &str = "…";

/// 枠の罫線に用いる文字。
const PANEL_RULE: char = '─';

/// 枠の 2 行目で項目同士を区切る記号。
///
/// [`crate::commands::COLUMN_SEPARATOR`] と同じく**装飾であって文言ではない**ため、
/// 言語ごとの文言（`crate::i18n`）ではなくここに持つ。
const PANEL_CONTEXT_SEPARATOR: &str = " · ";

/// ANSI: 装飾をすべて解除する。
const ANSI_RESET: &str = "\u{1b}[0m";

/// ANSI: 前景を暗くする（枠の 2 行目と罫線）。
///
/// 色番号ではなく装飾（faint）を用いるのは、端末テーマの前景色をそのまま弱めるため。
/// 「暗い灰色」を色番号で指定すると、明るいテーマで読めなくなる組み合わせが生じる。
const ANSI_DIM: &str = "\u{1b}[2m";

/// ANSI: エスケープ文字そのものを表示するための代替文字（U+241B SYMBOL FOR ESCAPE）。
const ESCAPE_SYMBOL: char = '␛';

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
    ///
    /// 実行はプロセスのカレントディレクトリで行われる（＝現在のリポジトリ）。
    /// 別のリポジトリを対象にする場合は [`PreviewSource::GitIn`] を使う。
    Git(Vec<String>),
    /// `directory` をカレントディレクトリとして `git <args>` を実行し、その出力を表示する。
    ///
    /// 現在のリポジトリ以外（`gz fetch --siblings` の兄弟リポジトリ）のプレビューに用いる。
    /// ディレクトリは cwd として渡し、**引数配列には載せない**（`git -C <path>` 方式を採らない）。
    /// パスをコマンドの引数へ埋め込まないという既存方針を保つため。
    GitIn {
        /// git を実行するディレクトリ。カレントディレクトリに依存しないよう絶対パスを渡すこと。
        directory: PathBuf,
        /// `git` に渡す引数配列。
        args: Vec<String>,
    },
    /// ローカルファイルの内容をそのまま表示する。
    ///
    /// 未追跡ファイルは git の管理下に無く差分を取れないため、内容を直接読んで表示する。
    /// カレントディレクトリに依存しないよう絶対パスを渡すこと。
    File(PathBuf),
    /// あらかじめ取得済みの文字列をそのまま表示する。
    ///
    /// **プレビューで外部コマンドを起動しないための variant である。**`gz pr` は候補取得の
    /// 1 回で PR の本文まで取り切り、プレビューはその文字列を描く（design.md
    /// 「FR-34 / FR-35 の設計」）。`gh` を実行する variant を**作らない**ことで、
    /// 「プレビューはネットワークを使わない」という設計原則を型のレベルで担保する。
    ///
    /// 内容は fuzgit の管理外（GitHub 上の他人が書いた文字列）であるため、
    /// [`PreviewSource::File`] と同じくエスケープ文字を無害化してから描画する。
    Text(String),
    /// ラベル付きの複数ソースを 1 つのプレビューへ連結する。
    ///
    /// `gz status` の「staged / unstaged」のように、1 つの候補について複数の観点を
    /// 並べて見せるために用いる。各ソースの実行は従来どおり選択項目ごとの遅延生成であり、
    /// 呼び出し側が必要なセクションだけを持たせることで余計な git 実行を避ける。
    /// 出力が空になったセクションは見出しごと省略する（[`render_composite`] を参照）。
    Composite(Vec<(String, PreviewSource)>),
}

/// `git <args>` を実行してプレビュー本文を得る。
///
/// プレビュー本文は fuzgit が解釈せずそのまま画面へ描画されるため、git の実行は
/// **(B) 系**（[`capture_git_display`]）で行い、解決された表示言語を子プロセスへ伝える。
///
/// 失敗した場合も選択操作を止めず、表示用のメッセージを `Err` として返す
/// （呼び出し側がプレビュー領域へ出す）。
fn render_git(messages: &dyn Messages, args: &[String]) -> std::result::Result<String, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    capture_git_display(messages.language(), &args)
        .map(|stdout| String::from_utf8_lossy(&stdout).into_owned())
        .map_err(|err| err.to_string())
}

/// `directory` をカレントディレクトリとして `git <args>` を実行し、プレビュー本文を得る。
///
/// [`render_git`] と同じく (B) 系で実行し、失敗しても選択操作を止めず表示用のメッセージを
/// `Err` として返す。
fn render_git_in(
    messages: &dyn Messages,
    directory: &Path,
    args: &[String],
) -> std::result::Result<String, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    capture_git_display_in(messages.language(), directory, &args)
        .map(|stdout| String::from_utf8_lossy(&stdout).into_owned())
        .map_err(|err| err.to_string())
}

/// ファイルの内容を読んでプレビュー本文を得る。
///
/// 読み取れない場合も選択操作を止めず、表示用のメッセージを `Err` として返す。
fn render_file(messages: &dyn Messages, path: &Path) -> std::result::Result<String, String> {
    read_preview(messages, path).map_err(|err| messages.finder().file_read_failed(path, &err))
}

/// セクションの見出し行を組み立てる。
fn section_heading(label: &str) -> String {
    format!("{SECTION_HEADING_PREFIX}{label}{SECTION_HEADING_SUFFIX}")
}

/// 1 つのソースからプレビュー本文（ANSI エスケープを含み得る）を組み立てる。
///
/// 失敗したソースはエラーメッセージを本文として返す。連結表示の一部が取れなかった
/// ことを利用者に見せるためであり、失敗を無かったことにはしない。
fn render(messages: &dyn Messages, source: &PreviewSource) -> String {
    match source {
        PreviewSource::None => String::new(),
        PreviewSource::Git(args) => render_git(messages, args).unwrap_or_else(|message| message),
        PreviewSource::GitIn { directory, args } => {
            render_git_in(messages, directory, args).unwrap_or_else(|message| message)
        }
        PreviewSource::File(path) => render_file(messages, path).unwrap_or_else(|message| message),
        // 取得済みの文字列は fuzgit の管理外の内容を含み得るため、そのまま ANSI として
        // 解釈させない（[`escape_ansi`] を参照。`File` と同じ扱い）
        PreviewSource::Text(text) => escape_ansi(text),
        PreviewSource::Composite(sections) => render_composite(messages, sections),
    }
}

/// ラベル付きの複数ソースを 1 つのプレビュー本文へ連結する。
///
/// 本文が空（空白のみを含む）になったセクションは見出しごと省略する。
/// 該当しない観点（staged の変更が無いファイルの「staged」など）の見出しだけが並ぶと、
/// プレビューの限られた表示領域が意味の無い行で埋まるため。
fn render_composite(messages: &dyn Messages, sections: &[(String, PreviewSource)]) -> String {
    sections
        .iter()
        .filter_map(|(label, source)| {
            let body = render(messages, source);
            if body.trim().is_empty() {
                return None;
            }

            Some(format!(
                "{heading}\n{body}",
                heading = section_heading(label),
                body = body.trim_end_matches('\n')
            ))
        })
        .collect::<Vec<String>>()
        .join(SECTION_SEPARATOR)
}

/// ファイルの先頭を [`FILE_PREVIEW_LIMIT`] バイトまで読み、表示用の文字列を返す。
///
/// 内容が UTF-8 でない場合（バイナリファイル等）も選択操作を止めたくないため、
/// ここではロッシー変換を行う。表示のみに使う値であり、git へ渡す値ではない。
///
/// 打ち切りが起きた場合は、その旨の注記（[`crate::i18n::messages::FinderMessages`]）を
/// 末尾へ添えるため `messages` を受け取る。
fn read_preview(messages: &dyn Messages, path: &Path) -> std::io::Result<String> {
    let mut buffer = Vec::new();
    // 打ち切りを検出するために上限より 1 バイト多く読む
    std::fs::File::open(path)?
        .take(FILE_PREVIEW_LIMIT as u64 + 1)
        .read_to_end(&mut buffer)?;

    let truncated = buffer.len() > FILE_PREVIEW_LIMIT;
    buffer.truncate(FILE_PREVIEW_LIMIT);

    let mut text = String::from_utf8_lossy(&buffer).into_owned();
    if truncated {
        // 注記は本文の続きではないため改行で区切る。区切りは装飾であり文言ではないので
        // ここで付け、[`FinderMessages::truncation_notice`] は注記そのものだけを返す
        text.push('\n');
        text.push_str(messages.finder().truncation_notice());
    }
    Ok(text)
}

/// 候補の表示文字列の一部に付ける前景色。
///
/// 端末のテーマ（配色設定）へ追随させるため、扱うのは基本 16 色（ANSI）のみとし、
/// 256 色 (`Color::Indexed`) や truecolor (`Color::Rgb`) は仮定しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightColor {
    /// 緑（ANSI 32）。
    Green,
    /// 赤（ANSI 31）。
    Red,
    /// 黄（ANSI 33）。
    Yellow,
    /// 青（ANSI 34）。
    Blue,
    /// 前景を暗くする（ANSI 2 / faint）。
    ///
    /// **色ではなく装飾である。**「重要でない」「まだ本番ではない」ことを、目立たせずに
    /// 示すために用いる（`gz pr` の draft な PR 番号）。色番号の灰
    /// （`gh` は 256 色の 242 を使う）を採らないのは [`ANSI_DIM`] と同じ理由で、
    /// 明るいテーマで読めなくなる組み合わせが生じるためである。
    Dim,
}

impl HighlightColor {
    /// 候補一覧の描画で用いる装飾を、既存の装飾へ重ねる。
    ///
    /// 前景色を差し替えるものと、[`HighlightColor::Dim`] のように**装飾を足す**ものが
    /// あるため、色を返すのではなく `Style` の変換として持つ。
    fn apply(self, style: Style) -> Style {
        match self {
            Self::Green => style.fg(Color::Green),
            Self::Red => style.fg(Color::Red),
            Self::Yellow => style.fg(Color::Yellow),
            Self::Blue => style.fg(Color::Blue),
            // 色ではなく装飾。前景色は端末テーマのまま弱める
            Self::Dim => style.add_modifier(Modifier::DIM),
        }
    }

    /// プレビュー本文へ埋め込む ANSI の前景色指定へ変換する。
    ///
    /// 候補一覧は ratatui の `Style` で描かれるのに対し、プレビューは
    /// [`ItemPreview::AnsiText`] として渡す文字列であり、色の伝え方が異なる。
    /// **同じ [`HighlightColor`] から両方を導く**ことで、一覧の色と枠 1 行目の色が
    /// 食い違わないようにする。
    fn to_ansi(self) -> &'static str {
        match self {
            Self::Green => "\u{1b}[32m",
            Self::Red => "\u{1b}[31m",
            Self::Yellow => "\u{1b}[33m",
            Self::Blue => "\u{1b}[34m",
            Self::Dim => ANSI_DIM,
        }
    }
}

/// 表示文字列のうち一部分を色付けする指定。
///
/// 範囲は [`FinderItem`] の表示文字列に対するバイト位置の半開区間 `[start, end)`。
/// 絞り込み対象の文字列（[`SkimItem::text`]）そのものは変えず、描画時にだけ色を乗せるため、
/// 色を持たせても絞り込み・事前選択の挙動は変わらない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    /// 色を付ける範囲の開始バイト位置（この位置を含む）。
    start: usize,
    /// 色を付ける範囲の終端バイト位置（この位置を含まない）。
    end: usize,
    /// 前景色。
    color: HighlightColor,
}

impl Highlight {
    /// 色付けする範囲を作る。
    #[must_use]
    pub fn new(start: usize, end: usize, color: HighlightColor) -> Self {
        Self { start, end, color }
    }

    /// バイト位置 `index` がこの範囲に含まれるか。
    fn contains(self, index: usize) -> bool {
        self.start <= index && index < self.end
    }

    /// バイト範囲 `[start, end)` と重なりを持つか。
    fn overlaps(self, start: usize, end: usize) -> bool {
        self.start < end && start < self.end
    }
}

/// `highlights` で指定された範囲の前景色を `line` へ乗せる。
///
/// `base_style` は [`DisplayContext::base_style`]、すなわち「クエリにマッチしていない部分」の
/// 装飾。マッチ部分は `base_style` に `matched_style` が重ねられた別の装飾を持つため、
/// **装飾が `base_style` のままの span にだけ**色を乗せ、マッチのハイライトは書き換えない
/// （利用者が今入力しているクエリへの反応の方が、状態コードの色より優先度が高いため）。
fn apply_highlights<'a>(line: Line<'a>, base_style: Style, highlights: &[Highlight]) -> Line<'a> {
    if highlights.is_empty() {
        return line;
    }

    let mut spans: Vec<Span<'a>> = Vec::with_capacity(line.spans.len());
    let mut offset = 0;
    for span in line.spans {
        let length = span.content.len();
        if span.style == base_style {
            spans.extend(split_span(span, offset, highlights));
        } else {
            spans.push(span);
        }
        offset += length;
    }

    // span 以外（行全体の装飾・寄せ）は skim が決めたものをそのまま保つ
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

/// 1 つの span を、色の指定ごとの span へ分割する。
///
/// `offset` は行頭から数えたこの span の開始バイト位置。分割位置は文字単位で走査して求める
/// （バイト位置で直接スライスすると、マルチバイト文字の途中で切って panic し得るため）。
fn split_span<'a>(span: Span<'a>, offset: usize, highlights: &[Highlight]) -> Vec<Span<'a>> {
    if !highlights
        .iter()
        .any(|highlight| highlight.overlaps(offset, offset + span.content.len()))
    {
        return vec![span];
    }

    let mut pieces: Vec<Span<'a>> = Vec::new();
    let mut text = String::new();
    let mut current: Option<HighlightColor> = None;
    for (index, character) in span.content.char_indices() {
        let color = highlights
            .iter()
            .find(|highlight| highlight.contains(offset + index))
            .map(|highlight| highlight.color);
        if color != current && !text.is_empty() {
            pieces.push(Span::styled(
                std::mem::take(&mut text),
                highlighted_style(span.style, current),
            ));
        }
        current = color;
        text.push(character);
    }
    if !text.is_empty() {
        pieces.push(Span::styled(text, highlighted_style(span.style, current)));
    }

    pieces
}

/// 前景色だけを差し替えた装飾を返す。色の指定が無い部分は元の装飾のまま。
fn highlighted_style(style: Style, color: Option<HighlightColor>) -> Style {
    match color {
        None => style,
        Some(color) => color.apply(style),
    }
}

/// プレビューの先頭へ固定表示する枠。
///
/// どのコマンドでも同じ位置に同じ意味の行が来るようにするための型。構成は
///
/// 1. **主キーと指標** — 候補の表示文字列（[`FinderItem::display`]）をそのまま再掲し、
///    右端へ指標を右寄せする
/// 2. **文脈** — [`PANEL_CONTEXT_SEPARATOR`] で連ねた補足（空なら行ごと省く）
/// 3. **罫線** — ここまでが枠、ここからが git の出力という境目
///
/// 1 行目の主キーは**この型が持たない**。一覧の行をそのまま再掲するのが決めであり、
/// 別に持たせると一覧と枠が食い違い得るため、描画時に [`FinderItem`] から受け取る。
///
/// 枠の下に続く本文（[`PreviewSource`]）には手を入れない。git の出力をそのまま見せる
/// という既存の性質を保つための分担であり、枠は本文の**上に乗るだけ**である。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewPanel {
    /// 1 行目の右端へ右寄せする指標。無い場合は右端を空ける。
    metric: Option<String>,
    /// `metric` のうち色を付ける範囲（バイト位置）。
    metric_highlights: Vec<Highlight>,
    /// 2 行目に並べる補足。空文字の項目は描画時に取り除く。
    context: Vec<String>,
}

impl PreviewPanel {
    /// 指標も文脈も持たない枠（1 行目と罫線だけ）を作る。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 1 行目の右端へ右寄せする指標を設定する。
    ///
    /// `highlights` は `metric` に対するバイト位置であり、一覧の色付け
    /// （[`FinderItem::with_highlights`]）と同じ仕組みで指定する。
    #[must_use]
    pub fn with_metric(mut self, metric: String, highlights: Vec<Highlight>) -> Self {
        self.metric = Some(metric);
        self.metric_highlights = highlights;
        self
    }

    /// 2 行目に並べる補足を設定する。
    ///
    /// 取れなかった項目は**空文字ではなく要素ごと落として**渡してよい。空文字の項目は
    /// 区切り記号だけが並ぶことになるため、描画時に取り除く。
    #[must_use]
    pub fn with_context(mut self, context: Vec<String>) -> Self {
        self.context = context;
        self
    }
}

/// 枠を組み立てる。返る文字列は末尾に改行を含まない。
///
/// `width` は skim が渡すプレビュー領域の幅（[`PreviewContext::width`]）。
/// [`PANEL_MIN_WIDTH`] を下回る場合は枠を出さず `None` を返す。
fn render_panel(
    display: &str,
    highlights: &[Highlight],
    panel: &PreviewPanel,
    width: usize,
) -> Option<String> {
    if width < PANEL_MIN_WIDTH {
        return None;
    }

    let mut lines = vec![render_ident(display, highlights, panel, width)];
    if let Some(context) = render_context(&panel.context, width) {
        lines.push(context);
    }
    lines.push(render_rule(width));

    Some(lines.join("\n"))
}

/// 枠の 1 行目（主キーと右寄せの指標）を組み立てる。
///
/// 幅に収まらない場合は**主キーの末尾**を省略して指標を残す。ただし主キーが
/// [`PANEL_MIN_IDENT_WIDTH`] を下回るところまで削られる場合は、指標の方を落とす。
fn render_ident(
    display: &str,
    highlights: &[Highlight],
    panel: &PreviewPanel,
    width: usize,
) -> String {
    let metric = panel.metric.as_deref().filter(|metric| !metric.is_empty());
    let reserved = metric.map_or(0, |metric| metric.width() + PANEL_METRIC_GAP);

    // 指標を置く余地があるかを先に決める。`width` は PANEL_MIN_WIDTH 以上であるため、
    // 指標を落とした場合の主キーの幅は必ず PANEL_MIN_IDENT_WIDTH を上回る
    let (limit, metric) = match width.checked_sub(reserved) {
        Some(limit) if limit >= PANEL_MIN_IDENT_WIDTH => (limit, metric),
        _ => (width, None),
    };

    let (kept, truncated) = fit(display, limit);
    let mut line = colorize(&kept, highlights);
    let mut used = kept.width();
    if truncated {
        // 省略記号は候補の文字ではないため、一覧の色は乗せない
        line.push_str(PANEL_ELLIPSIS);
        used += PANEL_ELLIPSIS.width();
    }

    let Some(metric) = metric else {
        return line;
    };

    line.push_str(&" ".repeat(width.saturating_sub(used + metric.width())));
    line.push_str(&colorize(metric, &panel.metric_highlights));

    line
}

/// 枠の 2 行目（文脈）を組み立てる。項目が 1 つも無い場合は行ごと省く。
fn render_context(context: &[String], width: usize) -> Option<String> {
    let items: Vec<&str> = context
        .iter()
        .map(String::as_str)
        .filter(|item| !item.trim().is_empty())
        .collect();
    if items.is_empty() {
        return None;
    }

    let (kept, truncated) = fit(&items.join(PANEL_CONTEXT_SEPARATOR), width);
    let ellipsis = if truncated { PANEL_ELLIPSIS } else { "" };

    Some(format!("{ANSI_DIM}{kept}{ellipsis}{ANSI_RESET}"))
}

/// 枠の罫線を組み立てる。
fn render_rule(width: usize) -> String {
    let rule: String = std::iter::repeat_n(PANEL_RULE, width).collect();

    format!("{ANSI_DIM}{rule}{ANSI_RESET}")
}

/// 表示幅が `limit` に収まる先頭部分と、切り詰めが起きたかどうかを返す。
///
/// 幅は文字数ではなく端末上の表示幅で測る（[`crate::commands`] の列揃えと同じ理由で、
/// 全角文字を 1 桁と数えると桁が合わない）。切り詰めが起きる場合は
/// [`PANEL_ELLIPSIS`] の分だけ手前で止めるため、呼び出し側が省略記号を足すと
/// ちょうど `limit` に収まる。
fn fit(text: &str, limit: usize) -> (String, bool) {
    if text.width() <= limit {
        return (text.to_owned(), false);
    }

    let budget = limit.saturating_sub(PANEL_ELLIPSIS.width());
    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        // 幅を持たない文字（結合文字等）はそのまま残す。落とすと表示が壊れるため
        let width = character.width().unwrap_or(0);
        if used + width > budget {
            break;
        }
        kept.push(character);
        used += width;
    }

    (kept, true)
}

/// `highlights` で指定された範囲へ ANSI の前景色を乗せた文字列を返す。
///
/// 範囲は `text` に対するバイト位置であり、[`apply_highlights`]（候補一覧側）と同じ値を
/// そのまま使える。`text` が切り詰められている場合でも、切り詰めるのは末尾だけであるため
/// 先頭からのバイト位置はずれない。
fn colorize(text: &str, highlights: &[Highlight]) -> String {
    if highlights.is_empty() {
        return text.to_owned();
    }

    let mut painted = String::with_capacity(text.len());
    let mut current: Option<HighlightColor> = None;
    for (index, character) in text.char_indices() {
        let color = highlights
            .iter()
            .find(|highlight| highlight.contains(index))
            .map(|highlight| highlight.color);
        if color != current {
            // 色が乗っていない範囲の前に解除を挟むと、無意味なエスケープ列が並ぶ
            if current.is_some() {
                painted.push_str(ANSI_RESET);
            }
            if let Some(color) = color {
                painted.push_str(color.to_ansi());
            }
            current = color;
        }
        painted.push(character);
    }
    if current.is_some() {
        painted.push_str(ANSI_RESET);
    }

    painted
}

/// 本文に含まれるエスケープ文字を表示用の記号へ置き換える。
///
/// 枠は ANSI として渡すため、本文も同じ扱いになる。git の出力は fuzgit 自身が
/// `--color=always` を付けて色付けさせたものだが、**ローカルファイルの内容は
/// fuzgit の管理外**であり、たまたま含まれたエスケープ列が装飾として解釈されると
/// 表示が壊れる。読み取り上限での打ち切りと同じく「表示のためだけの加工」であり、
/// git へ渡す値には一切影響しない。
fn escape_ansi(text: &str) -> String {
    text.replace('\u{1b}', &ESCAPE_SYMBOL.to_string())
}

/// fuzzy finder に渡す汎用の候補アイテム。
#[derive(Debug, Clone)]
pub struct FinderItem {
    /// 一覧表示および絞り込み対象となる文字列。
    display: String,
    /// 決定時に呼び出し側へ返す値（ブランチ名・コミットハッシュ・パス等）。
    ///
    /// **決して翻訳しない。**この値は候補一覧との照合検証を経て git の引数になるため、
    /// 言語によって変わると検証も git 実行も壊れる。
    key: String,
    /// プレビュー生成方法。
    preview: PreviewSource,
    /// プレビュー生成に用いる文言一式（＝表示言語）。
    ///
    /// [`SkimItem::preview`] は skim から呼ばれるため引数を足せない。したがって
    /// アイテム自身が言語を持つ必要がある。**必須フィールドであり `Option` にはしない**
    /// （未設定を許すと、その場合に何語で出すかという暗黙のフォールバックが生まれるため）。
    /// 実体はフィールドを持たない ZST への `&'static` 参照であり、`Clone` のコストは変わらない。
    messages: &'static dyn Messages,
    /// 表示文字列のうち色を付ける範囲。色を付けない候補では空。
    ///
    /// 色は描画時（[`SkimItem::display`]）にだけ乗せ、絞り込み対象の文字列は
    /// [`FinderItem::display`] のままにしておく。
    highlights: Vec<Highlight>,
    /// プレビュー本文の先頭へ乗せる枠。`None` の場合は本文だけを出す。
    ///
    /// **既定は枠あり**（指標も文脈も無い、1 行目と罫線だけの枠）。1 行目は表示文字列を
    /// 再掲するだけで追加の読み取りを要さないため、どのコマンドでも同じ枠が出ることを
    /// 既定に置ける。指標・文脈を足す場合は [`FinderItem::with_panel`] を使う。
    panel: Option<PreviewPanel>,
}

impl FinderItem {
    /// 候補アイテムを生成する。
    ///
    /// `messages` には解決済みの表示言語（`language.messages()`）を渡す。
    /// プレビューで実行する git は (B) 系となり、この言語が子プロセスへ伝播する。
    #[must_use]
    pub fn new(
        display: String,
        key: String,
        preview: PreviewSource,
        messages: &'static dyn Messages,
    ) -> Self {
        Self {
            display,
            key,
            preview,
            messages,
            highlights: Vec::new(),
            panel: Some(PreviewPanel::new()),
        }
    }

    /// 表示文字列の一部に付ける色を設定する。
    #[must_use]
    pub fn with_highlights(mut self, highlights: Vec<Highlight>) -> Self {
        self.highlights = highlights;
        self
    }

    /// プレビューの先頭へ乗せる枠を設定する。
    ///
    /// 既定でも枠は出る（[`FinderItem::new`]）。指標や文脈を足す場合にだけ呼べばよい。
    #[must_use]
    pub fn with_panel(mut self, panel: PreviewPanel) -> Self {
        self.panel = Some(panel);
        self
    }

    /// プレビューの先頭へ枠を乗せない。
    ///
    /// 本文そのものが枠と同じ情報を先頭に持つ場合など、再掲が重複にしかならない候補で用いる。
    #[must_use]
    pub fn without_panel(mut self) -> Self {
        self.panel = None;
        self
    }

    /// 決定時に返される値を取得する。
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// この候補が使うプレビューの取得元。
    ///
    /// **プレビューが外部コマンドを起動しないことをテストで固定する**ために公開する
    /// （`gz pr` は取得済み本文の描画に閉じる。[`PreviewSource::Text`] を参照）。
    #[cfg(test)]
    pub fn preview_source(&self) -> &PreviewSource {
        &self.preview
    }

    /// プレビュー本文を組み立てる。返る文字列は ANSI として解釈される前提。
    ///
    /// 失敗しても選択操作を止めず、表示用のメッセージを本文として返す
    /// （[`render`] と同じ扱い）。
    fn preview_body(&self) -> String {
        match &self.preview {
            PreviewSource::None => String::new(),
            // ローカルファイルの内容は fuzgit の管理外であり、たまたま含まれた
            // エスケープ列を装飾として解釈させない（[`escape_ansi`] を参照）
            PreviewSource::File(path) => {
                escape_ansi(&render_file(self.messages, path).unwrap_or_else(|message| message))
            }
            // `Text` も管理外の内容だが、無害化は `render` 側で行う（二重に掛けない）
            source => render(self.messages, source),
        }
    }
}

impl SkimItem for FinderItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display)
    }

    fn display(&self, context: DisplayContext) -> Line<'_> {
        // `to_line` は context を消費するため、先に控えておく
        let base_style = context.base_style;

        apply_highlights(context.to_line(self.text()), base_style, &self.highlights)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.key)
    }

    fn preview(&self, context: PreviewContext) -> ItemPreview {
        // 本文を持たない候補には枠も出さない。プレビューを出さないという意思表示であり、
        // 見出しだけの枠を出しても示せることが無いため
        if self.preview == PreviewSource::None {
            return ItemPreview::Text(String::new());
        }

        let body = self.preview_body();
        let Some(panel) = &self.panel else {
            return ItemPreview::AnsiText(body);
        };

        // 枠を描く幅が無い端末では本文を優先する（[`render_panel`] を参照）
        match render_panel(&self.display, &self.highlights, panel, context.width) {
            Some(header) => ItemPreview::AnsiText(format!("{header}\n{body}")),
            None => ItemPreview::AnsiText(body),
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
        // カーソル行を行末まで塗る。既定では文字のある範囲しか色が変わらず、
        // どの行にカーソルがあるのか候補が詰まっているほど分かりにくい。
        // **これはカーソル行にだけ効く**（skim の `tui/item_renderer.rs` は
        // `highlight_line && is_current` で判定する）。Tab で選択した行の見え方は変わらない
        .highlight_line(true)
        // マーカー列と本文の間に余白を作る。skim は [selector 列][marker 列][本文] を
        // 区切り無しで並べるため（`tui/item_renderer.rs`）、既定の ">" のままだと
        // `>>mike   origin/main` のようにマーカーと本文がくっついて読みにくい。
        // 余白専用のオプションは無く、本文の行頭に空白を足すと列揃えや事前選択の一致に
        // 影響するため、マーカーの字形は変えずに末尾へ空白を 1 つ足す。
        // marker が描画されない行でも同じ幅の空白が確保されるので、単一選択も含め
        // 全コマンドで接頭辞が 3 桁に揃う
        .multi_select_icon("> ")
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
    select_one_with(items, &FinderOptions::new(SelectionMode::Single))
}

/// [`select_one`] と同じだが、ヘッダーを含む [`FinderOptions`] を受け取る。
///
/// 単一選択を複数回続けて行う場合（`gz diff --branch` / `--commit` の base と対象）に、
/// 今どちらを選んでいるのかをヘッダーで示すために用いる。
/// [`select_many_with`] と同様、[`FinderOptions::mode`] は呼び出し側が指定する
/// （[`SelectionMode::Multi`] を渡した場合、返るのは選択のうち 1 件だけになる）。
///
/// # Errors
///
/// [`select_one`] と同じ。
pub fn select_one_with(items: Vec<FinderItem>, options: &FinderOptions) -> Result<String> {
    let mut selected = run_finder_with(items, options)?;

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
    use crate::i18n::Language;

    fn item() -> FinderItem {
        FinderItem::new(
            "* main".to_string(),
            "main".to_string(),
            PreviewSource::Git(vec!["log".to_string(), "--oneline".to_string()]),
            Language::Japanese.messages(),
        )
    }

    #[test]
    fn text_is_the_display_string_and_output_is_the_key() {
        let item = item();
        assert_eq!(item.text(), "* main");
        assert_eq!(item.output(), "main");
        assert_eq!(item.key(), "main");
    }

    /// マッチしていない部分の装飾（skim が渡す `base_style` 相当）。
    fn base_style() -> Style {
        Style::default()
    }

    /// マッチ部分の装飾（`base_style` と区別できるよう別の色を与える）。
    fn matched_style() -> Style {
        Style::default().fg(Color::Blue)
    }

    /// 行を「本文, 前景色」の組へ落として比較しやすくする。
    fn spans(line: &Line<'_>) -> Vec<(String, Option<Color>)> {
        line.spans
            .iter()
            .map(|span| (span.content.to_string(), span.style.fg))
            .collect()
    }

    #[test]
    fn a_highlight_splits_the_span_it_covers_partially() {
        let line = Line::from(vec![Span::styled("M  src/main.rs", base_style())]);

        let highlighted = apply_highlights(
            line,
            base_style(),
            &[Highlight::new(0, 1, HighlightColor::Green)],
        );

        assert_eq!(
            spans(&highlighted),
            [
                ("M".to_string(), Some(Color::Green)),
                ("  src/main.rs".to_string(), None),
            ]
        );
    }

    #[test]
    fn two_highlights_colour_their_own_ranges() {
        let line = Line::from(vec![Span::styled("MM src/main.rs", base_style())]);

        let highlighted = apply_highlights(
            line,
            base_style(),
            &[
                Highlight::new(0, 1, HighlightColor::Green),
                Highlight::new(1, 2, HighlightColor::Red),
            ],
        );

        assert_eq!(
            spans(&highlighted),
            [
                ("M".to_string(), Some(Color::Green)),
                ("M".to_string(), Some(Color::Red)),
                (" src/main.rs".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_highlight_may_start_after_the_beginning_of_the_line() {
        let line = Line::from(vec![Span::styled(" M src/main.rs", base_style())]);

        let highlighted = apply_highlights(
            line,
            base_style(),
            &[Highlight::new(1, 2, HighlightColor::Red)],
        );

        assert_eq!(
            spans(&highlighted),
            [
                (" ".to_string(), None),
                ("M".to_string(), Some(Color::Red)),
                (" src/main.rs".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_line_without_highlights_is_left_untouched() {
        let line = Line::from(vec![Span::styled("?? new.txt", base_style())]);

        let highlighted = apply_highlights(line.clone(), base_style(), &[]);

        assert_eq!(highlighted, line);
    }

    #[test]
    fn a_matched_span_keeps_the_match_highlighting() {
        // skim が「M」をクエリのマッチとして色付けした状態
        let line = Line::from(vec![
            Span::styled("M", base_style().patch(matched_style())),
            Span::styled("  src/main.rs", base_style()),
        ]);

        let highlighted = apply_highlights(
            line,
            base_style(),
            &[Highlight::new(0, 1, HighlightColor::Green)],
        );

        assert_eq!(
            spans(&highlighted),
            [
                ("M".to_string(), matched_style().fg),
                ("  src/main.rs".to_string(), None),
            ],
            "クエリへのマッチ表示は状態コードの色より優先する"
        );
    }

    #[test]
    fn a_highlight_after_a_matched_span_is_still_applied() {
        // 先頭がマッチ済みでも、続く base_style の部分には色が乗る
        let line = Line::from(vec![
            Span::styled("M", base_style().patch(matched_style())),
            Span::styled("M src/main.rs", base_style()),
        ]);

        let highlighted = apply_highlights(
            line,
            base_style(),
            &[
                Highlight::new(0, 1, HighlightColor::Green),
                Highlight::new(1, 2, HighlightColor::Red),
            ],
        );

        assert_eq!(
            spans(&highlighted),
            [
                ("M".to_string(), matched_style().fg),
                ("M".to_string(), Some(Color::Red)),
                (" src/main.rs".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_multibyte_character_is_not_split_in_the_middle() {
        // 状態コードは ASCII だが、範囲がマルチバイト文字に掛かっても壊れないことを保つ
        let line = Line::from(vec![Span::styled("??日本語.txt", base_style())]);

        let highlighted = apply_highlights(
            line,
            base_style(),
            &[Highlight::new(0, 3, HighlightColor::Red)],
        );

        assert_eq!(
            spans(&highlighted),
            [
                ("??日".to_string(), Some(Color::Red)),
                ("本語.txt".to_string(), None),
            ],
            "文字の途中では分割せず、その文字ごと色を付ける"
        );
    }

    #[test]
    fn the_displayed_line_is_coloured_but_the_matching_text_is_not() {
        let item = FinderItem::new(
            "M  src/main.rs".to_string(),
            "src/main.rs".to_string(),
            PreviewSource::None,
            Language::Japanese.messages(),
        )
        .with_highlights(vec![Highlight::new(0, 1, HighlightColor::Green)]);

        let context = DisplayContext {
            base_style: base_style(),
            matched_style: matched_style(),
            ..DisplayContext::default()
        };

        assert_eq!(
            spans(&item.display(context)),
            [
                ("M".to_string(), Some(Color::Green)),
                ("  src/main.rs".to_string(), None),
            ]
        );
        assert_eq!(
            item.text(),
            "M  src/main.rs",
            "絞り込みと事前選択の対象となる文字列は色を含めない"
        );
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

    /// 幅を指定してプレビュー生成のコンテキストを作る。
    fn preview_context_of_width(width: usize) -> PreviewContext<'static> {
        PreviewContext {
            width,
            ..preview_context()
        }
    }

    /// 枠だけを取り出す（本文は罫線の次の行から始まる）。
    fn panel_lines(text: &str) -> Vec<String> {
        text.lines()
            .take_while(|line| !line.contains(PANEL_RULE))
            .map(str::to_owned)
            .collect()
    }

    /// 枠付きのプレビューを組み立てて本文を返す。
    fn preview_text(item: &FinderItem, width: usize) -> String {
        match item.preview(preview_context_of_width(width)) {
            ItemPreview::AnsiText(text) => text,
            _ => panic!("a framed preview must be ANSI text"),
        }
    }

    /// 本文が固定のファイルを持つ候補を作る。
    fn framed_item(dir: &crate::test_support::TempDir, display: &str) -> FinderItem {
        FinderItem::new(
            display.to_string(),
            "key".to_string(),
            text_section(dir, "body.txt", "body line\n"),
            Language::Japanese.messages(),
        )
    }

    #[test]
    fn the_panel_repeats_the_candidate_line_and_rules_it_off_from_the_body() {
        let dir = crate::test_support::TempDir::new("panel-ident");
        let text = preview_text(&framed_item(&dir, "  feature/login"), 40);

        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("  feature/login"),
            "1 行目は候補の表示文字列をそのまま再掲する"
        );
        let rule = lines.next().expect("罫線が続く");
        assert!(rule.contains(PANEL_RULE), "本文との境目は罫線: {rule:?}");
        assert_eq!(lines.next(), Some("body line"), "罫線の次から本文");
    }

    #[test]
    fn the_metric_is_flushed_to_the_right_edge_of_the_preview() {
        let dir = crate::test_support::TempDir::new("panel-metric");
        let item = framed_item(&dir, "main")
            .with_panel(PreviewPanel::new().with_metric("↑3 ↓0".to_string(), Vec::new()));

        let ident = preview_text(&item, 20)
            .lines()
            .next()
            .expect("枠の 1 行目")
            .to_owned();

        assert_eq!(ident.width(), 20, "1 行目は幅いっぱいに広がる: {ident:?}");
        assert!(ident.starts_with("main"), "主キーは左端: {ident:?}");
        assert!(ident.ends_with("↑3 ↓0"), "指標は右端: {ident:?}");
    }

    #[test]
    fn a_candidate_line_too_wide_for_the_preview_is_truncated_so_the_metric_survives() {
        let dir = crate::test_support::TempDir::new("panel-truncate");
        let item = framed_item(&dir, "feature/very-long-branch-name-that-will-not-fit")
            .with_panel(PreviewPanel::new().with_metric("+48".to_string(), Vec::new()));

        let ident = preview_text(&item, 24)
            .lines()
            .next()
            .expect("枠の 1 行目")
            .to_owned();

        assert_eq!(ident.width(), 24, "幅を超えない: {ident:?}");
        assert!(
            ident.starts_with("feature/"),
            "主キーの先頭は残る: {ident:?}"
        );
        assert!(
            ident.contains(PANEL_ELLIPSIS),
            "削った位置を省略記号で示す: {ident:?}"
        );
        assert!(ident.ends_with("+48"), "指標は残る: {ident:?}");
    }

    #[test]
    fn a_metric_that_would_crowd_out_the_candidate_line_is_dropped_instead() {
        let dir = crate::test_support::TempDir::new("panel-metric-dropped");
        let item = framed_item(&dir, "src/session.rs").with_panel(
            PreviewPanel::new().with_metric("staged +18 −2 / unstaged +6".to_string(), Vec::new()),
        );

        let ident = preview_text(&item, 20)
            .lines()
            .next()
            .expect("枠の 1 行目")
            .to_owned();

        assert_eq!(
            ident, "src/session.rs",
            "主キーを削るより指標を落とす: {ident:?}"
        );
    }

    #[test]
    fn empty_context_items_are_dropped_and_an_empty_context_takes_no_line() {
        let dir = crate::test_support::TempDir::new("panel-context");

        let filled = framed_item(&dir, "main").with_panel(PreviewPanel::new().with_context(vec![
            "origin/main".to_string(),
            String::new(),
            "2 時間前".to_string(),
        ]));
        let context = panel_lines(&preview_text(&filled, 40))
            .get(1)
            .expect("枠の 2 行目")
            .to_owned();
        assert!(
            context.contains(&format!("origin/main{PANEL_CONTEXT_SEPARATOR}2 時間前")),
            "空の項目は区切りごと落ちる: {context:?}"
        );

        let empty = framed_item(&dir, "main")
            .with_panel(PreviewPanel::new().with_context(vec![String::new(), "  ".to_string()]));
        assert_eq!(
            panel_lines(&preview_text(&empty, 40)).len(),
            1,
            "項目が 1 つも無ければ 2 行目そのものを出さない"
        );
    }

    #[test]
    fn no_panel_is_drawn_when_the_preview_is_too_narrow_for_one() {
        let dir = crate::test_support::TempDir::new("panel-narrow");
        let item = framed_item(&dir, "main")
            .with_panel(PreviewPanel::new().with_metric("↑3".to_string(), Vec::new()));

        assert_eq!(
            preview_text(&item, PANEL_MIN_WIDTH - 1),
            "body line\n",
            "枠を描く幅が無い端末では本文を優先する"
        );
    }

    #[test]
    fn the_candidate_colours_are_carried_into_the_first_line_of_the_panel() {
        let dir = crate::test_support::TempDir::new("panel-colour");
        let item = framed_item(&dir, "7c1e9a4 fix").with_highlights(vec![Highlight::new(
            0,
            7,
            HighlightColor::Yellow,
        )]);

        let ident = preview_text(&item, 40)
            .lines()
            .next()
            .expect("枠の 1 行目")
            .to_owned();

        assert!(
            ident.starts_with(&format!(
                "{yellow}7c1e9a4{ANSI_RESET}",
                yellow = HighlightColor::Yellow.to_ansi()
            )),
            "一覧と同じ色が乗る: {ident:?}"
        );
    }

    #[test]
    fn escape_characters_in_a_previewed_file_are_shown_as_a_symbol() {
        let dir = crate::test_support::TempDir::new("panel-escape");
        let item = FinderItem::new(
            "captured.log".to_string(),
            "captured.log".to_string(),
            text_section(&dir, "body.txt", "\u{1b}[31mred\u{1b}[0m\n"),
            Language::Japanese.messages(),
        )
        .without_panel();

        assert_eq!(
            preview_text(&item, 40),
            format!("{ESCAPE_SYMBOL}[31mred{ESCAPE_SYMBOL}[0m\n"),
            "本文のエスケープ列は装飾として解釈させない"
        );
    }

    #[test]
    fn preview_of_a_non_previewable_item_is_empty_text() {
        let item = FinderItem::new(
            "main".to_string(),
            "main".to_string(),
            PreviewSource::None,
            Language::Japanese.messages(),
        );

        match item.preview(preview_context()) {
            ItemPreview::Text(text) => assert!(text.is_empty()),
            _ => panic!("PreviewSource::None must produce empty text"),
        }
    }

    #[test]
    fn a_file_preview_shows_the_contents_of_the_file() {
        let dir = crate::test_support::TempDir::new("finder-file-preview");
        crate::test_support::write_file(dir.path(), "untracked.txt", "line 1\nline 2\n");

        let text = read_preview(
            Language::Japanese.messages(),
            &dir.path().join("untracked.txt"),
        )
        .expect("an existing file should be readable");

        assert_eq!(text, "line 1\nline 2\n");
    }

    #[test]
    fn a_large_file_preview_is_truncated_with_a_notice() {
        let dir = crate::test_support::TempDir::new("finder-file-preview-large");
        let contents = "x".repeat(FILE_PREVIEW_LIMIT + 100);
        crate::test_support::write_file(dir.path(), "large.txt", &contents);

        let messages = Language::Japanese.messages();
        let text = read_preview(messages, &dir.path().join("large.txt"))
            .expect("a large file should be readable");

        let notice = messages.finder().truncation_notice();
        // 本文と注記は改行 1 つで区切られる
        assert_eq!(text.len(), FILE_PREVIEW_LIMIT + "\n".len() + notice.len());
        assert!(text.ends_with(notice), "the notice is missing");
    }

    #[test]
    fn the_truncation_notice_is_translated() {
        let dir = crate::test_support::TempDir::new("finder-file-preview-large-en");
        let contents = "x".repeat(FILE_PREVIEW_LIMIT + 100);
        crate::test_support::write_file(dir.path(), "large.txt", &contents);
        let path = dir.path().join("large.txt");

        let japanese = read_preview(Language::Japanese.messages(), &path)
            .expect("a large file should be readable");
        let english = read_preview(Language::English.messages(), &path)
            .expect("a large file should be readable");

        assert_ne!(japanese, english, "the notice must be translated");
        assert!(japanese.ends_with(Language::Japanese.messages().finder().truncation_notice()));
        assert!(english.ends_with(Language::English.messages().finder().truncation_notice()));
    }

    #[test]
    fn a_short_file_preview_carries_no_notice_in_either_language() {
        // 打ち切りが起きていない本文に注記が混ざらないこと（言語を問わない）
        let dir = crate::test_support::TempDir::new("finder-file-preview-short");
        crate::test_support::write_file(dir.path(), "small.txt", "line 1\n");
        let path = dir.path().join("small.txt");

        for language in [Language::Japanese, Language::English] {
            let text =
                read_preview(language.messages(), &path).expect("a small file should be readable");

            assert_eq!(text, "line 1\n", "{language:?} must not add a notice");
        }
    }

    #[test]
    fn the_file_read_failure_is_translated_and_names_the_path() {
        let dir = crate::test_support::TempDir::new("finder-file-preview-missing-languages");
        let missing = dir.path().join("missing.txt");

        let japanese = render_file(Language::Japanese.messages(), &missing)
            .expect_err("a missing file must not be readable");
        let english = render_file(Language::English.messages(), &missing)
            .expect_err("a missing file must not be readable");

        assert_ne!(japanese, english, "the message must be translated");
        for message in [&japanese, &english] {
            assert!(
                message.contains(&missing.display().to_string()),
                "the path should be named: {message}"
            );
        }
    }

    #[test]
    fn a_missing_file_is_reported_in_the_preview_instead_of_failing() {
        let dir = crate::test_support::TempDir::new("finder-file-preview-missing");
        let missing = dir.path().join("missing.txt");
        let item = FinderItem::new(
            "missing.txt".to_string(),
            "missing.txt".to_string(),
            PreviewSource::File(missing.clone()),
            Language::Japanese.messages(),
        )
        .without_panel();

        match item.preview(preview_context()) {
            ItemPreview::AnsiText(text) => assert!(
                text.contains(&missing.display().to_string()),
                "the path should be named: {text}"
            ),
            _ => panic!("a file preview must produce ANSI text"),
        }
    }

    #[test]
    fn a_git_in_preview_runs_in_the_given_repository() {
        // 現在のリポジトリではなく、指定したディレクトリの情報が出ることを確かめる
        let dir = crate::test_support::TempDir::new("finder-git-in");
        crate::test_support::init_repository(dir.path());
        let head = crate::test_support::commit(dir.path(), "sibling commit");

        let item = FinderItem::new(
            "sibling".to_string(),
            "sibling".to_string(),
            PreviewSource::GitIn {
                directory: dir.path().to_path_buf(),
                args: vec!["rev-parse".to_string(), "HEAD".to_string()],
            },
            Language::Japanese.messages(),
        )
        .without_panel();

        match item.preview(preview_context()) {
            ItemPreview::AnsiText(text) => assert_eq!(text.trim(), head),
            _ => panic!("a git preview must be rendered as ANSI text"),
        }
    }

    #[test]
    fn a_failing_git_in_preview_shows_the_reason_instead_of_failing() {
        let dir = crate::test_support::TempDir::new("finder-git-in-failure");
        crate::test_support::init_repository(dir.path());

        let item = FinderItem::new(
            "sibling".to_string(),
            "sibling".to_string(),
            PreviewSource::GitIn {
                directory: dir.path().to_path_buf(),
                args: vec!["fuzgit-no-such-subcommand".to_string()],
            },
            Language::Japanese.messages(),
        )
        .without_panel();

        match item.preview(preview_context()) {
            ItemPreview::AnsiText(message) => assert!(
                message.contains("fuzgit-no-such-subcommand"),
                "the failing command should be named: {message}"
            ),
            _ => panic!("a failing preview must produce ANSI text"),
        }
    }

    #[test]
    fn a_git_in_preview_can_be_a_section_of_a_composite() {
        let dir = crate::test_support::TempDir::new("finder-git-in-composite");
        crate::test_support::init_repository(dir.path());
        let head = crate::test_support::commit(dir.path(), "sibling commit");

        let sections = vec![(
            "HEAD".to_string(),
            PreviewSource::GitIn {
                directory: dir.path().to_path_buf(),
                args: vec!["rev-parse".to_string(), "HEAD".to_string()],
            },
        )];

        assert_eq!(
            render_composite(Language::Japanese.messages(), &sections),
            format!("── HEAD ──\n{head}")
        );
    }

    /// 内容が固定のセクションを作る（git を実行せずに連結結果だけを検証するため）。
    fn text_section(
        dir: &crate::test_support::TempDir,
        name: &str,
        contents: &str,
    ) -> PreviewSource {
        crate::test_support::write_file(dir.path(), name, contents);
        PreviewSource::File(dir.path().join(name))
    }

    #[test]
    fn a_composite_preview_joins_each_section_under_its_heading() {
        let dir = crate::test_support::TempDir::new("finder-composite");
        let sections = vec![
            (
                "staged".to_string(),
                text_section(&dir, "staged.txt", "+staged line\n"),
            ),
            (
                "unstaged".to_string(),
                text_section(&dir, "unstaged.txt", "+unstaged line\n"),
            ),
        ];

        let text = render_composite(Language::Japanese.messages(), &sections);

        assert_eq!(
            text,
            "── staged ──\n+staged line\n\n── unstaged ──\n+unstaged line"
        );
    }

    #[test]
    fn a_single_section_has_no_separator() {
        let dir = crate::test_support::TempDir::new("finder-composite-single");
        let sections = vec![(
            "untracked".to_string(),
            text_section(&dir, "new.txt", "hello\n"),
        )];

        assert_eq!(
            render_composite(Language::Japanese.messages(), &sections),
            "── untracked ──\nhello"
        );
    }

    #[test]
    fn an_empty_section_is_omitted_together_with_its_heading() {
        let dir = crate::test_support::TempDir::new("finder-composite-empty");
        let sections = vec![
            (
                "staged".to_string(),
                text_section(&dir, "staged.txt", "   \n\n"),
            ),
            (
                "unstaged".to_string(),
                text_section(&dir, "unstaged.txt", "+unstaged line\n"),
            ),
        ];

        let text = render_composite(Language::Japanese.messages(), &sections);

        assert!(
            !text.contains("staged ──\n\n"),
            "空セクションの見出しは出さない: {text}"
        );
        assert_eq!(text, "── unstaged ──\n+unstaged line");
    }

    #[test]
    fn a_composite_of_only_empty_sections_is_empty() {
        let sections = vec![
            ("staged".to_string(), PreviewSource::None),
            ("unstaged".to_string(), PreviewSource::None),
        ];

        assert_eq!(
            render_composite(Language::Japanese.messages(), &sections),
            ""
        );
    }

    #[test]
    fn a_composite_without_sections_is_empty() {
        assert_eq!(render_composite(Language::Japanese.messages(), &[]), "");
    }

    #[test]
    fn a_failing_section_keeps_its_heading_and_shows_the_reason() {
        // 一部が取れなかったことを隠さない（表示できない理由をその場に出す）
        let dir = crate::test_support::TempDir::new("finder-composite-missing");
        let missing = dir.path().join("missing.txt");
        let sections = vec![("staged".to_string(), PreviewSource::File(missing.clone()))];

        let text = render_composite(Language::Japanese.messages(), &sections);

        assert!(text.starts_with("── staged ──\n"), "unexpected: {text}");
        assert!(
            text.contains(&missing.display().to_string()),
            "the path should be named: {text}"
        );
    }

    #[test]
    fn a_composite_preview_is_rendered_as_ansi_text() {
        let dir = crate::test_support::TempDir::new("finder-composite-item");
        let item = FinderItem::new(
            "MM src/main.rs".to_string(),
            "src/main.rs".to_string(),
            PreviewSource::Composite(vec![(
                "staged".to_string(),
                text_section(&dir, "staged.txt", "+staged line\n"),
            )]),
            Language::Japanese.messages(),
        )
        .without_panel();

        match item.preview(preview_context()) {
            ItemPreview::AnsiText(text) => {
                assert_eq!(text, "── staged ──\n+staged line");
            }
            _ => panic!("a composite preview must be rendered as ANSI text"),
        }
    }

    #[test]
    fn a_nested_composite_is_flattened_into_the_same_preview() {
        let dir = crate::test_support::TempDir::new("finder-composite-nested");
        let sections = vec![(
            "outer".to_string(),
            PreviewSource::Composite(vec![(
                "inner".to_string(),
                text_section(&dir, "inner.txt", "body\n"),
            )]),
        )];

        assert_eq!(
            render_composite(Language::Japanese.messages(), &sections),
            "── outer ──\n── inner ──\nbody"
        );
    }

    #[test]
    fn options_enable_the_preview_pane_and_reflect_the_selection_mode() {
        let single = build_options(&FinderOptions::new(SelectionMode::Single))
            .expect("options should build");
        assert!(!single.multi);
        // preview がセットされていないと skim は SkimItem::preview() を呼ばない
        assert!(single.preview.is_some());
        assert!(single.reverse);
        assert!(single.highlight_line, "カーソル行を行末まで塗る");

        let multi =
            build_options(&FinderOptions::new(SelectionMode::Multi)).expect("options should build");
        assert!(multi.multi);
    }

    #[test]
    fn the_marker_ends_with_a_space_so_it_does_not_touch_the_candidate_text() {
        // skim は [selector 列][marker 列][本文] を区切り無しで並べるため、マーカーが
        // 末尾に空白を持たないと `>>mike   origin/main` のように本文とくっついて見える
        for mode in [SelectionMode::Single, SelectionMode::Multi] {
            let options = build_options(&FinderOptions::new(mode)).expect("options should build");

            assert_eq!(
                options.multi_select_icon, "> ",
                "{mode:?} のマーカーは字形を変えず末尾に空白を 1 つ持つ"
            );
            assert_eq!(
                options.selector_icon, ">",
                "{mode:?} のカーソル記号は既定のまま"
            );
        }
    }

    #[test]
    fn the_marker_is_the_same_in_both_selection_modes() {
        // marker が描画されない行でも同じ幅の空白が確保されるため、片方のモードにだけ
        // 空白を足すと接頭辞の幅がモードによって変わってしまう
        let single = build_options(&FinderOptions::new(SelectionMode::Single))
            .expect("options should build");
        let multi =
            build_options(&FinderOptions::new(SelectionMode::Multi)).expect("options should build");

        assert_eq!(single.multi_select_icon, multi.multi_select_icon);
        assert_eq!(single.selector_icon, multi.selector_icon);
    }

    #[test]
    fn options_have_no_header_and_no_selector_by_default() {
        let options =
            build_options(&FinderOptions::new(SelectionMode::Multi)).expect("options should build");

        assert!(options.header.is_none());
        assert!(options.selector.is_none());
    }

    #[test]
    fn a_header_is_passed_to_skim_in_either_mode() {
        // 単一選択にも「何を選ぶのか」を示すヘッダーを出すため、モードで分岐させない
        for mode in [SelectionMode::Single, SelectionMode::Multi] {
            let options =
                build_options(&FinderOptions::new(mode).with_header("Enter: 切り替え".to_string()))
                    .expect("options should build");

            assert_eq!(
                options.header.as_deref(),
                Some("Enter: 切り替え"),
                "{mode:?}"
            );
        }
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
            Language::Japanese.messages(),
        );
        let unstaged = FinderItem::new(
            " M src/main.rs".to_string(),
            "src/main.rs".to_string(),
            PreviewSource::None,
            Language::Japanese.messages(),
        );

        // 判定はキーではなく表示文字列の完全一致で行われる
        assert!(selector.should_select(0, &staged));
        assert!(!selector.should_select(1, &unstaged));
    }

    #[test]
    fn the_preselection_matches_the_display_string_regardless_of_the_display_language() {
        // 事前選択は表示文字列の完全一致で判定される。fuzgit 自身の文言（FinderMessages）は
        // display に入らないため、表示言語を変えても同じ display は同じように事前選択される
        // （`items()` と `preselect()` が同じ組み立て関数を使う依存関係が保たれている証拠）
        let display = "M  src/main.rs".to_string();

        for language in [Language::Japanese, Language::English] {
            let options = build_options(
                &FinderOptions::new(SelectionMode::Multi).with_preselect(vec![display.clone()]),
            )
            .expect("options should build");
            let selector = options
                .selector
                .expect("multi mode should install a selector");
            let item = FinderItem::new(
                display.clone(),
                "src/main.rs".to_string(),
                PreviewSource::None,
                language.messages(),
            );

            assert!(
                selector.should_select(0, &item),
                "{language:?} must not change the display string used for matching"
            );
        }
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
            Language::Japanese.messages(),
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
            Language::Japanese.messages(),
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
                Language::Japanese.messages(),
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
