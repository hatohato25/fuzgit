//! 標準エラーへ書き出す 1 行の着色（FR-36）。
//!
//! 対象は 2 種類ある。
//!
//! 1. **fuzgit 自身が組み立てた行**（`[3/11] koebon` のような進捗行、集計行、
//!    選択を省略した理由の行）。
//! 2. **git が出力した fetch の更新表**（` * [new branch]  foo -> origin/foo`）。
//!
//! # git の出力を解釈する唯一の場所である
//!
//! (2) は「fuzgit は git の出力を解釈せず、そのままユーザーへ渡す」という既定の方針
//! （design.md）に対する**明示的な例外**であり、例外はこの module に閉じる。ここで行うのは
//! **表示のための着色だけ**であり、読み取った内容で分岐したり、実行するコマンドを
//! 変えたり、終了コードを決めたりはしない。書式が変わって解析に失敗した場合は
//! **色を付けずにその行をそのまま通す**（[`status_range`] が `None` を返す）ため、
//! git 側の書式変更で情報が失われることはない。
//!
//! # 色を出す条件
//!
//! 書き出し先（標準エラー）が端末であり、かつ `NO_COLOR` が設定されていない場合だけ
//! 色を付ける（[`Painter::for_stderr`]）。**非端末での出力は 1 バイトも変わらない。**
//! パイプやリダイレクトで受けているスクリプトの挙動を変えないためである。

use std::io::IsTerminal as _;

use crate::finder::{ANSI_RESET, HighlightColor};

/// 色を抑止する環境変数（<https://no-color.org>）。
///
/// 値は見ない。「設定されていること」だけが意味を持つという同サイトの規約に従う
/// （空文字での設定は「未設定」と区別が付かないため、空文字は無視する）。
const NO_COLOR_ENV: &str = "NO_COLOR";

/// git の fetch 更新表で、行の先頭から状態コードまでに置かれる固定の桁。
///
/// git は ` <コード> <要約> <from> -> <to>` の形で 1 行を組み立てる
/// （`builtin/fetch.c` の `print_ref_status`）。先頭の 1 桁は必ず空白、2 桁目が状態コード、
/// 3 桁目が空白であり、要約はそれ以降に始まる。実測（git 2.55.0）で確認済み。
const STATUS_FLAG_INDEX: usize = 1;

/// 要約が始まるバイト位置（[`STATUS_FLAG_INDEX`] の次の空白の次）。
const SUMMARY_START_INDEX: usize = 3;

/// fetch の更新表の行であることを示す区切り。
///
/// `<from> -> <to>` の矢印。これを含むことを条件に加えることで、`From <URL>` の見出しや
/// git のヒント行を状態行と取り違えない。
const REF_ARROW: &[u8] = b" -> ";

/// 新しいタグであることを示す要約（git の英語表記）。
///
/// 状態コードは新しいブランチと同じ `*` であり、ブランチとタグを分けるにはここを見るしかない。
/// git の表示言語が英語以外に翻訳されている場合はこの一致が外れるが、そのときは
/// **タグも新規追加として緑になるだけ**で色が失われることはない（安全側に落ちる）。
const NEW_TAG_SUMMARY: &[u8] = b"[new tag]";

/// 着色の可否を握り、実際に色を乗せる。
///
/// `bool` を各コマンドへ配らず、**「色を出してよい状況か」を 1 か所で決めた結果**を
/// 持ち回る（`InstallMode` / `PruneMode` と同方針）。生成は原則 [`Painter::for_stderr`] で
/// 行い、単体テストだけが [`Painter::enabled`] / [`Painter::disabled`] を直接使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Painter {
    /// 色を乗せるかどうか。
    enabled: bool,
}

impl Painter {
    /// 標準エラーの状態から組み立てる。
    ///
    /// 端末でない場合（パイプ・リダイレクト）と `NO_COLOR` が設定されている場合は
    /// 色を出さない。判定を実行のたびに繰り返さず、コマンドの開始時に 1 度だけ行う。
    #[must_use]
    pub fn for_stderr() -> Self {
        Self {
            enabled: std::io::stderr().is_terminal() && !no_color_requested(),
        }
    }

