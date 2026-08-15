//! `gz rebase` — rebase の base となるブランチを選択して rebase する（FR-13）。
//!
//! base 候補はローカル・リモート追跡ブランチに限る（tag・任意コミットは
//! requirements.md「スコープ外」）。Before/After のアスキーアートも表示せず、
//! 判断材料は replay されるコミットのプレビューと確認プロンプトの件数提示で示す。
//!
//! merge / rebase が進行中の場合は、base 選択ではなく復帰メニュー（FR-14、
//! [`crate::commands::in_progress`]）へ委譲する。

use anyhow::{Context as _, Result, anyhow, bail};

use crate::commands::command_display;
use crate::commands::confirmation::confirm;
use crate::commands::in_progress;
use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::run_git;
use crate::git::read::{BranchInfo, commit_count, operation_in_progress, other_branches};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};

/// replay の対象となる現在の位置を指すリビジョン。
///
/// ブランチ名ではなく `HEAD` を使うのは、detached HEAD でも同じ式で「今いる位置」を
/// 指せるため。`git rebase <base>` が動かすのも HEAD であり、表示と実行の基準がずれない。
const CURRENT_REVISION: &str = "HEAD";

/// プレビューに表示する最大コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// rebase の base を 1 件選び、確認のうえ `git rebase <base>` を実行する。
///
/// merge / rebase が進行中の場合は base 選択を行わず、復帰メニュー（FR-14）を表示する。
///
/// # Errors
///
/// ブランチ一覧の取得、選択（中断を含む）、コミット数の取得、`git rebase` の実行に
/// 失敗した場合にエラーを返す。確認プロンプトで承認が得られなかった場合は
/// [`crate::error::Error::Cancelled`]。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<()> {
    // 進行中の merge / rebase を残したまま新しい rebase は開始できないため、
    // 選択させる前に復帰メニューへ委譲する
    if let Some(operation) = operation_in_progress(repository) {
        return in_progress::run(language, messages, repository, operation);
    }

    let candidates =
        other_branches(repository).context(messages.common().branch_list_read_failed())?;
    if candidates.is_empty() {
        bail!(messages.rebase().no_candidates());
    }

    let items = candidates
        .iter()
        .map(|branch| to_item(language, branch))
        .collect();
    let selected = select_one(items)?;

    // `git rebase` はブランチ名を位置引数に取り `--` で保護できないため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let branch = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!(messages.rebase().selection_not_found(&selected)))?;

    let count = commit_count(workdir(repository)?, &replay_range(&branch.name))
        .with_context(|| messages.rebase().replayed_commit_count_failed(&branch.name))?;

    let arguments = rebase_args(&branch.name);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    // 履歴改変であり、コンフリクトすれば作業が中断されるため、実行前に同意を求める
    // （design.md セキュリティ設計）
    confirm(
        messages,
        &confirmation_header(messages, &branch.name, count),
        &[&command_display(&arguments)],
    )?;

    run_git(language, &arguments).with_context(|| messages.rebase().rebase_failed(&branch.name))?;

    Ok(())
}

