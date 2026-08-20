//! `gz branch` — ブランチを選択して切り替える（FR-1）。

use anyhow::{Context as _, Result, anyhow};

use crate::commands::selection_header;
use crate::finder::{FinderItem, FinderOptions, PreviewSource, SelectionMode, select_one_with};
use crate::git::exec::run_git;
use crate::git::read::{BranchInfo, BranchScope, branches};
use crate::i18n::{Language, Messages};

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
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    scope: BranchScope,
) -> Result<()> {
    let candidates =
        branches(repository, scope).context(messages.common().branch_list_read_failed())?;

    let items = candidates
        .iter()
        .map(|branch| to_item(language, branch))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.branch().header_subject(),
        messages.branch().header_outcome(),
    ));
    let selected = select_one_with(items, &options)?;

    let branch = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!(messages.branch().selection_not_found(&selected)))?;

    let target = switch_target(messages, branch)?;
    run_git(language, &["switch", &target])
        .with_context(|| messages.common().switch_failed(&target))?;

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
fn to_item(language: Language, branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        display_line(branch),
        branch.name.clone(),
        PreviewSource::Git(preview_args(branch)),
        language.messages(),
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
fn switch_target(messages: &dyn Messages, branch: &BranchInfo) -> Result<String> {
    if !branch.is_remote {
        return Ok(branch.name.clone());
    }

    branch
        .name
        .split_once('/')
        .map(|(_remote, local)| local.to_owned())
        .ok_or_else(|| anyhow!(messages.branch().tracking_target_undetermined(&branch.name)))
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
        let target = switch_target(Language::Japanese.messages(), &local("feature"))
            .expect("local branch is always switchable");

        assert_eq!(target, "feature");
    }

    #[test]
    fn the_current_branch_is_switchable_as_well() {
        let mut branch = local("main");
        branch.is_current = true;

        let target = switch_target(Language::Japanese.messages(), &branch)
            .expect("current branch is switchable");

        assert_eq!(target, "main");
    }

    #[test]
    fn a_remote_branch_drops_the_remote_name_for_dwim() {
        let target = switch_target(Language::Japanese.messages(), &remote("origin/feature"))
            .expect("remote branch is switchable");

        assert_eq!(target, "feature");
    }

    #[test]
    fn only_the_remote_name_is_dropped_from_a_hierarchical_branch() {
        let target = switch_target(
            Language::Japanese.messages(),
            &remote("upstream/feature/login"),
        )
        .expect("remote branch is switchable");

        assert_eq!(target, "feature/login");
    }

    #[test]
    fn a_local_branch_containing_a_slash_keeps_its_full_name() {
        let target = switch_target(Language::Japanese.messages(), &local("feature/login"))
            .expect("local branch is switchable");

        assert_eq!(target, "feature/login");
    }

    #[test]
    fn a_remote_branch_without_a_remote_name_is_rejected() {
        let err = switch_target(Language::Japanese.messages(), &remote("origin"))
            .expect_err("a bare remote name is not a branch");

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
        let item = to_item(Language::Japanese, &remote("origin/main"));

        assert_eq!(item.key(), "origin/main");
    }

    #[test]
    fn every_branch_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let branch = language.messages().branch();

            for text in [branch.header_subject(), branch.header_outcome()] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            for (text, argument) in [
                (branch.selection_not_found("feature"), "feature"),
                (branch.tracking_target_undetermined("origin"), "origin"),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_branch_wording_is_translated() {
        let japanese = Language::Japanese.messages().branch();
        let english = Language::English.messages().branch();

        assert_ne!(japanese.header_subject(), english.header_subject());
        assert_ne!(japanese.header_outcome(), english.header_outcome());
        assert_ne!(
            japanese.selection_not_found("feature"),
            english.selection_not_found("feature")
        );
        assert_ne!(
            japanese.tracking_target_undetermined("origin"),
            english.tracking_target_undetermined("origin")
        );
    }
}
