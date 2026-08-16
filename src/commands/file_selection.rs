//! `gz restore` / `gz add` で共用する、変更ファイル候補の整形と選択結果の解決。
//!
//! 両コマンドとも「作業ツリールート基準のパスを候補として見せ、選択結果を
//! git へ渡すパスへ戻す」処理が同じであるため、ここへまとめて単体テストの対象とする。

use anyhow::{Result, bail};

use crate::finder::{Highlight, HighlightColor};
use crate::git::read::FileChange;
use crate::i18n::Messages;

/// リネーム・コピーの変更元パスを git の対象に含めるかどうか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameOrigin {
    /// 変更元も対象に含める。
    ///
    /// ステージ済みのリネームをアンステージする場合、変更後のパスだけを指定すると
    /// 変更元の削除がインデックスに残るため、両方を指定して初めて元の状態へ戻せる。
    Include,
    /// 変更後のパスのみを対象にする。
    ///
    /// 作業ツリーの復元・ステージでは、変更元のパスはインデックスに存在せず
    /// 指定しても一致しないため含めない。
    Exclude,
}

/// finder に見せる 1 候補。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCandidate {
    /// 一覧表示および絞り込み対象の文字列。
    pub display: String,
    /// finder のキー。作業ツリールート基準のパス（確認プロンプトの表示にも使う）。
    pub key: String,
    /// git へ渡すパス（作業ツリールート基準）。リネームでは変更元を含むことがある。
    pub paths: Vec<String>,
    /// 表示文字列のうち色を付ける範囲（状態コードの列）。状態コードを持たない候補では空。
    pub highlights: Vec<Highlight>,
}

impl FileCandidate {
    /// 変更ファイルを候補へ変換する。
    #[must_use]
    pub fn from_change(change: &FileChange, origin: RenameOrigin) -> Self {
        let mut paths = vec![change.path.clone()];
        if let (RenameOrigin::Include, Some(original)) = (origin, change.original_path.as_ref()) {
            paths.push(original.clone());
        }

        Self {
            display: display_line(change),
            key: change.path.clone(),
            paths,
            highlights: status_highlights(change),
        }
    }

    /// リビジョン内のファイルパスを候補へ変換する。
    ///
    /// `git restore --source <rev>` の対象は現在の変更有無に依らないため、状態コードを持たない。
    #[must_use]
    pub fn from_path(path: &str) -> Self {
        Self {
            display: path.to_owned(),
            key: path.to_owned(),
            paths: vec![path.to_owned()],
            // 状態コードが無いので色を付ける対象も無い
            highlights: Vec::new(),
        }
    }
}

/// 状態コードの列数。左詰めにしても幅は変えない（パスの開始位置を揃えるため）。
const STATUS_CODE_WIDTH: usize = 2;

/// 状態コードのうち「その列に変更が無い」ことを表す文字。
const UNCHANGED_COLUMN: char = ' ';

/// 状態コードを左詰めにした [`STATUS_CODE_WIDTH`] 文字を返す。
///
/// 変更の無い列（` `）を落として意味のある文字を先頭へ寄せ、右側を空白で埋める。
/// ` M` は `M ` に、`M ` / `MM` / `??` / `R ` はそのままになる。
fn aligned_status_code(change: &FileChange) -> String {
    let changed: String = change
        .status_code()
        .chars()
        .filter(|column| *column != UNCHANGED_COLUMN)
        .collect();

    format!("{changed:<STATUS_CODE_WIDTH$}")
}