/// replay されるコミットの範囲（`<base>..HEAD`）。
fn replay_range(branch: &str) -> String {
    format!("{branch}..{CURRENT_REVISION}")
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
///
/// 表示するのは `<base>..HEAD` = この rebase で replay されるコミット。
fn preview_args(branch: &BranchInfo) -> Vec<String> {
    let range = replay_range(&branch.name);

    // 末尾の `--` により、リビジョンがパスとして解釈されることを防ぐ
    [
        "log",
        "--color=always",
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        &range,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// ブランチを finder の候補へ変換する。
///
/// 表示は名前だけとし、一覧へ ahead/behind 等を事前表示しない
/// （全候補分の事前計算は初期表示の応答性を損なうため。requirements.md「スコープ外」）。
fn to_item(language: Language, branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        branch.name.clone(),
        branch.name.clone(),
        PreviewSource::Git(preview_args(branch)),
        language.messages(),
    )
}

/// `git rebase <base>` の引数を組み立てる。
///
/// ブランチ名は gix が列挙した候補に由来する値だけを渡す
/// （`git rebase` の位置引数は `--` で保護できないため、値の由来で担保する）。
fn rebase_args(branch: &str) -> Vec<String> {
    vec!["rebase".to_owned(), branch.to_owned()]
}

/// 確認プロンプトの見出しを組み立てる。
///
/// replay されるコミット数と、履歴改変であることを示す。
fn confirmation_header(messages: &dyn Messages, base: &str, count: usize) -> String {
    // 本文と注記を区切る改行は装飾であるため、文言ではなくここで付ける
    format!(
        "{headline}\n{note}",
        headline = messages.rebase().confirmation(base, count),
        note = messages.common().history_rewrite_note()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{TempDir, commit, init_repository};

    fn local(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    #[test]
    fn rebasing_passes_the_base_as_it_was_listed() {
        assert_eq!(rebase_args("main"), ["rebase", "main"]);
    }

    #[test]
    fn a_branch_containing_a_slash_keeps_its_full_name() {
        assert_eq!(
            rebase_args("origin/feature/login"),
            ["rebase", "origin/feature/login"]
        );
    }

    #[test]
    fn the_preview_lists_the_commits_that_would_be_replayed() {
        assert_eq!(
            preview_args(&local("main")),
            [
                "log",
                "--color=always",
                "--oneline",
                "--decorate",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "main..HEAD",
                "--"
            ],
            "the range ends at the current position"
        );
    }

    #[test]
    fn the_replay_range_is_the_opposite_of_the_merge_one() {
        // merge が `HEAD..<候補>`（取り込まれるコミット）なのに対し、
        // rebase は `<候補>..HEAD`（作り直されるコミット）を対象にする
        assert_eq!(replay_range("origin/main"), "origin/main..HEAD");
    }

    #[test]
    fn an_item_keeps_the_branch_name_as_its_key() {
        assert_eq!(
            to_item(Language::Japanese, &local("origin/main")).key(),
            "origin/main"
        );
    }

    #[test]
    fn the_confirmation_names_the_base_and_the_number_of_commits() {
        let header = confirmation_header(Language::Japanese.messages(), "main", 2);

        assert!(header.contains("`main`"), "unexpected header: {header}");
        assert!(header.contains("2 件"), "unexpected header: {header}");
    }

    #[test]
    fn the_confirmation_warns_that_the_history_is_rewritten() {
        let header = confirmation_header(Language::Japanese.messages(), "main", 0);

        assert!(
            header.contains("コミットハッシュが変わります"),
            "a history rewrite must be spelled out: {header}"
        );
    }

    #[test]
    fn the_confirmation_shows_the_command_that_runs() {
        assert_eq!(command_display(&["rebase", "main"]), "git rebase main");
    }

    #[test]
    fn a_repository_with_no_other_branch_offers_no_base() {
        let dir = TempDir::new("rebase-no-candidates");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = run(
            Language::Japanese,
            Language::Japanese.messages(),
            &repository,
        )
        .expect_err("the only branch is the current one");

        assert!(
            err.to_string()
                .contains("rebase の base になるブランチがありません"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn every_rebase_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let rebase = language.messages().rebase();

            assert!(
                !rebase.no_candidates().trim().is_empty(),
                "{language:?} left a message empty"
            );

            assert!(
                rebase.selection_not_found("main").contains("main"),
                "{language:?} must name the selection"
            );
            assert!(
                rebase.replayed_commit_count_failed("main").contains("main"),
                "{language:?} must name the base"
            );
            assert!(
                rebase.rebase_failed("main").contains("main"),
                "{language:?} must name the base"
            );

            let confirmation = rebase.confirmation("main", 2);
            assert!(
                confirmation.contains("main") && confirmation.contains('2'),
                "{language:?} must name the base and the number of commits: {confirmation}"
            );
        }
    }

    #[test]
    fn the_rebase_wording_is_translated() {
        let japanese = Language::Japanese.messages().rebase();
        let english = Language::English.messages().rebase();

        assert_ne!(japanese.no_candidates(), english.no_candidates());
        assert_ne!(
            japanese.selection_not_found("main"),
            english.selection_not_found("main")
        );
        assert_ne!(
            japanese.replayed_commit_count_failed("main"),
            english.replayed_commit_count_failed("main")
        );
        assert_ne!(
            japanese.rebase_failed("main"),
            english.rebase_failed("main")
        );
        assert_ne!(
            japanese.confirmation("main", 2),
            english.confirmation("main", 2)
        );
    }

    #[test]
    fn the_english_count_agrees_with_the_noun_it_qualifies() {
        let english = Language::English.messages().rebase();

        assert!(
            english.confirmation("main", 1).contains("1 commit will"),
            "unexpected wording: {header}",
            header = english.confirmation("main", 1)
        );
        assert!(
            english.confirmation("main", 2).contains("2 commits will"),
            "unexpected wording: {header}",
            header = english.confirmation("main", 2)
        );
    }
}
