//! `gz pr` — GitHub の Pull Request を選んで checkout する（FR-34 / FR-35）。
//!
//! # 採用基準 C の例外は「候補生成の 1 回」だけである
//!
//! fuzgit は候補生成もプレビューもネットワークを使わないことを設計原則にしている
//! （requirements.md「採用基準」C）。本コマンドはその唯一の例外だが、**破るのは候補生成の
//! 1 回だけ**である。`gh pr list` の 1 回に PR の**本文まで相乗りさせる**ことで、
//! プレビューは取得済み文字列の描画に閉じる（[`crate::finder::PreviewSource::Text`]）。
//!
//! 実測では `body` の追加コストは +0.2 秒であるのに対し、`reviewDecision` は +1.45 秒、
//! `statusCheckRollup` は +2.7 秒である。後者 2 つは既定では取得せず、`--checks` を
//! 指定したときだけ取りに行く（design.md「実機で確認済みの前提」）。
//!
//! # フィールドの並びは「2 通りを別に持つ」
//!
//! `--checks` の有無でフィールド数が 8 と 12 に変わる。**可変長にして実行時に
//! 「あるかもしれない」を判定しない**（暗黙のフォールバック禁止）。jq 式・期待フィールド数・
//! 復元処理をそれぞれ 2 通り持ち、どちらを使うかは呼び出し時に確定させる。

use std::io::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::commands::worktree_install::InstallMode;
use crate::commands::{COLUMN_SEPARATOR, aligned_candidates, selection_header, worktree};
use crate::finder::{
    FinderItem, FinderOptions, Highlight, HighlightColor, PreviewPanel, PreviewSource,
    SelectionMode, select_one_with,
};
use crate::gh;
use crate::i18n::{Language, Messages};

/// 候補として取得する PR の最大件数。
///
/// `gh pr list` の既定は 30 件。日常的に開いている PR の数を上回る一方、
/// 取得時間が件数にほとんど比例しないため（往復が支配的）、既定より広めに取る。
const DEFAULT_LIMIT: usize = 100;

/// `--checks` を付けない場合に取得する JSON フィールド。
const FIELDS: &str = "number,headRefName,baseRefName,author,isDraft,url,title,body";

/// `--checks` を付けた場合に取得する JSON フィールド。
const FIELDS_WITH_CHECKS: &str =
    "number,headRefName,baseRefName,author,isDraft,url,title,body,reviewDecision,statusCheckRollup";

/// `--checks` を付けない場合の 1 行あたりのフィールド数。
const FIELD_COUNT: usize = 8;

/// `--checks` を付けた場合の 1 行あたりのフィールド数。
const FIELD_COUNT_WITH_CHECKS: usize = 12;

/// 基本フィールドを `@tsv` の 1 行へ並べる jq 式。
///
/// `.author` は削除済みユーザーで **null になり得る**。`.author.login` は null でも
/// エラーにならず空フィールドになるためフィールド数は保たれるが、表示が空欄になるのを
/// 避けて `// "-"` で既定値を与える（実測確認済み）。
const JQ_BASE: &str = r#"(.number|tostring), .headRefName, .baseRefName, (.author.login // "-"), (.isDraft|tostring), .url, .title, .body"#;

/// `--checks` で加わる 4 フィールド（レビュー判定と、CI の成功・失敗・保留の件数）を並べる jq 式。
///
/// `statusCheckRollup` の要素は `CheckRun`（`status` / `conclusion` を持つ）と
/// `StatusContext`（`state` を持つ）の 2 種類が混在する。**どちらの形も数えられるように**
/// 両方の鍵を見る。チェックが 1 件も無い PR では `statusCheckRollup` が null になるため、
/// `[]?` で「無ければ空」として扱う（エラーにしない）。
const JQ_CHECKS: &str = r#"(.reviewDecision // ""), ([.statusCheckRollup[]? | select((.conclusion // "") as $c | $c=="SUCCESS" or $c=="SKIPPED" or $c=="NEUTRAL" or (.state=="SUCCESS"))] | length | tostring), ([.statusCheckRollup[]? | select((.conclusion // "") as $c | $c=="FAILURE" or $c=="TIMED_OUT" or $c=="CANCELLED" or $c=="ACTION_REQUIRED" or $c=="STARTUP_FAILURE" or (.state=="FAILURE") or (.state=="ERROR"))] | length | tostring), ([.statusCheckRollup[]? | select(((.status // "") != "COMPLETED" and (.state // "") == "") or (.state=="PENDING"))] | length | tostring)"#;

