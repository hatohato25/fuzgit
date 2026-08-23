//! 各サブコマンドのユースケース実装。
//!
//! 各コマンドは「`git::read` で候補取得 → `finder` で選択 → `git::exec` で実行」の
//! 直列オーケストレーションのみを担う。

use anyhow::Result;
use unicode_width::UnicodeWidthStr as _;

use crate::cli::{Command, StashCommand};
use crate::commands::diff::DiffMode;
use crate::commands::fetch::{FetchScope, PruneMode};
use crate::commands::fixup::FixupKind;
use crate::commands::merge::MergeMode;
use crate::commands::pull::PullMode;
use crate::commands::restore::RestoreTarget;
use crate::commands::revert::MessageEditing;
use crate::commands::stash::{StashAction, UntrackedFiles};
use crate::error;
use crate::finder::{Highlight, HighlightColor};
use crate::git::read::{BranchScope, CommitInfo, CommitScope};
use crate::i18n::{Language, Messages};

pub mod add;
pub mod branch;
pub mod branch_manage;
pub mod cherry_pick;
pub mod commit;
pub mod commit_menu;
pub mod confirmation;
pub mod diff;
pub mod fetch;
pub mod file_selection;
pub mod fixup;
pub mod in_progress;
pub mod log;
pub mod merge;
pub mod pull;
pub mod rebase;
pub mod reflog;
pub mod restore;
pub mod revert;
pub mod stash;
pub mod status;
pub mod worktree;
pub mod worktree_claude;
pub mod worktree_install;

/// 候補一覧の列を区切る空白。
///
/// 候補一覧の体裁をコマンド間で揃えるため、区切りはこの 1 か所だけに持つ。
pub(crate) const COLUMN_SEPARATOR: &str = "  ";

/// 候補一覧のヘッダーで節を区切る記号。
///
/// 複数選択（`gz pull` / `gz fetch --siblings` / `gz status`）と単一選択
/// （[`selection_header`]）が同じ見た目のヘッダーを出すため、[`COLUMN_SEPARATOR`] と
/// 同じくコマンド間で 1 か所に持つ。
pub(crate) const HEADER_SEPARATOR: &str = "  |  ";

/// 決定キーの表記。
///
/// キー名は端末が送るキーの名前であり訳さないため、言語ごとの文言（`crate::i18n`）ではなく
/// ヘッダーの骨格としてここに持つ。
const DECIDE_KEY: &str = "Enter: ";

/// 単一選択の候補一覧に固定表示するヘッダーを組み立てる。
///
/// 単一選択には Tab による選択の切替が無く、候補行だけでは「何を選ばされているのか」も
/// 「決定すると何が起きるのか」も読み取れない。`subject`（選ぶ対象）と `outcome`
/// （Enter で起きること）を必ず対で受け取り、片方だけのヘッダーが生まれないようにする。
///
/// `outcome` は**実際に行う操作**を渡すこと。同じ一覧でもフラグで結果が変わるコマンド
/// （`gz stash` / `gz reflog` / `gz log` / `gz branch create`）は、選択前に見えている説明と
/// 決定後の挙動が食い違うと、取り返しの付かない操作を承知せずに実行させることになる。
pub(crate) fn selection_header(subject: &str, outcome: &str) -> String {
    format!("{subject}{HEADER_SEPARATOR}{DECIDE_KEY}{outcome}")
}

/// 候補と、列の幅を候補一覧全体で揃えた表示行の対を組み立てる。
///
/// `cells` は候補 1 件を列の並びへ分解する。返るのは候補とその表示行の**対**であり、
/// finder へ渡す候補行（[`crate::finder::FinderItem`] の `display`）と事前選択
/// （[`crate::finder::FinderOptions::preselect`]）を必ず同じ値から作れる。
/// 事前選択は表示文字列の完全一致で判定される一方、列の幅は候補一覧全体に依存するため、
/// 両者を別々に組み立てると幅がずれた瞬間に一致しなくなる。
pub(crate) fn aligned_candidates<T>(
    candidates: &[T],
    cells: impl Fn(&T) -> Vec<String>,
) -> Vec<(&T, String)> {
    let rows: Vec<Vec<String>> = candidates.iter().map(cells).collect();

    // `align_columns` は行数を変えないため、対応関係は候補の並びのまま保たれる
    candidates.iter().zip(align_columns(&rows)).collect()
}

