//! `gz restore` / `gz add` で共用する、変更ファイル候補の整形と選択結果の解決。
//!
//! 両コマンドとも「作業ツリールート基準のパスを候補として見せ、選択結果を
//! git へ渡すパスへ戻す」処理が同じであるため、ここへまとめて単体テストの対象とする。

use anyhow::{Result, bail};

use crate::git::read::FileChange;

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
        }
    }
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// `git status` と同じ状態コードを前置し、リネーム・コピーは変更元も併記する。
fn display_line(change: &FileChange) -> String {
    match &change.original_path {
        Some(original) => format!(
            "{code} {original} -> {path}",
            code = change.status_code(),
            path = change.path
        ),
        None => format!(
            "{code} {path}",
            code = change.status_code(),
            path = change.path
        ),
    }
}

/// 選択されたキーに対応する候補を、候補一覧の並び順で返す。
///
/// # Errors
///
/// 選択されたキーが候補一覧に含まれない場合にエラーを返す（対象を取り違えたまま
/// git 操作を実行しないよう、暗黙に読み飛ばさない）。
pub fn resolve<'a>(
    candidates: &'a [FileCandidate],
    selected: &[String],
) -> Result<Vec<&'a FileCandidate>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|key| !candidates.iter().any(|candidate| &candidate.key == *key))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            "選択されたファイル {} が候補に見つかりません",
            missing.join(", ")
        );
    }

    Ok(candidates
        .iter()
        .filter(|candidate| selected.contains(&candidate.key))
        .collect())
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
        assert_eq!(display_line(&change("src/main.rs", " M")), " M src/main.rs");
        assert_eq!(
            display_line(&change("new file.txt", "??")),
            "?? new file.txt"
        );
    }

    #[test]
    fn a_rename_line_shows_both_paths() {
        assert_eq!(
            display_line(&rename("new.txt", "old.txt", "R ")),
            "R  old.txt -> new.txt"
        );
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

        let resolved = resolve(&candidates, &selected).expect("all keys are candidates");

        assert_eq!(keys(&resolved), ["a.txt", "c.txt"]);
    }

    #[test]
    fn unselected_candidates_are_dropped() {
        let candidates = candidates();
        let selected = vec!["b.txt".to_owned()];

        let resolved = resolve(&candidates, &selected).expect("all keys are candidates");

        assert_eq!(keys(&resolved), ["b.txt"]);
    }

    #[test]
    fn a_key_outside_of_the_candidates_is_rejected() {
        let candidates = candidates();
        let selected = vec!["a.txt".to_owned(), "z.txt".to_owned()];

        let err = resolve(&candidates, &selected).expect_err("unknown key must be rejected");

        assert!(
            err.to_string().contains("z.txt"),
            "the unknown file should be named: {err:#}"
        );
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
}