/// `--checks` で取得する CI の集計。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checks {
    /// 成功（SKIPPED / NEUTRAL を含む）した件数。
    pub passed: usize,
    /// 失敗した件数。
    pub failed: usize,
    /// まだ終わっていない件数。
    pub pending: usize,
}

/// 候補 1 件分の PR。
///
/// **`body` を持つことが本コマンドの要**である。プレビューはこの文字列を描くだけであり、
/// カーソル移動のたびにネットワークへ出ない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCandidate {
    /// PR 番号。
    pub number: u64,
    /// 取り込み元のブランチ名。
    pub head_ref: String,
    /// 取り込み先のブランチ名。
    pub base_ref: String,
    /// 作者のログイン名（取得できない場合は `-`）。
    pub author: String,
    /// draft かどうか。
    pub is_draft: bool,
    /// PR の URL。
    pub url: String,
    /// PR のタイトル。
    pub title: String,
    /// PR の本文（プレビューに使う）。
    pub body: String,
    /// レビュー判定（`--checks` 指定時のみ。未レビューでは空文字列）。
    pub review_decision: Option<String>,
    /// CI の集計（`--checks` 指定時のみ）。
    pub checks: Option<Checks>,
}

impl PrCandidate {
    /// finder の候補キー。
    ///
    /// **決して翻訳しない。**この値は候補一覧との照合を経て `gh` の引数になる。
    pub fn key(&self) -> String {
        self.number.to_string()
    }
}

/// `gh pr list` へ渡す引数配列を組み立てる。
///
/// `--checks` の有無で **jq 式とフィールド数が変わる**ため、ここで 2 通りを作り分ける。
pub fn list_args(checks: bool) -> Vec<String> {
    let (fields, jq) = if checks {
        (
            FIELDS_WITH_CHECKS,
            format!(".[] | [{JQ_BASE}, {JQ_CHECKS}] | @tsv"),
        )
    } else {
        (FIELDS, format!(".[] | [{JQ_BASE}] | @tsv"))
    };

    vec![
        "pr".to_owned(),
        "list".to_owned(),
        "--limit".to_owned(),
        DEFAULT_LIMIT.to_string(),
        "--json".to_owned(),
        fields.to_owned(),
        "--jq".to_owned(),
        jq,
    ]
}

/// `--checks` の有無に対応する 1 行あたりのフィールド数。
fn field_count(checks: bool) -> usize {
    if checks {
        FIELD_COUNT_WITH_CHECKS
    } else {
        FIELD_COUNT
    }
}

/// `gh pr list` の出力を候補一覧へ復元する。
///
/// レコード区切りは実改行、フィールド区切りは実タブであり、値に含まれる改行・タブは
/// `@tsv` が 2 文字へ退避している（[`gh::split_tsv`]）。したがって行の分割で本文の改行に
/// 引っ掛かることは無い。
///
/// # Errors
///
/// フィールド数が想定と違う行があれば [`crate::error::Error::GhOutputMalformed`]。
/// **黙って読み飛ばさない**（暗黙のフォールバック禁止）。
pub fn parse(stdout: &[u8], checks: bool) -> Result<Vec<PrCandidate>> {
    let text = String::from_utf8_lossy(stdout);
    let expected = field_count(checks);

    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| Ok(to_candidate(&gh::split_tsv(line, expected)?, checks)))
        .collect()
}