/// 行ごとの列を、列ごとの最大表示幅に合わせて空白で埋めながら連結する。
///
/// 幅は文字数ではなく端末上の表示幅で測る（全角文字は 2 セル幅を占めるため、
/// 文字数で埋めると日本語を含む候補で桁が合わない）。
/// 各行の**最終列は埋めない**（行末に意味のない空白を残さないため）。
fn align_columns(rows: &[Vec<String>]) -> Vec<String> {
    let widths = column_widths(rows);

    rows.iter().map(|row| join_row(row, &widths)).collect()
}

/// 後続の列を持つ列について、列ごとの最大表示幅を求める。
///
/// 幅は「次の列の開始位置を揃える」ためだけに必要であるため、最終列は測らない。
fn column_widths(rows: &[Vec<String>]) -> Vec<usize> {
    let mut widths: Vec<usize> = Vec::new();

    for row in rows {
        let Some((_last, leading)) = row.split_last() else {
            continue;
        };

        for (index, cell) in leading.iter().enumerate() {
            let width = cell.width();
            match widths.get_mut(index) {
                Some(current) => *current = (*current).max(width),
                // 列は先頭から順に見るため、未知の列は必ず末尾への追加になる
                None => widths.push(width),
            }
        }
    }

    widths
}

/// 1 行分の列を、与えられた幅に合わせて連結する。
///
/// `widths` は [`column_widths`] が同じ行集合から求めた値であり、各行の最終列を除く
/// すべての列を必ず覆う。
fn join_row(row: &[String], widths: &[usize]) -> String {
    let Some((last, leading)) = row.split_last() else {
        // 列を 1 つも持たない候補は空行になる
        return String::new();
    };

    let mut line = String::new();
    for (cell, width) in leading.iter().zip(widths) {
        line.push_str(&pad(cell, *width));
        line.push_str(COLUMN_SEPARATOR);
    }
    line.push_str(last);

    line
}

/// 表示幅が `width` になるまで右側を空白で埋める。
///
/// `width` は同じ列の最大幅であるため `cell` の幅を下回らないが、下回った場合に
/// 引き算で破綻させないよう飽和演算で扱う（埋めないだけで内容は削らない）。
fn pad(cell: &str, width: usize) -> String {
    let mut padded = cell.to_owned();
    padded.push_str(&" ".repeat(width.saturating_sub(cell.width())));

    padded
}

/// コミット候補の表示行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// `gz log` / `gz cherry-pick` / `gz revert` / `gz fixup` / `gz diff --commit` は同じ形の
/// コミット一覧を出すため、書式と配色（[`commit_highlights`]）をここへ集約する。
/// 片方だけを変えると同じ見た目の一覧でコマンドごとに色が食い違うため、両者は必ず対で使う。
///
/// コミットメッセージでの検索を主用途とするため、サマリを作者より前に置く。
pub(crate) fn commit_line(commit: &CommitInfo) -> String {
    format!(
        "{short_id} {time} {summary} ({author})",
        short_id = commit.short_id,
        time = commit.time,
        summary = commit.summary,
        author = commit.author
    )
}

/// [`commit_line`] のうち色を付ける範囲。
///
/// ハッシュを黄、日付を緑にする。これは fuzgit が独自に決めた配色ではなく、git 自身が
/// `git log` で用いる配色（コミットハッシュが黄）と、`--pretty=format` の慣用例
/// （`%C(yellow)%h %C(green)%ad`）に合わせたもの。サマリと作者は色を付けない
/// （一覧の大半を占めるため、色を付けると逆に目印が埋もれる）。
///
/// 範囲はバイト位置である。短縮ハッシュ（16 進数）と日付（`YYYY-MM-DD`）はいずれも
/// ASCII であり、後続のサマリに多バイト文字が含まれても前 2 列の位置には影響しない。
pub(crate) fn commit_highlights(commit: &CommitInfo) -> Vec<Highlight> {
    let hash_end = commit.short_id.len();
    // 区切りの空白 1 文字を挟んで日付が始まる
    let time_start = hash_end + 1;
    let time_end = time_start + commit.time.len();

    vec![
        Highlight::new(0, hash_end, HighlightColor::Yellow),
        Highlight::new(time_start, time_end, HighlightColor::Green),
    ]
}