/// 状態コードに付ける色を組み立てる。
///
/// index 列は緑、作業ツリー列は赤、未追跡（`??`）は 2 文字とも赤。
/// これは fuzgit が独自に決めた配色ではなく、git 自身の既定の配色
/// （`git -c color.status=always status --short` の出力で確認）に合わせたもの。
/// 変更の無い列（` `）は git と同様に色を付けない。
///
/// 範囲は [`display_line`] が出力する左詰めの状態コード上の位置である。
/// index 列に変更が無いときは作業ツリー列の文字が先頭へ寄るため、赤も先頭の 1 文字に付く。
/// 状態コードは ASCII であり、1 文字が 1 バイトを占める。
fn status_highlights(change: &FileChange) -> Vec<Highlight> {
    if change.is_untracked() {
        return vec![Highlight::new(0, STATUS_CODE_WIDTH, HighlightColor::Red)];
    }

    let mut highlights = Vec::new();
    if change.has_staged_change() {
        highlights.push(Highlight::new(0, 1, HighlightColor::Green));
    }
    if change.has_worktree_change() {
        // 左詰めのため、index 列に変更が無ければ作業ツリー列の文字が先頭へ来る
        let start = usize::from(change.has_staged_change());
        highlights.push(Highlight::new(start, start + 1, HighlightColor::Red));
    }
    highlights
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// 状態コードを前置し、リネーム・コピーは変更元も併記する。
///
/// 状態コードは git の `--porcelain` と同じ 2 文字だが、表記は**意図的に外している**。
/// git は 1 文字目を index 列、2 文字目を作業ツリー列に固定し、変更の無い列を空白にするため、
/// 作業ツリーだけの変更は ` M` と先頭が空白になる。fuzgit ではこれを左詰めにして `M ` と表示し、
/// 状態コードが行の先頭から始まるようにする（列幅は 2 のまま保つ）。
///
/// この結果、`M ` が staged（index 側）なのか unstaged（作業ツリー側）なのかを
/// **文字の位置では区別できなくなる**。区別は [`status_highlights`] が付ける色だけが担う
/// （staged は緑、unstaged は赤）。したがって色を使えない環境
/// （`NO_COLOR`、パイプ経由、色覚特性など）では両者が同じ見た目になる。
fn display_line(change: &FileChange) -> String {
    match &change.original_path {
        Some(original) => format!(
            "{code} {original} -> {path}",
            code = aligned_status_code(change),
            path = change.path
        ),
        None => format!(
            "{code} {path}",
            code = aligned_status_code(change),
            path = change.path
        ),
    }
}

/// 選択されたキーに対応する要素を、一覧の並び順で返す。
///
/// # Errors
///
/// 選択されたキーが一覧に含まれない場合にエラーを返す（対象を取り違えたまま
/// git 操作を実行しないよう、暗黙に読み飛ばさない）。
fn resolve_by_key<'a, T>(
    messages: &dyn Messages,
    items: &'a [T],
    selected: &[String],
    key: impl Fn(&T) -> &str,
) -> Result<Vec<&'a T>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|selected| !items.iter().any(|item| key(item) == selected.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            messages
                .file_selection()
                .selection_not_found(&missing.join(", "))
        );
    }

    Ok(items
        .iter()
        .filter(|item| selected.iter().any(|selected| selected == key(item)))
        .collect())
}

/// 選択されたキーに対応する候補を、候補一覧の並び順で返す。
///
/// # Errors
///
/// [`resolve_by_key`] と同じ。
pub fn resolve<'a>(
    messages: &dyn Messages,
    candidates: &'a [FileCandidate],
    selected: &[String],
) -> Result<Vec<&'a FileCandidate>> {
    resolve_by_key(messages, candidates, selected, |candidate| {
        candidate.key.as_str()
    })
}

/// 選択されたパスに対応する変更ファイルを、一覧の並び順で返す。
///
/// 候補が [`FileCandidate::from_change`] 由来（キー＝パス）のコマンドは、選択結果を
/// [`FileChange`] のまま受け取れる。git へ渡すパスの決め方（リネーム元を含めるか）は
/// 実行するコマンドごとに異なるため、その判断を各コマンド側に残したまま
/// 選択結果を受け渡すために用いる（`gz status` のアクションメニュー）。
///
/// # Errors
///
/// [`resolve_by_key`] と同じ。
pub fn resolve_changes<'a>(
    messages: &dyn Messages,
    changes: &'a [FileChange],
    selected: &[String],
) -> Result<Vec<&'a FileChange>> {
    resolve_by_key(messages, changes, selected, |change| change.path.as_str())
}