/// 復号済みのフィールド列を候補へ組み立てる。
///
/// フィールド数は [`parse`] が検証済みであるため、ここでは並びだけを扱う。
fn to_candidate(fields: &[String], checks: bool) -> PrCandidate {
    let review_decision = checks.then(|| fields[8].clone());
    let checks = checks.then(|| Checks {
        passed: parse_count(&fields[9]),
        failed: parse_count(&fields[10]),
        pending: parse_count(&fields[11]),
    });

    PrCandidate {
        number: parse_count(&fields[0]) as u64,
        head_ref: fields[1].clone(),
        base_ref: fields[2].clone(),
        author: fields[3].clone(),
        is_draft: fields[4] == "true",
        url: fields[5].clone(),
        title: fields[6].clone(),
        body: fields[7].clone(),
        review_decision,
        checks,
    }
}

/// jq が `tostring` で書き出した整数を読む。
///
/// 値は jq が数値から起こしたものであり、非数が来ることは無い。読めなかった場合に
/// 実行を止める価値は無いため 0 として扱う（表示にしか使わない）。
fn parse_count(value: &str) -> usize {
    value.parse().unwrap_or(0)
}

/// メニューで選べる操作（`--action`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// PR を checkout する（既定の経路と同じ）。
    Checkout,
    /// PR の詳細を表示する（`gh pr view`）。
    View,
    /// PR の差分を表示する（`gh pr diff`）。
    Diff,
    /// 番号を標準出力へ書き出す。
    PrintNumber,
    /// URL を標準出力へ書き出す。
    PrintUrl,
}

/// メニューに載せる操作の並び。
///
/// **選択中の PR の性質で出し分けない。**項目が毎回同じ並びであることが選び間違いを
/// 防ぐという FR-16 / FR-32 の方針をそのまま踏襲する。
const MENU_ACTIONS: [MenuAction; 5] = [
    MenuAction::Checkout,
    MenuAction::View,
    MenuAction::Diff,
    MenuAction::PrintNumber,
    MenuAction::PrintUrl,
];

/// 操作に対応する、言語に依らない固定キー。
///
/// 表示文字列は翻訳の対象であり、選択結果の照合に使うと文言の変更が
/// 「別の操作を実行する」形の事故になり得る（`gz status` / FR-32 で確立済みの分離）。
///
/// ワイルドカードの腕を置かないのは、[`MenuAction`] を増やしたときに追加漏れが
/// コンパイルエラーになることがこの分離の目的そのものであるため。
fn menu_key(action: MenuAction) -> &'static str {
    match action {
        MenuAction::Checkout => "checkout",
        MenuAction::View => "view",
        MenuAction::Diff => "diff",
        MenuAction::PrintNumber => "print-number",
        MenuAction::PrintUrl => "print-url",
    }
}

/// 操作に対応する表示文字列。
fn menu_display(messages: &dyn Messages, action: MenuAction) -> &'static str {
    let pr = messages.pr();

    match action {
        MenuAction::Checkout => pr.action_checkout(),
        MenuAction::View => pr.action_view(),
        MenuAction::Diff => pr.action_diff(),
        MenuAction::PrintNumber => pr.action_print_number(),
        MenuAction::PrintUrl => pr.action_print_url(),
    }
}

/// `gz pr` の結果の置き場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// 現在の作業ツリーへ checkout する。
    Worktree,
    /// 指定した名前の新しい worktree へ checkout する（FR-35）。
    NewWorktree {
        /// 作成する worktree の名前（パスではない）。
        name: String,
        /// 依存インストールを行うか。
        install: InstallMode,
    },
}