    /// 常に色を出す [`Painter`]（テスト用）。
    #[must_use]
    pub fn enabled() -> Self {
        Self { enabled: true }
    }

    /// 色を出さない [`Painter`]。
    ///
    /// 単体テストのほか、書き出し先が端末でないことが呼び出し側で確定している場合に使う。
    #[must_use]
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    /// 色を出す状況かどうか。
    ///
    /// 着色そのものではなく、**色を出せる端末かどうかで振る舞いを変えたい**呼び出し側が使う
    /// （`git fetch` の進捗表示を要求するかの判断。[`crate::commands::fetch`]）。
    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    /// 1 行全体へ前景色を乗せる。
    ///
    /// 色を出さない場合は受け取った文字列をそのまま返す。
    #[must_use]
    pub fn paint(self, line: &str, color: HighlightColor) -> String {
        if !self.enabled || line.is_empty() {
            return line.to_owned();
        }

        format!(
            "{start}{line}{ANSI_RESET}",
            start = color.to_ansi(),
            line = line
        )
    }

    /// git の fetch 更新表を着色する。
    ///
    /// **バイト列のまま扱う。**ブランチ名は UTF-8 とは限らず、文字列へ変換すると
    /// 不正なバイトが置換文字へ潰れて情報が失われるためである。解析に使うのは
    /// 行頭の状態コードと ASCII の区切りだけであり、符号化に依存しない。
    ///
    /// 色を出さない場合は受け取ったバイト列をそのまま返す。
    #[must_use]
    pub fn paint_fetch_output(self, output: &[u8]) -> Vec<u8> {
        if !self.enabled {
            return output.to_vec();
        }

        let mut painted = Vec::with_capacity(output.len());
        let mut stream = FetchOutputStream::new();
        stream.push(output, &mut painted);
        stream.flush(&mut painted);

        painted
    }
}

/// `NO_COLOR` による抑止が要求されているか。
///
/// 空文字での設定は「未設定」と区別が付かないため無視する（`NO_COLOR=` を
/// 「色を出す」の意味で使う環境と衝突しないようにする）。
fn no_color_requested() -> bool {
    std::env::var_os(NO_COLOR_ENV).is_some_and(|value| !value.is_empty())
}

/// git の出力を行ごとに着色しながら流す。
///
/// **`git fetch` の出力は行だけではない。**進捗表示（`Receiving objects: 42%`）は
/// 改行ではなく復帰（`\r`）で区切って同じ行を上書きするため、改行だけを境目にすると
/// 進捗が画面へ出ないまま溜まる。復帰で区切られた断片は**着色せずそのまま通す**ことで、
/// 進捗表示を git が書いたとおりに見せる。
///
/// 逐次実行（[`crate::git::exec::run_git_painted_in`]）では受け取った分から順に書き出す
/// 必要があるため、状態を持つこの型で扱う。一括で受け取る経路
/// （[`Painter::paint_fetch_output`]）も同じ実装を通す。
pub(crate) struct FetchOutputStream {
    /// 区切りがまだ現れていない末尾。
    pending: Vec<u8>,
}

impl FetchOutputStream {
    /// 空の状態で作る。
    ///
    /// 着色するかどうかの判定は呼び出し側で済んでいる（この型へ流すのは着色する場合だけ）。
    /// 判定を二重に持たないことで、「色を出さないときは 1 バイトも変えない」という
    /// 保証の根拠を [`Painter`] 1 か所に閉じる。
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// 受け取ったバイト列を処理し、書き出せる分を `out` へ積む。
    pub(crate) fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for byte in bytes {
            self.pending.push(*byte);

            match *byte {
                // 改行で終わる 1 行だけが更新表の候補である
                b'\n' => {
                    let line = std::mem::take(&mut self.pending);
                    paint_line(&line, out);
                }
                // 進捗表示の断片。解析せずそのまま通す
                b'\r' => out.append(&mut self.pending),
                _ => {}
            }
        }
    }

    /// 区切りの現れないまま残った末尾を書き出す。
    ///
    /// 改行で終わらない最終行（git が改行を付けずに終えた場合）を落とさないために要る。
    pub(crate) fn flush(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.pending);
    }
}