/// 選択された候補が git の対象とするパスを、重複を除いて集める。
#[must_use]
pub fn target_paths(selected: &[&FileCandidate]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for path in selected.iter().flat_map(|candidate| candidate.paths.iter()) {
        if !paths.contains(path) {
            paths.push(path.clone());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    fn change(path: &str, code: &str) -> FileChange {
        let mut codes = code.chars();
        FileChange {
            path: path.to_owned(),
            original_path: None,
            index_status: codes.next().expect("a status code has two characters"),
            worktree_status: codes.next().expect("a status code has two characters"),
        }
    }

    fn rename(path: &str, original: &str, code: &str) -> FileChange {
        FileChange {
            original_path: Some(original.to_owned()),
            ..change(path, code)
        }
    }

    fn candidates() -> Vec<FileCandidate> {
        ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|path| FileCandidate::from_path(path))
            .collect()
    }

    fn keys(selected: &[&FileCandidate]) -> Vec<String> {
        selected
            .iter()
            .map(|candidate| candidate.key.clone())
            .collect()
    }

    #[test]
    fn a_line_shows_the_status_code_before_the_path() {
        assert_eq!(display_line(&change("src/main.rs", "M ")), "M  src/main.rs");
        assert_eq!(display_line(&change("src/main.rs", "MM")), "MM src/main.rs");
        assert_eq!(
            display_line(&change("new file.txt", "??")),
            "?? new file.txt"
        );
    }

    #[test]
    fn a_worktree_only_change_shows_its_status_code_left_aligned() {
        // git の ` M` と違い、意味のある文字を先頭へ寄せる
        assert_eq!(display_line(&change("src/main.rs", " M")), "M  src/main.rs");
        assert_eq!(
            display_line(&rename("new.txt", "old.txt", " R")),
            "R  old.txt -> new.txt"
        );
    }

    #[test]
    fn the_status_code_always_takes_two_columns() {
        // パスの開始位置が揃うよう、左詰めにしても幅は 2 のまま
        for code in ["M ", " M", "MM", "??", "R "] {
            let line = display_line(&change("a.txt", code));

            assert_eq!(
                &line[..STATUS_CODE_WIDTH],
                &aligned_status_code(&change("a.txt", code))
            );
            assert_eq!(
                line.find("a.txt"),
                Some(STATUS_CODE_WIDTH + 1),
                "the path must start at the same column for {code:?}"
            );
        }
    }

    #[test]
    fn a_rename_line_shows_both_paths() {
        assert_eq!(
            display_line(&rename("new.txt", "old.txt", "R ")),
            "R  old.txt -> new.txt"
        );
    }

    #[test]
    fn the_index_column_is_green_and_the_worktree_column_is_red() {
        // git 自身の `status --short` の配色に合わせる
        assert_eq!(
            FileCandidate::from_change(&change("a.txt", "M "), RenameOrigin::Exclude).highlights,
            [Highlight::new(0, 1, HighlightColor::Green)]
        );
        assert_eq!(
            FileCandidate::from_change(&change("a.txt", "MM"), RenameOrigin::Exclude).highlights,
            [
                Highlight::new(0, 1, HighlightColor::Green),
                Highlight::new(1, 2, HighlightColor::Red)
            ]
        );
    }

    #[test]
    fn a_worktree_only_change_is_red_on_the_first_column() {
        // 左詰めで作業ツリー列の文字が先頭へ来るため、色も先頭へ付ける
        // （範囲を追随させないと、色が空白の側に付いて何も見えなくなる）
        let candidate = FileCandidate::from_change(&change("a.txt", " M"), RenameOrigin::Exclude);

        assert_eq!(candidate.display, "M  a.txt");
        assert_eq!(
            candidate.highlights,
            [Highlight::new(0, 1, HighlightColor::Red)]
        );
    }

    #[test]
    fn an_untracked_file_is_red_on_both_columns() {
        assert_eq!(
            FileCandidate::from_change(&change("new.txt", "??"), RenameOrigin::Exclude).highlights,
            [Highlight::new(0, 2, HighlightColor::Red)]
        );
    }

    #[test]
    fn a_renamed_file_is_coloured_on_the_index_column() {
        assert_eq!(
            FileCandidate::from_change(&rename("new.txt", "old.txt", "R "), RenameOrigin::Include)
                .highlights,
            [Highlight::new(0, 1, HighlightColor::Green)]
        );
    }

    #[test]
    fn a_revision_file_has_no_colour_because_it_has_no_status_code() {
        assert!(FileCandidate::from_path("a.txt").highlights.is_empty());
    }

    #[test]
    fn a_plain_change_targets_its_own_path_only() {
        for origin in [RenameOrigin::Include, RenameOrigin::Exclude] {
            let candidate = FileCandidate::from_change(&change("a.txt", " M"), origin);

            assert_eq!(candidate.key, "a.txt");
            assert_eq!(candidate.paths, ["a.txt"]);
        }
    }

    #[test]
    fn a_rename_targets_its_original_path_as_well_when_unstaging() {
        let candidate =
            FileCandidate::from_change(&rename("new.txt", "old.txt", "R "), RenameOrigin::Include);

        assert_eq!(candidate.key, "new.txt", "the key stays the new path");
        assert_eq!(candidate.paths, ["new.txt", "old.txt"]);
    }

    #[test]
    fn a_rename_targets_the_new_path_only_when_the_index_is_kept() {
        let candidate =
            FileCandidate::from_change(&rename("new.txt", "old.txt", "RM"), RenameOrigin::Exclude);

        assert_eq!(candidate.paths, ["new.txt"]);
    }

    #[test]
    fn a_revision_file_is_shown_and_targeted_by_its_path() {
        let candidate = FileCandidate::from_path("dir/with space.txt");

        assert_eq!(candidate.display, "dir/with space.txt");
        assert_eq!(candidate.key, "dir/with space.txt");
        assert_eq!(candidate.paths, ["dir/with space.txt"]);
    }

    #[test]
    fn a_selection_is_returned_in_the_order_of_the_candidates() {
        let candidates = candidates();
        let selected = vec!["c.txt".to_owned(), "a.txt".to_owned()];

        let resolved = resolve(Language::Japanese.messages(), &candidates, &selected)
            .expect("all keys are candidates");

        assert_eq!(keys(&resolved), ["a.txt", "c.txt"]);
    }

    #[test]
    fn unselected_candidates_are_dropped() {
        let candidates = candidates();
        let selected = vec!["b.txt".to_owned()];

        let resolved = resolve(Language::Japanese.messages(), &candidates, &selected)
            .expect("all keys are candidates");

        assert_eq!(keys(&resolved), ["b.txt"]);
    }

    #[test]
    fn a_key_outside_of_the_candidates_is_rejected() {
        let candidates = candidates();
        let selected = vec!["a.txt".to_owned(), "z.txt".to_owned()];

        let err = resolve(Language::Japanese.messages(), &candidates, &selected)
            .expect_err("unknown key must be rejected");

        assert!(
            err.to_string().contains("z.txt"),
            "the unknown file should be named: {err:#}"
        );
    }

    #[test]
    fn changes_are_resolved_in_the_order_of_the_list() {
        let changes = [
            change("a.txt", "M "),
            change("b.txt", " M"),
            change("c.txt", "??"),
        ];
        let selected = vec!["c.txt".to_owned(), "a.txt".to_owned()];

        let resolved = resolve_changes(Language::Japanese.messages(), &changes, &selected)
            .expect("all paths are listed");

        assert_eq!(
            resolved
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>(),
            ["a.txt", "c.txt"]
        );
    }

    #[test]
    fn a_change_outside_of_the_list_is_rejected() {
        let changes = [change("a.txt", "M ")];
        let selected = vec!["z.txt".to_owned()];

        let err = resolve_changes(Language::Japanese.messages(), &changes, &selected)
            .expect_err("unknown path must be rejected");

        assert!(
            err.to_string().contains("z.txt"),
            "the unknown file should be named: {err:#}"
        );
    }

    #[test]
    fn a_renamed_change_is_resolved_by_its_new_path() {
        // 候補のキーは変更後のパスであるため、選択結果もそのパスで戻ってくる
        let changes = [rename("new.txt", "old.txt", "R ")];
        let selected = vec!["new.txt".to_owned()];

        let resolved = resolve_changes(Language::Japanese.messages(), &changes, &selected)
            .expect("the new path is the key");

        assert_eq!(resolved[0].original_path.as_deref(), Some("old.txt"));
    }

    #[test]
    fn target_paths_flattens_the_selection() {
        let rename =
            FileCandidate::from_change(&rename("new.txt", "old.txt", "R "), RenameOrigin::Include);
        let plain = FileCandidate::from_change(&change("a.txt", "M "), RenameOrigin::Include);

        assert_eq!(
            target_paths(&[&rename, &plain]),
            ["new.txt", "old.txt", "a.txt"]
        );
    }

    #[test]
    fn a_path_shared_by_two_candidates_is_passed_once() {
        let rename = FileCandidate::from_change(
            &rename("new.txt", "shared.txt", "R "),
            RenameOrigin::Include,
        );
        let plain = FileCandidate::from_change(&change("shared.txt", "M "), RenameOrigin::Include);

        assert_eq!(
            target_paths(&[&rename, &plain]),
            ["new.txt", "shared.txt"],
            "a duplicated path must not be passed to git twice"
        );
    }

    #[test]
    fn every_file_selection_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let missing = language
                .messages()
                .file_selection()
                .selection_not_found("a.txt, z.txt");

            assert!(!missing.trim().is_empty(), "{language:?} left it empty");
            assert!(
                missing.contains("a.txt") && missing.contains("z.txt"),
                "{language:?} must name every missing path: {missing}"
            );
        }
    }

    #[test]
    fn the_file_selection_wording_is_translated() {
        assert_ne!(
            Language::Japanese
                .messages()
                .file_selection()
                .selection_not_found("a.txt"),
            Language::English
                .messages()
                .file_selection()
                .selection_not_found("a.txt")
        );
    }
}