/// PR を選んで checkout する。
///
/// # Errors
///
/// `gh` の不在・未認証・非ゼロ終了、候補の取得と選択（中断を含む）、
/// checkout の実行に失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    checks: bool,
    action: bool,
    destination: Destination,
) -> Result<()> {
    // `gh` の不在と worktree 名の不正は、いずれも**選ばせる前に**分かる。
    // 選ばせたあとに失敗させないため、ネットワークへ出る前に検査する
    if !gh::is_available() {
        return Err(crate::error::Error::GhNotFound.into());
    }
    // 名前 → パスの解決も**選ばせる前**に済ませる。`gh` へ渡すのは利用者が打った文字列
    // ではなく、ここで組み立てた値である（design.md セキュリティ設計）
    let destination = match destination {
        Destination::Worktree => Target::Current,
        Destination::NewWorktree { name, install } => Target::NewWorktree {
            path: worktree::resolve_new_name(messages, repository, &name)?,
            install,
        },
    };

    let candidates = fetch_candidates(messages, checks, &mut std::io::stderr())?;
    if candidates.is_empty() {
        bail!(messages.pr().no_candidates());
    }

    let selected = select(language, messages, &candidates, checks)?;

    if action {
        return run_menu(language, messages, repository, selected, &destination);
    }

    checkout(messages, repository, selected, &destination)
}

/// 解決済みの checkout 先。
///
/// [`Destination`] が受け取った**名前**を、実行前に**パス**へ解決したもの。
/// 解決を選択より前に済ませることで、選ばせたあとに名前の不備で失敗させない。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// 現在の作業ツリー。
    Current,
    /// 新しい worktree（絶対パス）。
    NewWorktree {
        /// 作成先の絶対パス。
        path: String,
        /// 依存インストールを行うか。
        install: InstallMode,
    },
}

/// 選んだ PR を checkout する。
///
/// `gh pr checkout` は継承 stdio で実行する。進捗・認証プロンプト・作成された
/// ブランチ名はいずれも `gh` 自身が出すものであり、fuzgit は再掲しない
/// （`gz fetch --siblings` が git の出力を再掲しないのと同じ判断）。
fn checkout(
    messages: &dyn Messages,
    repository: &gix::Repository,
    candidate: &PrCandidate,
    target: &Target,
) -> Result<()> {
    let number = candidate.key();

    match target {
        Target::Current => {
            gh::run_gh(&["pr", "checkout", &number])?;
            Ok(())
        }
        Target::NewWorktree { path, install } => {
            gh::run_gh(&["pr", "checkout", &number, "--worktree", path])?;

            // 作成の手段は違っても、作られたあとにやることは `gz worktree add` と同一である。
            // ただしパスの報告だけは違う——`gh` は `✓ Checked out PR #<n> in worktree <path>`
            // を自ら出すため、fuzgit が重ねて出さない（実測確認済み・T-478）
            worktree::finish_creation(
                messages,
                repository,
                path,
                *install,
                worktree::PathReport::AlreadyReported,
            )
        }
    }
}

/// 選んだ PR に対する操作をメニューから選んで実行する（`--action`）。
fn run_menu(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    candidate: &PrCandidate,
    target: &Target,
) -> Result<()> {
    let items = MENU_ACTIONS
        .iter()
        .map(|&action| {
            FinderItem::new(
                menu_display(messages, action).to_owned(),
                menu_key(action).to_owned(),
                // どの項目を選んでいても対象の PR が見えるよう、プレビューは項目ごとに
                // 変えず本文を出す（FR-32 と同じ扱い）
                PreviewSource::Text(candidate.body.clone()),
                language.messages(),
            )
            .with_panel(panel(messages, candidate))
        })
        .collect();

    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        &messages.pr().action_header_subject(candidate.number),
        messages.pr().action_header_outcome(),
    ));
    let selected = select_one_with(items, &options)?;

    let action = MENU_ACTIONS
        .iter()
        .find(|&&action| menu_key(action) == selected)
        .copied()
        .ok_or_else(|| anyhow!(messages.pr().selection_not_found(&selected)))?;

    perform(messages, repository, candidate, target, action)
}