/// 改行で終わる 1 行を着色して `out` へ積む。
fn paint_line(line: &[u8], out: &mut Vec<u8>) {
    let Some((start, end, color)) = status_range(line) else {
        out.extend_from_slice(line);
        return;
    };

    out.extend_from_slice(&line[..start]);
    out.extend_from_slice(color.to_ansi().as_bytes());
    out.extend_from_slice(&line[start..end]);
    out.extend_from_slice(ANSI_RESET.as_bytes());
    out.extend_from_slice(&line[end..]);
}

/// fetch 更新表の 1 行から、着色する範囲（半開区間）と色を求める（純関数）。
///
/// 色を乗せるのは**状態コードと要約だけ**であり、参照名には乗せない。行の大半を占める
/// 参照名を塗ると、どこが目印なのかが読み取れなくなるためである
/// （候補一覧でタイトル列を塗らないのと同じ判断。design.md）。
///
/// 更新表の行と判断できない場合は `None` を返す。判断の材料は
/// 「行頭の桁の形」と「`->` を含むこと」の 2 つだけで、参照名の中身は見ない。
fn status_range(line: &[u8]) -> Option<(usize, usize, HighlightColor)> {
    // 更新表以外の行（`From <URL>` の見出し、git のヒント）を除く
    if !contains(line, REF_ARROW) {
        return None;
    }
    if line.first() != Some(&b' ') || line.get(STATUS_FLAG_INDEX + 1) != Some(&b' ') {
        return None;
    }

    let flag = *line.get(STATUS_FLAG_INDEX)?;
    let summary = line.get(SUMMARY_START_INDEX..)?;
    let end = SUMMARY_START_INDEX + summary_length(summary)?;

    Some((STATUS_FLAG_INDEX, end, flag_color(flag, summary)?))
}

/// 要約（`[new branch]` / `abc1234..def5678`）の長さを求める。
///
/// 角括弧で始まるものは閉じ括弧まで、そうでないもの（コミット ID の範囲）は
/// 次の空白までを 1 つの要約とする。
fn summary_length(summary: &[u8]) -> Option<usize> {
    if summary.first() == Some(&b'[') {
        return summary
            .iter()
            .position(|byte| *byte == b']')
            .map(|at| at + 1);
    }

    summary.iter().position(|byte| *byte == b' ')
}

/// 状態コードに対応する色を求める。
///
/// 選ぶ基準は**その行がユーザーに何を知らせるべきか**である。取り込んだ内容が
/// 増えたのか（緑）、こちらが持っていたものが消えた・置き換わったのか（赤）、
/// タグという別の種類なのか（黄）、何も変わっていないのか（Dim）を分ける。
/// 早送りの更新（コード無し）に色を付けないのは、これが最も多く現れる「通常の結果」であり、
/// すべてに色が付くと注意を向けるべき行が埋もれるためである。
fn flag_color(flag: u8, summary: &[u8]) -> Option<HighlightColor> {
    match flag {
        // 新しい参照。タグだけは種類が違うため分ける（英語表記でない場合は緑へ落ちる）
        b'*' if summary.starts_with(NEW_TAG_SUMMARY) => Some(HighlightColor::Yellow),
        b'*' => Some(HighlightColor::Green),
        // タグの付け替え
        b't' => Some(HighlightColor::Yellow),
        // 消えた・巻き戻された・拒否された。いずれも手元の状態が失われ得る
        b'-' | b'+' | b'!' => Some(HighlightColor::Red),
        // 変化が無いことは目立たせない
        b'=' => Some(HighlightColor::Dim),
        _ => None,
    }
}

