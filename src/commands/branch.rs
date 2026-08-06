//! `gz branch` — ブランチを選択して切り替える（FR-1）。

use anyhow::{Context as _, Result, anyhow};

use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::run_git;
use crate::git::read::{BranchInfo, BranchScope, branches};

/// 現在のブランチを示すマーク（`git branch` と同じ `*`）。
const CURRENT_MARK: &str = "* ";

/// 現在のブランチ以外の行頭。マークの有無で名前の桁がずれないよう空白で揃える。
const OTHER_MARK: &str = "  ";

/// プレビューに表示する直近コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// ブランチ一覧から 1 件選び、そのブランチへ切り替える。
///
/// # Errors
///
/// ブランチ一覧の取得、選択（中断を含む）、`git switch` の実行に失敗した場合にエラーを返す。
pub fn run(repository: &gix::Repository, scope: BranchScope) -> Result<()> {
    let candidates = branches(repository, scope).context("ブランチ一覧の取得に失敗しました")?;

    let items = candidates.iter().map(to_item).collect();
    let selected = select_one(items)?;

    let branch = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!("選択されたブランチ `{selected}` が候補に見つかりません"))?;

    let target = switch_target(branch)?;
    run_git(&["switch", &target])
        .with_context(|| format!("`{target}` への切り替えに失敗しました"))?;

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
fn display_line(branch: &BranchInfo) -> String {
    let mark = if branch.is_current {
        CURRENT_MARK
    } else {
        OTHER_MARK
    };
    format!("{mark}{name}", name = branch.name)
}

/// プレビュー用の `git log --oneline` 相当の引数を組み立てる。
fn preview_args(branch: &BranchInfo) -> Vec<String> {
    // 末尾の `--` により、ブランチ名がパスではなくリビジョンとして解釈されることを保証する
    [
        "log",
        "--color=always",
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        &branch.name,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// ブランチを finder の候補へ変換する。
fn to_item(branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        display_line(branch),
        branch.name.clone(),
        PreviewSource::Git(preview_args(branch)),
    )
}

/// `git switch` へ渡すブランチ名を決める。
///
/// リモート追跡ブランチ（`origin/feature`）はリモート名を取り除いた短縮名（`feature`）へ変換する。
/// `git switch` の DWIM により、同名のローカルブランチが無ければリモートを追跡する
/// ローカルブランチが作成される。
///
/// # Errors
///
/// リモート追跡ブランチ名がリモート名を含まない形式で、追跡先の名前を決定できない場合にエラーを返す。
fn switch_target(branch: &BranchInfo) -> Result<String> {
    if !branch.is_remote {
        return Ok(branch.name.clone());
    }

    branch
        .name
        .split_once('/')
        .map(|(_remote, local)| local.to_owned())
        .ok_or_else(|| {
            anyhow!(
                "リモート追跡ブランチ `{}` から追跡先のブランチ名を決定できません",
                branch.name
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    fn remote(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: true,
        }
    }

    #[test]
    fn a_local_branch_switches_to_its_own_name() {
        let target = switch_target(&local("feature")).expect("local branch is always switchable");

        assert_eq!(target, "feature");
    }

    #[test]
    fn the_current_branch_is_switchable_as_well() {
        let mut branch = local("main");
        branch.is_current = true;

        let target = switch_target(&branch).expect("current branch is switchable");

        assert_eq!(target, "main");
    }

    #[test]
    fn a_remote_branch_drops_the_remote_name_for_dwim() {
        let target = switch_target(&remote("origin/feature")).expect("remote branch is switchable");

        assert_eq!(target, "feature");
    }

    #[test]
    fn only_the_remote_name_is_dropped_from_a_hierarchical_branch() {
        let target =
            switch_target(&remote("upstream/feature/login")).expect("remote branch is switchable");

        assert_eq!(target, "feature/login");
    }

    #[test]
    fn a_local_branch_containing_a_slash_keeps_its_full_name() {
        let target = switch_target(&local("feature/login")).expect("local branch is switchable");

        assert_eq!(target, "feature/login");
    }

    #[test]
    fn a_remote_branch_without_a_remote_name_is_rejected() {
        let err = switch_target(&remote("origin")).expect_err("a bare remote name is not a branch");

        assert!(
            err.to_string().contains("origin"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn the_current_branch_is_marked_and_the_others_are_aligned() {
        let mut current = local("main");
        current.is_current = true;

        assert_eq!(display_line(&current), "* main");
        assert_eq!(display_line(&local("feature")), "  feature");
        assert_eq!(display_line(&remote("origin/main")), "  origin/main");
    }

    #[test]
    fn the_preview_shows_the_branch_log_and_ends_with_a_path_separator() {
        let args = preview_args(&remote("origin/main"));

        assert_eq!(
            args,
            [
                "log",
                "--color=always",
                "--oneline",
                "--decorate",
                "-n",
                "50",
                "origin/main",
                "--"
            ]
        );
    }

    #[test]
    fn an_item_keeps_the_branch_name_as_its_key() {
        let item = to_item(&remote("origin/main"));

        assert_eq!(item.key(), "origin/main");
    }
}