/// これから実行する git コマンドを表示用の 1 行に整形する。
///
/// 確認プロンプトや復帰メニューで「何が実行されるのか」を示すために用いる。
/// 表示専用であり、この文字列をコマンドとして実行することはない（実行は常に引数配列渡し）。
/// 実行する引数配列そのものから組み立てるため、説明と実際の操作が食い違わない。
pub(crate) fn command_display(args: &[&str]) -> String {
    format!("git {args}", args = args.join(" "))
}

/// 選択中のコミットを色付きで示す `git show` の引数を組み立てる（プレビュー用）。
///
/// `gz log` / `gz reflog` の候補一覧と、コミット選択後のアクションメニュー（FR-32）が
/// 同じプレビューを出すため、組み立てを 1 か所に持つ。
///
/// 末尾の `--` により、ハッシュがパスではなくリビジョンとして解釈されることを保証する。
/// 出力をキャプチャして実行する場合 git は色付けを止めるため、明示的に有効化する
/// （**実行用の `git show` には付けない**。そちらは端末へ直接出るため git が自ら判断する）。
pub(crate) fn commit_preview_args(id: &str) -> Vec<String> {
    ["show", "--color=always", id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// 現在の状態を色付きで示す `git status` の引数を組み立てる。
///
/// 固定項目のメニュー（FR-14 の復帰メニュー、FR-16 のアクションメニュー）で、
/// どの項目を選んでいても現在の状態（未解決 / 解決済み・staged / unstaged の区別を含む
/// 短縮表記）が見えるようにするために用いる。
/// 出力をキャプチャして実行する場合 git は色付けを止めるため、明示的に有効化する。
pub(crate) fn status_preview_args() -> Vec<String> {
    ["-c", "color.status=always", "status", "--short", "--branch"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// サブコマンドを対応する実装へ振り分ける。
///
/// `repository` は `main` が開いた結果をそのまま受け取る。リポジトリを開く位置が `main` に
/// あるのは、表示言語の解決が `git config fuzgit.lang` を必要とし、かつリポジトリ外でも
/// 成立しなければならないため（design.md「起動シーケンス」）。**開けなかった場合の判定は
/// 従来どおりここで行う**ので、サブコマンドから見た挙動は変わらない。
///
/// # Errors
///
/// リポジトリのオープンに失敗した場合、および各サブコマンドの処理が失敗した場合にエラーを返す。
pub fn dispatch(
    language: Language,
    messages: &dyn Messages,
    repository: error::Result<gix::Repository>,
    command: &Command,
) -> Result<()> {
    // すべてのサブコマンドが git リポジトリ内での実行を前提とするため、
    // 個別処理へ入る前にここで一度だけ検証してエラーメッセージを統一する
    let repository = repository?;

    match command {
        // サブコマンドなしは従来どおりの切替（FR-1）。管理操作は branch_manage が担う
        Command::Branch { all, command } => match command {
            None => {
                let scope = if *all {
                    BranchScope::All
                } else {
                    BranchScope::Local
                };
                branch::run(language, messages, &repository, scope)
            }
            Some(command) => branch_manage::run(language, messages, &repository, command),
        },
        Command::Log { limit, action } => log::run(
            language,
            messages,
            &repository,
            *limit,
            log::Decision::from_flag(*action),
        ),
        Command::CherryPick { branch } => {
            let scope = match branch {
                Some(name) => CommitScope::Branch(name),
                None => CommitScope::AllBranches,
            };
            cherry_pick::run(language, messages, &repository, scope)
        }
        Command::Restore { source, staged } => {
            let target = if *staged {
                RestoreTarget::Index
            } else {
                RestoreTarget::Worktree
            };
            restore::run(language, messages, &repository, target, source.as_deref())
        }
        Command::Add => add::run(language, messages, &repository),
        Command::Stash { command } => match command {
            StashCommand::Push {
                message,
                include_untracked,
            } => {
                let untracked = if *include_untracked {
                    UntrackedFiles::Include
                } else {
                    UntrackedFiles::Exclude
                };
                stash::push(
                    language,
                    messages,
                    &repository,
                    message.as_deref(),
                    untracked,
                )
            }
            StashCommand::Apply => stash::run(language, messages, &repository, StashAction::Apply),
            StashCommand::Pop => stash::run(language, messages, &repository, StashAction::Pop),
            StashCommand::Drop => stash::run(language, messages, &repository, StashAction::Drop),
        },
        Command::Reflog { restore, action } => {
            let decision = reflog::Decision::from_flags(messages, restore.as_deref(), *action)?;
            reflog::run(language, messages, &repository, decision)
        }
        Command::Commit { message } => {
            commit::run(language, messages, &repository, message.as_deref())
        }
        Command::Fixup { squash } => {
            let kind = if *squash {
                FixupKind::Squash
            } else {
                FixupKind::Fixup
            };
            fixup::run(language, messages, &repository, kind)
        }
        Command::Merge {
            no_ff,
            squash,
            ff_only,
        } => merge::run(
            language,
            messages,
            &repository,
            MergeMode::from_flags(messages, *no_ff, *squash, *ff_only)?,
        ),
        Command::Rebase => rebase::run(language, messages, &repository),
        Command::Revert { no_edit } => {
            let editing = if *no_edit {
                MessageEditing::Skip
            } else {
                MessageEditing::Interactive
            };
            revert::run(language, messages, &repository, editing)
        }
        Command::Status => status::run(language, messages, &repository),
        Command::Diff {
            staged,
            head,
            upstream,
            branch,
            commit,
        } => diff::run(
            language,
            messages,
            &repository,
            DiffMode::from_flags(messages, *staged, *head, *upstream, *branch, *commit)?,
        ),
        Command::Fetch { prune, siblings } => {
            let prune = if *prune {
                PruneMode::Prune
            } else {
                PruneMode::Keep
            };
            let scope = if *siblings {
                FetchScope::Siblings
            } else {
                FetchScope::Current
            };
            fetch::run(language, messages, &repository, scope, prune)
        }
        Command::Pull { rebase, merge } => pull::run(
            language,
            messages,
            &repository,
            PullMode::from_flags(messages, *rebase, *merge)?,
        ),
        Command::Worktree { command } => {
            worktree::run(language, messages, &repository, command.as_ref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 列の並びを行として組み立てる。
    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|cell| (*cell).to_owned()).collect()
    }

    #[test]
    fn a_single_selection_header_states_the_subject_and_what_enter_does() {
        let header = selection_header("切り替えるブランチを選択", "切り替え");

        assert_eq!(header, "切り替えるブランチを選択  |  Enter: 切り替え");
    }

    #[test]
    fn a_single_selection_header_stays_on_one_line() {
        // ヘッダーは候補一覧の上に 1 行だけ表示される領域であり、折り返させない
        let header = selection_header("Pick a stash", "drop it after a confirmation");

        assert_eq!(header.lines().count(), 1, "1 行に収める: {header}");
        assert!(
            header.contains(HEADER_SEPARATOR),
            "the sections should be separated: {header}"
        );
    }

    #[test]
    fn a_column_is_padded_to_the_widest_value_in_the_list() {
        let lines = align_columns(&[
            row(&["fuzgit", "origin/main"]),
            row(&["advent-calendar", "origin/master"]),
            row(&["book-viewer", "hatohato25/main"]),
        ]);

        assert_eq!(
            lines,
            [
                "fuzgit           origin/main",
                "advent-calendar  origin/master",
                "book-viewer      hatohato25/main",
            ]
        );
    }

    #[test]
    fn a_full_width_value_is_measured_by_how_wide_it_looks_not_by_how_many_characters_it_has() {
        // 「リリース」は 4 文字だが端末では 8 セルを占める。文字数で埋めると桁が合わない
        let lines = align_columns(&[row(&["リリース", "v1.0"]), row(&["release", "v2.0"])]);

        assert_eq!(lines, ["リリース  v1.0", "release   v2.0"]);
    }

    #[test]
    fn the_last_column_is_never_padded() {
        let lines = align_columns(&[row(&["a", "short"]), row(&["b", "much longer value"])]);

        for line in &lines {
            assert_eq!(line.trim_end(), line, "行末に空白を残さない: {line:?}");
        }
    }

    #[test]
    fn a_row_that_ends_early_does_not_widen_the_column_it_ends_in() {
        // タグ一覧のように、メッセージを持たない候補は名前が最終列になる
        let lines = align_columns(&[row(&["v1.0", "リリース v1.0"]), row(&["v2.0-lightweight"])]);

        assert_eq!(lines, ["v1.0  リリース v1.0", "v2.0-lightweight"]);
    }

    #[test]
    fn a_row_with_more_columns_than_the_others_is_aligned_up_to_its_last_column() {
        let lines = align_columns(&[
            row(&["alpha", "origin/main"]),
            row(&["bravo", "origin, upstream", "detached HEAD"]),
            row(&["charlie", "origin", "detached HEAD"]),
        ]);

        assert_eq!(
            lines,
            [
                "alpha    origin/main",
                "bravo    origin, upstream  detached HEAD",
                "charlie  origin            detached HEAD",
            ]
        );
    }

    #[test]
    fn a_single_candidate_is_left_as_it_is() {
        assert_eq!(
            align_columns(&[row(&["only", "origin/main"])]),
            ["only  origin/main"]
        );
    }

    #[test]
    fn an_empty_list_produces_no_line() {
        assert!(align_columns(&[]).is_empty());
    }

    #[test]
    fn a_candidate_without_any_column_produces_an_empty_line() {
        assert_eq!(align_columns(&[Vec::new()]), [""]);
    }

    #[test]
    fn every_candidate_keeps_its_own_line() {
        let candidates = ["mike", "advent-calendar"];

        let aligned = aligned_candidates(&candidates, |name| row(&[name, "origin/main"]));

        assert_eq!(
            aligned,
            [
                (&"mike", "mike             origin/main".to_owned()),
                (
                    &"advent-calendar",
                    "advent-calendar  origin/main".to_owned()
                ),
            ]
        );
    }

    /// 色の範囲を検証するためのコミット。
    fn commit(short_id: &str, time: &str, summary: &str) -> CommitInfo {
        CommitInfo {
            id: format!("{short_id}0000000000000000000000000000000"),
            short_id: short_id.to_owned(),
            summary: summary.to_owned(),
            author: "hatohato25".to_owned(),
            time: time.to_owned(),
        }
    }

    #[test]
    fn a_commit_line_puts_the_hash_and_the_date_first() {
        assert_eq!(
            commit_line(&commit("1f0c9a4", "2026-08-20", "add the notify feature")),
            "1f0c9a4 2026-08-20 add the notify feature (hatohato25)"
        );
    }

    #[test]
    fn the_hash_is_yellow_and_the_date_is_green() {
        // git 自身の配色に合わせる（`git log` のハッシュが黄、`%C(green)%ad` が慣用）
        let highlights = commit_highlights(&commit("1f0c9a4", "2026-08-20", "add the feature"));

        assert_eq!(
            highlights,
            [
                Highlight::new(0, 7, HighlightColor::Yellow),
                Highlight::new(8, 18, HighlightColor::Green),
            ]
        );
    }

    /// 行を独立に走査して、ハッシュ列と日付列のバイト位置を求める。
    ///
    /// `commit_highlights` と同じ計算を書き写すと、両方が同じ間違いをしても気付けない。
    fn ranges_of(line: &str) -> (usize, usize, usize) {
        let hash_end = line.find(' ').expect("the hash is followed by a separator");
        let time_start = hash_end + 1;
        let time_end = time_start
            + line[time_start..]
                .find(' ')
                .expect("the date is followed by a separator");

        (hash_end, time_start, time_end)
    }

    #[test]
    fn the_coloured_ranges_match_the_line_they_describe() {
        // 範囲はバイト位置。サマリの多バイト文字が前 2 列の位置へ影響しないことも同時に見る
        let commit = commit("9bde988", "2026-08-07", "fuzgit 基盤と P1 フェーズ実装");
        let (hash_end, time_start, time_end) = ranges_of(&commit_line(&commit));

        assert_eq!(
            commit_highlights(&commit),
            [
                Highlight::new(0, hash_end, HighlightColor::Yellow),
                Highlight::new(time_start, time_end, HighlightColor::Green),
            ]
        );
    }

    #[test]
    fn a_shorter_hash_moves_the_date_range_with_it() {
        // 短縮ハッシュの長さは core.abbrev で変わるため、固定長を前提にしない
        let commit = commit("1f0c9", "2026-08-20", "add the feature");
        let (hash_end, time_start, time_end) = ranges_of(&commit_line(&commit));

        assert_eq!(hash_end, 5, "the hash column follows the short id");
        assert_eq!(
            commit_highlights(&commit),
            [
                Highlight::new(0, hash_end, HighlightColor::Yellow),
                Highlight::new(time_start, time_end, HighlightColor::Green),
            ]
        );
    }
}