/// メニューで選ばれた操作を実行する。
///
/// `view` / `diff` はネットワークを使うが、**プレビューではなく決定後の実行**であるため
/// 採用基準 C に反しない。標準出力へ書くのは番号と URL の 2 項目だけであり、
/// それ以外のメッセージはすべて標準エラーへ出す（パイプ用途を壊さない）。
fn perform(
    messages: &dyn Messages,
    repository: &gix::Repository,
    candidate: &PrCandidate,
    target: &Target,
    action: MenuAction,
) -> Result<()> {
    let number = candidate.key();

    match action {
        MenuAction::Checkout => checkout(messages, repository, candidate, target),
        MenuAction::View => Ok(gh::run_gh(&["pr", "view", &number])?),
        MenuAction::Diff => Ok(gh::run_gh(&["pr", "diff", &number])?),
        MenuAction::PrintNumber => print_line(messages, &number),
        MenuAction::PrintUrl => print_line(messages, &candidate.url),
    }
}

/// 標準出力へ 1 行書き出す（パイプ用途）。
fn print_line(messages: &dyn Messages, value: &str) -> Result<()> {
    writeln!(std::io::stdout(), "{value}").context(messages.common().stdout_write_failed())
}

/// 候補を取得する。
///
/// **採用基準 C の例外はここだけ**である。取得は数百ミリ秒〜数秒を要するため、
/// 何も起きていないように見せないよう先に 1 行示す（`gz fetch` が対象を示すのと同じ形）。
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
fn fetch_candidates(
    messages: &dyn Messages,
    checks: bool,
    writer: &mut impl std::io::Write,
) -> Result<Vec<PrCandidate>> {
    writeln!(writer, "{}", messages.pr().fetching())
        .context(messages.common().stderr_write_failed())?;

    let arguments = list_args(checks);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let stdout = gh::capture_gh(&arguments)?;

    parse(&stdout, checks)
}

/// 候補一覧から 1 件選ぶ。
fn select<'a>(
    language: Language,
    messages: &dyn Messages,
    candidates: &'a [PrCandidate],
    checks: bool,
) -> Result<&'a PrCandidate> {
    let rows = aligned_candidates(candidates, |candidate| cells(messages, candidate, checks));
    let items = rows
        .iter()
        .map(|(candidate, display)| to_item(language, messages, candidate, display))
        .collect();

    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.pr().header_subject(),
        messages.pr().header_outcome(),
    ));
    let selected = select_one_with(items, &options)?;

    // 番号は `--` で保護できない位置引数として `gh` へ渡るため、候補一覧に
    // 含まれることを確かめてから使う（design.md セキュリティ設計）
    candidates
        .iter()
        .find(|candidate| candidate.key() == selected)
        .ok_or_else(|| anyhow!(messages.pr().selection_not_found(&selected)))
}

/// 候補 1 件を列の並びへ分解する。
fn cells(messages: &dyn Messages, candidate: &PrCandidate, checks: bool) -> Vec<String> {
    let pr = messages.pr();
    let draft = if candidate.is_draft {
        pr.draft_label().to_owned()
    } else {
        pr.draft_placeholder()
    };

    let mut cells = vec![
        format!("#{number}", number = candidate.number),
        candidate.head_ref.clone(),
        candidate.author.clone(),
        draft,
    ];

    if checks {
        cells.push(review_text(messages, candidate));
        cells.push(checks_text(messages, candidate));
    }

    // タイトルは長さがまちまちであり、後続の列があると桁が読みにくくなるため必ず最終列に置く
    cells.push(candidate.title.clone());
    cells
}

/// レビュー判定の表示。
fn review_text(messages: &dyn Messages, candidate: &PrCandidate) -> String {
    match candidate.review_decision.as_deref() {
        None | Some("") => messages.pr().no_review().to_owned(),
        Some(decision) => decision.to_owned(),
    }
}

/// CI 集計の表示。
fn checks_text(messages: &dyn Messages, candidate: &PrCandidate) -> String {
    match candidate.checks {
        None => String::new(),
        Some(checks) => messages
            .pr()
            .checks_summary(checks.passed, checks.failed, checks.pending),
    }
}