/// `haystack` が `needle` を含むか。
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実測（git 2.55.0）で採取した `git fetch` の更新表。
    ///
    /// 桁の詰め方は同時に更新される参照名の長さで変わるため、**幅の違う 2 群**を
    /// そのまま残してある（`[rejected]` の行は他より要約が短い）。
    const NEW_BRANCH: &[u8] = b" * [new branch]      newone     -> origin/newone\n";
    const NEW_TAG: &[u8] = b" * [new tag]         t-new      -> t-new\n";
    const DELETED: &[u8] = b" - [deleted]         (none)     -> origin/deleted\n";
    const FAST_FORWARD: &[u8] = b"   ac909ca..00e1336  ff         -> origin/ff\n";
    const FORCED: &[u8] = b" + b5df0a5...d152852 forced     -> origin/forced  (forced update)\n";
    const REJECTED: &[u8] = b" ! [rejected] t-new      -> t-new  (would clobber existing tag)\n";
    const UP_TO_DATE: &[u8] = b" = [up to date]      main       -> origin/main\n";
    const HEADER: &[u8] = b"From github.com:cyg-dev-base/cyloc\n";

    /// 着色された範囲だけを取り出す（色指定と解除の間）。
    ///
    /// **バイト列のまま探す。**着色の対象には UTF-8 でない参照名が含まれ得るため、
    /// 文字列へ変換してから探すとその検査自体が失敗する。
    fn painted_part(painted: &[u8]) -> String {
        let escape = painted
            .iter()
            .position(|byte| *byte == b'\x1b')
            .expect("a colour must be applied");
        let body = &painted[escape..];
        let open = body
            .iter()
            .position(|byte| *byte == b'm')
            .expect("the colour opens")
            + 1;
        let close = open
            + body[open..]
                .iter()
                .position(|byte| *byte == b'\x1b')
                .expect("the colour closes");

        String::from_utf8(body[open..close].to_vec()).expect("the status code is ASCII")
    }

    #[test]
    fn the_status_code_and_its_summary_are_the_coloured_part() {
        // 参照名は塗らない。塗ると行の大半が色になり目印が埋もれる
        let painted = Painter::enabled().paint_fetch_output(NEW_BRANCH);

        assert_eq!(painted_part(&painted), "* [new branch]");
    }

    #[test]
    fn a_range_of_commit_ids_is_a_summary_as_well() {
        let painted = Painter::enabled().paint_fetch_output(FORCED);

        assert_eq!(painted_part(&painted), "+ b5df0a5...d152852");
    }

    #[test]
    fn every_status_code_takes_the_colour_of_what_it_tells_the_user() {
        for (line, expected) in [
            (NEW_BRANCH, Some(HighlightColor::Green)),
            (NEW_TAG, Some(HighlightColor::Yellow)),
            (DELETED, Some(HighlightColor::Red)),
            (FORCED, Some(HighlightColor::Red)),
            (REJECTED, Some(HighlightColor::Red)),
            (UP_TO_DATE, Some(HighlightColor::Dim)),
            // 最も多く現れる通常の結果。塗ると注意すべき行が埋もれる
            (FAST_FORWARD, None),
        ] {
            assert_eq!(
                status_range(line).map(|(_, _, color)| color),
                expected,
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_line_that_is_not_part_of_the_update_table_is_left_alone() {
        for line in [
            HEADER,
            b"\n".as_slice(),
            b"fatal: could not read from remote repository\n".as_slice(),
            // 矢印が無い。git のヒント行を状態行と取り違えない
            b" * [new branch] not-a-table-line\n".as_slice(),
        ] {
            assert_eq!(status_range(line), None, "{line:?}");
            assert_eq!(
                Painter::enabled().paint_fetch_output(line),
                line,
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_localised_new_tag_is_still_coloured_as_a_new_reference() {
        // 翻訳された git では種類を見分けられない。色が消えるのではなく緑へ落ちる
        let line = b" * [%e6%96%b0]  v1 -> v1\n";

        assert_eq!(
            status_range(line).map(|(_, _, color)| color),
            Some(HighlightColor::Green)
        );
    }

    #[test]
    fn a_disabled_painter_changes_not_a_single_byte() {
        // パイプ・リダイレクトで受けているスクリプトの挙動を変えない
        let table = [NEW_BRANCH, NEW_TAG, DELETED, FORCED, HEADER].concat();

        assert_eq!(Painter::disabled().paint_fetch_output(&table), table);
        assert_eq!(
            Painter::disabled().paint("[1/3] alpha", HighlightColor::Cyan),
            "[1/3] alpha"
        );
    }

    #[test]
    fn a_whole_line_is_wrapped_by_one_colour() {
        assert_eq!(
            Painter::enabled().paint("[1/3] alpha", HighlightColor::Cyan),
            format!(
                "{start}[1/3] alpha{ANSI_RESET}",
                start = HighlightColor::Cyan.to_ansi()
            )
        );
    }

    #[test]
    fn an_empty_line_is_not_wrapped() {
        // 中身の無い行に色指定だけが残ると、その後の出力へ色が漏れる余地を作る
        assert_eq!(Painter::enabled().paint("", HighlightColor::Cyan), "");
    }

    #[test]
    fn every_line_of_a_block_is_examined() {
        let block = [HEADER, NEW_BRANCH, FAST_FORWARD, NEW_TAG].concat();
        let painted = Painter::enabled().paint_fetch_output(&block);
        let text = String::from_utf8(painted).expect("the sample is ASCII");

        assert_eq!(
            text.matches(ANSI_RESET).count(),
            2,
            "only the new branch and the new tag carry a colour: {text:?}"
        );
    }

    #[test]
    fn a_progress_fragment_is_passed_through_without_being_examined() {
        // `git fetch` の進捗は復帰で同じ行を上書きする。改行だけを境目にすると
        // 進捗が画面へ出ないまま溜まる
        let mut painted = Vec::new();
        let mut stream = FetchOutputStream::new();

        stream.push(b"Receiving objects:  42% (42/100)\r", &mut painted);
        assert_eq!(painted, b"Receiving objects:  42% (42/100)\r");

        stream.push(b"Receiving objects: 100% (100/100), done.\n", &mut painted);
        stream.flush(&mut painted);
        assert!(
            !painted.contains(&b'\x1b'),
            "a progress line carries no status code: {painted:?}"
        );
    }

    #[test]
    fn a_line_split_across_reads_is_coloured_once_it_is_complete() {
        // 逐次実行では 1 行が複数回の読み取りに分かれて届く
        let mut painted = Vec::new();
        let mut stream = FetchOutputStream::new();

        let (head, tail) = NEW_BRANCH.split_at(10);
        stream.push(head, &mut painted);
        assert!(painted.is_empty(), "an incomplete line is held back");

        stream.push(tail, &mut painted);
        stream.flush(&mut painted);
        assert_eq!(painted_part(&painted), "* [new branch]");
    }

    #[test]
    fn a_final_line_without_a_newline_is_not_lost() {
        let mut painted = Vec::new();
        let mut stream = FetchOutputStream::new();

        stream.push(b"no trailing newline", &mut painted);
        stream.flush(&mut painted);

        assert_eq!(painted, b"no trailing newline");
    }

    #[test]
    fn a_reference_name_that_is_not_utf8_is_passed_through_untouched() {
        // ブランチ名は UTF-8 とは限らない。文字列へ変換すると置換文字へ潰れる
        let mut line = b" * [new branch]      ".to_vec();
        line.extend_from_slice(&[0xff, 0xfe]);
        line.extend_from_slice(b" -> origin/x\n");

        let painted = Painter::enabled().paint_fetch_output(&line);

        assert!(
            painted.windows(2).any(|window| window == [0xff, 0xfe]),
            "the raw bytes must survive: {painted:?}"
        );
        assert_eq!(painted_part(&painted), "* [new branch]");
    }
}
