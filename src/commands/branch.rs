//! `gz branch` — ブランチを選択して切り替える（FR-1）。

use anyhow::{Context as _, Result, anyhow};

use crate::commands::selection_header;
use crate::finder::{
    FinderItem, FinderOptions, Highlight, HighlightColor, PreviewPanel, PreviewSource,
    SelectionMode, select_one_with,
};
use crate::git::exec::run_git;
use crate::git::read::{BranchDetail, BranchInfo, BranchScope, branch_details, branches};
use crate::i18n::{Language, Messages};

/// 現在のブランチを示すマーク（`git branch` と同じ `*`）。
const CURRENT_MARK: &str = "* ";

/// 現在のブランチ以外の行頭。マークの有無で名前の桁がずれないよう空白で揃える。
const OTHER_MARK: &str = "  ";

/// プレビューに表示する直近コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// 枠の指標で「切り替えると増えるコミット数」を示す記号。
///
/// 記号は文言ではないため翻訳しない（`crate::commands::COLUMN_SEPARATOR` と同じ扱い）。
const AHEAD_MARK: &str = "↑";

/// 枠の指標で「切り替えると見えなくなるコミット数」を示す記号。
const BEHIND_MARK: &str = "↓";

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

    // 補足情報は 1 回の git 呼び出しで全ブランチ分をまとめて取る。プレビューは
    // カーソル移動のたびに作り直されるため、候補ごとに git を起動しない
    let details = branch_details(repository, language);
    let items = candidates
        .iter()
        .map(|branch| to_item(language, branch, details.get(&branch.name)))
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
fn to_item(language: Language, branch: &BranchInfo, detail: Option<&BranchDetail>) -> FinderItem {
    let item = FinderItem::new(
        display_line(branch),
        branch.name.clone(),
        PreviewSource::Git(preview_args(branch)),
        language.messages(),
    );

    match detail {
        // 補足情報が取れなかったブランチは枠の 1 行目と罫線だけになる。
        // 取れなかった項目を空欄で埋めるより、行ごと省くほうが読み取りを誤らせない
        None => item,
        Some(detail) => item.with_panel(panel(detail)),
    }
}

/// ブランチの補足情報からプレビューの枠を組み立てる。
fn panel(detail: &BranchDetail) -> PreviewPanel {
    let (metric, highlights) = metric(detail);

    PreviewPanel::new()
        .with_metric(metric, highlights)
        .with_context(
            [
                detail.upstream.clone(),
                (!detail.committed.is_empty()).then(|| detail.committed.clone()),
                (!detail.author.is_empty()).then(|| detail.author.clone()),
            ]
            .into_iter()
            .flatten()
            .collect(),
        )
}

/// 枠の右端へ出す指標（`↑3 ↓0`）と、その色付けを組み立てる。
///
/// **0 件の側には色を付けない**。増減が無いことは目印にする値ではなく、色を付けると
/// 実際に増減があるブランチとの区別が付かなくなるため。
fn metric(detail: &BranchDetail) -> (String, Vec<Highlight>) {
    let ahead = format!("{AHEAD_MARK}{count}", count = detail.ahead);
    let behind = format!("{BEHIND_MARK}{count}", count = detail.behind);
    let metric = format!("{ahead} {behind}");

    let mut highlights = Vec::new();
    if detail.ahead > 0 {
        highlights.push(Highlight::new(0, ahead.len(), HighlightColor::Green));
    }
    if detail.behind > 0 {
        // 区切りの空白 1 文字を挟んで behind が始まる
        let start = ahead.len() + 1;
        highlights.push(Highlight::new(
            start,
            start + behind.len(),
            HighlightColor::Red,
        ));
    }

    (metric, highlights)
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
        let item = to_item(Language::Japanese, &remote("origin/main"), None);

        assert_eq!(item.key(), "origin/main");
    }

    /// 補足情報 1 件分（値は各テストで上書きする）。
    fn detail(ahead: usize, behind: usize) -> BranchDetail {
        BranchDetail {
            upstream: Some("origin/feature".to_owned()),
            ahead,
            behind,
            committed: "2 hours ago".to_owned(),
            author: "Mika Tanaka".to_owned(),
        }
    }

    #[test]
    fn the_metric_counts_what_switching_would_gain_and_lose() {
        let (text, _) = metric(&detail(3, 0));

        assert_eq!(text, "↑3 ↓0");
    }

    #[test]
    fn only_a_non_zero_side_of_the_metric_is_coloured() {
        let (text, highlights) = metric(&detail(3, 0));

        assert_eq!(
            highlights,
            vec![Highlight::new(0, "↑3".len(), HighlightColor::Green)],
            "増減の無い側に色を付けると、実際に増減がある候補と見分けが付かなくなる: {text}"
        );

        let (_, both) = metric(&detail(3, 2));
        assert_eq!(both.len(), 2, "両側に増減があれば両方に色が付く");

        let (_, neither) = metric(&detail(0, 0));
        assert!(neither.is_empty(), "増減が無ければ色は付かない");
    }

    #[test]
    fn a_branch_without_details_still_gets_the_plain_frame() {
        // 補足が取れないことを理由に選択そのものを止めない（枠は 1 行目と罫線だけになる）
        let item = to_item(Language::Japanese, &remote("origin/main"), None);

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