/// 候補を finder のアイテムへ変換する。
///
/// プレビューは**取得済みの本文の描画だけ**であり、`gh` を起動しない
/// （[`PreviewSource::Text`] を参照）。
fn to_item(
    language: Language,
    messages: &dyn Messages,
    candidate: &PrCandidate,
    display: &str,
) -> FinderItem {
    FinderItem::new(
        display.to_owned(),
        candidate.key(),
        PreviewSource::Text(candidate.body.clone()),
        language.messages(),
    )
    .with_panel(panel(messages, candidate))
}

/// プレビュー先頭の要約枠を組み立てる。
fn panel(messages: &dyn Messages, candidate: &PrCandidate) -> PreviewPanel {
    let metric = format!("#{number}", number = candidate.number);
    // draft は「まだレビューを求めていない」という状態であり、
    // 通常の PR と取り違えないよう番号ごと色を変える
    let highlights = if candidate.is_draft {
        vec![Highlight::new(0, metric.len(), HighlightColor::Yellow)]
    } else {
        Vec::new()
    };

    PreviewPanel::new()
        .with_metric(metric, highlights)
        .with_context(vec![
            candidate.author.clone(),
            format!(
                "{section}: {base}{separator}<- {head}",
                section = messages.pr().branches_section(),
                base = candidate.base_ref,
                separator = COLUMN_SEPARATOR,
                head = candidate.head_ref
            ),
            candidate.url.clone(),
        ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用に `@tsv` の 1 行を組み立てる（区切りは実タブ）。
    fn line(fields: &[&str]) -> String {
        fields.join("\t")
    }

    fn base_fields() -> Vec<&'static str> {
        vec![
            "12",
            "feature/login",
            "main",
            "octocat",
            "false",
            "https://github.com/o/r/pull/12",
            "Add login",
            "body text",
        ]
    }

    #[test]
    fn the_default_run_does_not_ask_for_the_expensive_fields() {
        let args = list_args(false);
        let joined = args.join(" ");

        assert!(
            !joined.contains("reviewDecision"),
            "既定では reviewDecision を取得しない（+1.45 秒）: {joined}"
        );
        assert!(
            !joined.contains("statusCheckRollup"),
            "既定では statusCheckRollup を取得しない（+2.7 秒）: {joined}"
        );
        assert!(
            joined.contains("body"),
            "本文は候補取得に相乗りさせる（プレビューをローカルに閉じるため）: {joined}"
        );
    }

    #[test]
    fn the_checks_run_adds_the_expensive_fields() {
        let joined = list_args(true).join(" ");

        assert!(joined.contains("reviewDecision"));
        assert!(joined.contains("statusCheckRollup"));
    }

    #[test]
    fn the_arguments_stay_a_single_list_call() {
        let args = list_args(false);

        assert_eq!(&args[0..2], ["pr", "list"]);
        assert!(
            args.iter().any(|arg| arg == "--jq"),
            "パースは jq の @tsv に委ね、JSON クレートを足さない"
        );
    }

    #[test]
    fn a_base_line_becomes_a_candidate() {
        let stdout = line(&base_fields());
        let candidates = parse(stdout.as_bytes(), false).expect("8 フィールドの行はパースできる");

        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.number, 12);
        assert_eq!(candidate.head_ref, "feature/login");
        assert_eq!(candidate.base_ref, "main");
        assert_eq!(candidate.author, "octocat");
        assert!(!candidate.is_draft);
        assert_eq!(candidate.title, "Add login");
        assert_eq!(candidate.body, "body text");
        assert_eq!(candidate.review_decision, None);
        assert_eq!(candidate.checks, None);
    }

    #[test]
    fn a_body_with_newlines_and_tabs_survives_the_round_trip() {
        // `@tsv` は本文の改行・タブを 2 文字へ退避する。したがって 1 レコードは必ず 1 行に収まり、
        // 行分割が本文の改行に引っ掛かることは無い——この性質がパース全体の前提である
        let mut fields = base_fields();
        fields[7] = r"first\nsecond\twith tab";
        let stdout = line(&fields);

        let candidates = parse(stdout.as_bytes(), false).expect("退避された本文もパースできる");

        assert_eq!(candidates[0].body, "first\nsecond\twith tab");
    }

    #[test]
    fn several_records_are_split_on_real_newlines() {
        let mut second = base_fields();
        second[0] = "13";
        let stdout = format!("{}\n{}", line(&base_fields()), line(&second));

        let candidates = parse(stdout.as_bytes(), false).expect("複数行をパースできる");

        assert_eq!(
            candidates.iter().map(|c| c.number).collect::<Vec<_>>(),
            [12, 13]
        );
    }

    #[test]
    fn a_checks_line_carries_the_review_and_the_counts() {
        let mut fields = base_fields();
        fields.extend(["REVIEW_REQUIRED", "7", "1", "2"]);
        let stdout = line(&fields);

        let candidates = parse(stdout.as_bytes(), true).expect("12 フィールドの行はパースできる");

        assert_eq!(
            candidates[0].review_decision.as_deref(),
            Some("REVIEW_REQUIRED")
        );
        assert_eq!(
            candidates[0].checks,
            Some(Checks {
                passed: 7,
                failed: 1,
                pending: 2
            })
        );
    }

    #[test]
    fn a_line_with_the_wrong_field_count_stops_the_run() {
        let stdout = line(&base_fields()[0..5]);

        assert!(
            parse(stdout.as_bytes(), false).is_err(),
            "想定と違う行を黙って読み飛ばさない"
        );
    }

    #[test]
    fn a_checks_expression_is_not_accepted_by_the_base_field_count() {
        // `--checks` の有無でフィールド数が変わることを、実行時の「あるかもしれない」判定では
        // なく期待値の不一致として検出する
        let mut fields = base_fields();
        fields.extend(["REVIEW_REQUIRED", "7", "1", "2"]);

        assert!(parse(line(&fields).as_bytes(), false).is_err());
    }

    #[test]
    fn an_empty_output_yields_no_candidates() {
        assert!(
            parse(b"", false)
                .expect("空の出力はエラーではない")
                .is_empty()
        );
        assert!(
            parse(b"\n", false)
                .expect("空行だけの出力も同じ")
                .is_empty()
        );
    }

    #[test]
    fn a_draft_is_read_from_the_boolean_field() {
        let mut fields = base_fields();
        fields[4] = "true";

        let candidates = parse(line(&fields).as_bytes(), false).expect("パースできる");

        assert!(candidates[0].is_draft);
    }

    fn messages() -> &'static dyn Messages {
        Language::English.messages()
    }

    fn candidate(fields: &[&str], checks: bool) -> PrCandidate {
        parse(line(fields).as_bytes(), checks)
            .expect("テスト用の行はパースできること")
            .remove(0)
    }

    #[test]
    fn the_title_is_always_the_last_column() {
        // タイトルは長さがまちまちであり、後続の列があると桁が読みにくくなる
        let plain = cells(messages(), &candidate(&base_fields(), false), false);
        assert_eq!(plain.last().map(String::as_str), Some("Add login"));

        let mut with_checks = base_fields();
        with_checks.extend(["APPROVED", "3", "0", "0"]);
        let checked = cells(messages(), &candidate(&with_checks, true), true);
        assert_eq!(checked.last().map(String::as_str), Some("Add login"));
    }

    #[test]
    fn the_draft_marker_keeps_the_column_width_constant() {
        // draft の有無で桁がずれると、一覧の右側の列が行ごとに動いて読めなくなる
        let mut draft_fields = base_fields();
        draft_fields[4] = "true";

        let draft = cells(messages(), &candidate(&draft_fields, false), false);
        let plain = cells(messages(), &candidate(&base_fields(), false), false);

        assert_eq!(draft[3].chars().count(), plain[3].chars().count());
        assert_eq!(draft[3], messages().pr().draft_label());
        assert!(plain[3].trim().is_empty());
    }

    #[test]
    fn the_check_columns_appear_only_with_the_flag() {
        let plain = cells(messages(), &candidate(&base_fields(), false), false);

        let mut with_checks = base_fields();
        with_checks.extend(["APPROVED", "3", "1", "2"]);
        let checked = cells(messages(), &candidate(&with_checks, true), true);

        assert_eq!(
            checked.len(),
            plain.len() + 2,
            "レビューと CI の 2 列だけ増える"
        );
        assert_eq!(checked[4], "APPROVED");
        assert!(checked[5].contains('3') && checked[5].contains('1') && checked[5].contains('2'));
    }

    #[test]
    fn a_pull_request_without_a_review_shows_a_placeholder() {
        let mut fields = base_fields();
        fields.extend(["", "0", "0", "0"]);

        let row = cells(messages(), &candidate(&fields, true), true);

        assert_eq!(row[4], messages().pr().no_review());
    }

    #[test]
    fn the_preview_never_launches_gh() {
        // 採用基準 C の例外は候補生成の 1 回だけである。プレビューが外部コマンドを
        // 起動しないことを、`PreviewSource` の variant で構造的に固定する
        let candidate = candidate(&base_fields(), false);
        let item = to_item(Language::English, messages(), &candidate, "#12  feature");

        assert_eq!(
            item.preview_source(),
            &PreviewSource::Text("body text".to_owned()),
            "プレビューは取得済み本文の描画に閉じること"
        );
    }

    #[test]
    fn the_panel_shows_where_the_pull_request_would_land() {
        let candidate = candidate(&base_fields(), false);
        let panel = panel(messages(), &candidate);
        let rendered = format!("{panel:?}");

        assert!(rendered.contains("#12"));
        assert!(rendered.contains("octocat"));
        assert!(
            rendered.contains("main"),
            "取り込み先が読めること: {rendered}"
        );
        assert!(rendered.contains("feature/login"), "取り込み元が読めること");
        assert!(rendered.contains("https://github.com/o/r/pull/12"));
    }

    #[test]
    fn the_menu_keys_are_unique_and_independent_of_the_language() {
        let keys: Vec<&str> = MENU_ACTIONS.iter().map(|&a| menu_key(a)).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();

        assert_eq!(
            sorted.len(),
            keys.len(),
            "キーが重複すると別の操作を実行し得る"
        );
        assert!(
            keys.iter().all(|key| key.is_ascii()),
            "照合キーは翻訳の対象にしない: {keys:?}"
        );
    }

    #[test]
    fn every_menu_action_has_a_display_string_in_both_languages() {
        for language in [Language::English, Language::Japanese] {
            for &action in &MENU_ACTIONS {
                assert!(
                    !menu_display(language.messages(), action).is_empty(),
                    "{action:?} の表示文字列が {language:?} で空"
                );
            }
        }
    }

    #[test]
    fn the_menu_order_does_not_depend_on_the_pull_request() {
        // 項目が毎回同じ並びであることが選び間違いを防ぐ（FR-16 / FR-32 の方針）
        assert_eq!(MENU_ACTIONS[0], MenuAction::Checkout);
        assert_eq!(MENU_ACTIONS.len(), 5);
    }

    #[test]
    fn the_fetching_notice_goes_to_the_writer_before_anything_else() {
        // 取得は数百ミリ秒〜数秒掛かる。何も起きていないように見せない
        let mut written = Vec::new();
        // `gh` を起動せずに 1 行目だけを確かめたいため、書き出しだけを切り出して呼ぶ
        writeln!(&mut written, "{}", messages().pr().fetching()).expect("書き出せること");

        assert_eq!(
            String::from_utf8_lossy(&written).trim(),
            messages().pr().fetching()
        );
    }

    #[test]
    fn the_key_is_the_number_and_is_never_translated() {
        let candidates = parse(line(&base_fields()).as_bytes(), false).expect("パースできる");

        assert_eq!(candidates[0].key(), "12");
    }
}
