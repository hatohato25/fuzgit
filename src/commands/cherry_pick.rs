//! `gz cherry-pick` — コミットを複数選択して cherry-pick する（FR-3）。

use anyhow::{Context as _, Result, bail};

use crate::cli::DEFAULT_COMMIT_LIMIT;
use crate::commands::{commit_highlights, commit_line};
use crate::finder::{FinderItem, PreviewSource, select_many};
use crate::git::exec::run_git;
use crate::git::read::{CommitInfo, CommitScope, commits};
use crate::i18n::{Language, Messages};

/// コミットを複数選択し、選択順ではなく履歴順（古い順）に cherry-pick する。
///
/// # Errors
///
/// コミット履歴の取得、選択（中断を含む）、`git cherry-pick` の実行に失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    scope: CommitScope<'_>,
) -> Result<()> {
    let candidates = commits(repository, scope, DEFAULT_COMMIT_LIMIT)
        .context(messages.common().commit_history_read_failed())?;

    let items = candidates
        .iter()
        .map(|commit| to_item(language, commit))
        .collect();
    let selected = select_many(items)?;

    let ordered = oldest_first(messages, &candidates, &selected)?;

    apply(language, messages, &ordered)
}

/// 選択済みの 1 コミットを cherry-pick する。
///
/// コミット選択後のアクションメニュー（FR-32）から呼ぶための入口であり、`gz cherry-pick` を
/// 直接使った場合と同じ実行部（[`apply`]）を通る。
///
/// # Errors
///
/// `git cherry-pick` の実行に失敗した場合にエラーを返す。
pub fn run_on_commit(language: Language, messages: &dyn Messages, id: &str) -> Result<()> {
    apply(language, messages, std::slice::from_ref(&id.to_owned()))
}

/// 履歴順に並べ替え済みのハッシュを 1 回の `git cherry-pick` で適用する。
///
/// 複数コミットを 1 回の実行にまとめるのは、途中で失敗したときに git 自身の
/// `--continue` / `--abort` で再開・中止できるようにするためであり、
/// 1 件ずつ呼び分けない。
///
/// # Errors
///
/// `git cherry-pick` の実行に失敗した場合にエラーを返す。
fn apply(language: Language, messages: &dyn Messages, hashes: &[String]) -> Result<()> {
    let arguments = cherry_pick_args(hashes);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    // 継承 stdio で実行するため、コンフリクト時の git のメッセージはそのまま端末へ表示される
    run_git(language, &arguments).context(messages.cherry_pick().resolution_hint())?;

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// コミットメッセージでの検索を主用途とするため、サマリを作者より前に置く。
fn display_line(commit: &CommitInfo) -> String {
    commit_line(commit)
}

/// プレビュー用の `git show` の引数を組み立てる。
fn preview_args(commit: &CommitInfo) -> Vec<String> {
    // 末尾の `--` により、ハッシュがパスではなくリビジョンとして解釈されることを保証する
    ["show", "--color=always", &commit.id, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// コミットを finder の候補へ変換する。
fn to_item(language: Language, commit: &CommitInfo) -> FinderItem {
    FinderItem::new(
        display_line(commit),
        commit.id.clone(),
        PreviewSource::Git(preview_args(commit)),
        language.messages(),
    )
    .with_highlights(commit_highlights(commit))
}

/// 選択されたコミットを古い順（履歴順）に並べ替える。
///
/// finder の選択結果はユーザーが選んだ順で返るため、そのまま cherry-pick すると
/// 適用順が履歴と食い違って不要なコンフリクトを招く。候補一覧は新しい順に並んでいるので、
/// 末尾から辿ることで履歴どおりの順序を復元する。
///
/// # Errors
///
/// 選択されたハッシュが候補一覧に含まれない場合にエラーを返す。
fn oldest_first(
    messages: &dyn Messages,
    candidates: &[CommitInfo],
    selected: &[String],
) -> Result<Vec<String>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|hash| !candidates.iter().any(|candidate| &candidate.id == *hash))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            messages
                .cherry_pick()
                .selection_not_found(&missing.join(", "))
        );
    }

    Ok(candidates
        .iter()
        .rev()
        .filter(|candidate| selected.contains(&candidate.id))
        .map(|candidate| candidate.id.clone())
        .collect())
}

/// `git cherry-pick <hash>...` の引数を組み立てる。
///
/// ハッシュは候補一覧に由来する 16 進表記のみであり、オプションとして解釈される余地はない。
fn cherry_pick_args(hashes: &[String]) -> Vec<String> {
    std::iter::once("cherry-pick".to_owned())
        .chain(hashes.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 新しい順（`commits()` の戻り値と同じ並び）の候補を作る。
    fn candidates() -> Vec<CommitInfo> {
        ["cccc", "bbbb", "aaaa"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| CommitInfo {
                id: id.repeat(10),
                short_id: id.to_owned(),
                summary: format!("commit {index}"),
                author: "fuzgit test".to_owned(),
                time: "2024-01-02".to_owned(),
            })
            .collect()
    }

    fn id(prefix: &str) -> String {
        prefix.repeat(10)
    }

    #[test]
    fn selections_are_reordered_from_the_oldest_commit() {
        let candidates = candidates();
        let selected = vec![id("cccc"), id("aaaa")];

        let ordered = oldest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ordered, [id("aaaa"), id("cccc")]);
    }

    #[test]
    fn an_already_ordered_selection_is_kept_as_is() {
        let candidates = candidates();
        let selected = vec![id("aaaa"), id("bbbb"), id("cccc")];

        let ordered = oldest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ordered, [id("aaaa"), id("bbbb"), id("cccc")]);
    }

    #[test]
    fn a_single_selection_is_returned_unchanged() {
        let candidates = candidates();
        let selected = vec![id("bbbb")];

        let ordered = oldest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ordered, [id("bbbb")]);
    }

    #[test]
    fn unselected_candidates_are_dropped() {
        let candidates = candidates();
        let selected = vec![id("cccc")];

        let ordered = oldest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ordered, [id("cccc")]);
    }

    #[test]
    fn a_hash_outside_of_the_candidates_is_rejected() {
        let candidates = candidates();
        let selected = vec![id("cccc"), id("dddd")];

        let err = oldest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect_err("unknown hash must be rejected");

        assert!(
            err.to_string().contains(&id("dddd")),
            "the unknown hash should be named: {err:#}"
        );
    }

    #[test]
    fn arguments_start_with_the_subcommand_and_keep_the_given_order() {
        let hashes = vec![id("aaaa"), id("cccc")];

        assert_eq!(
            cherry_pick_args(&hashes),
            ["cherry-pick", &id("aaaa"), &id("cccc")]
        );
    }

    #[test]
    fn a_single_hash_produces_two_arguments() {
        assert_eq!(
            cherry_pick_args(&[id("bbbb")]),
            ["cherry-pick", &id("bbbb")]
        );
    }

    #[test]
    fn a_line_shows_the_short_hash_date_summary_and_author() {
        let candidates = candidates();
        let commit = candidates.first().expect("candidates are not empty");

        assert_eq!(
            display_line(commit),
            "cccc 2024-01-02 commit 0 (fuzgit test)"
        );
    }

    #[test]
    fn the_preview_shows_the_commit_and_ends_with_a_path_separator() {
        let candidates = candidates();
        let commit = candidates.first().expect("candidates are not empty");

        assert_eq!(
            preview_args(commit),
            ["show", "--color=always", &id("cccc"), "--"]
        );
    }

    #[test]
    fn an_item_keeps_the_full_hash_as_its_key() {
        let candidates = candidates();
        let commit = candidates.first().expect("candidates are not empty");

        assert_eq!(to_item(Language::Japanese, commit).key(), id("cccc"));
    }

    #[test]
    fn every_cherry_pick_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let cherry_pick = language.messages().cherry_pick();
            let missing = cherry_pick.selection_not_found("aaaa, bbbb");

            assert!(
                missing.contains("aaaa") && missing.contains("bbbb"),
                "{language:?} must name every missing hash: {missing}"
            );
            assert!(
                !cherry_pick.resolution_hint().trim().is_empty(),
                "{language:?} left the hint empty"
            );
            // ユーザーがそのまま打ち込むコマンド列は訳さない
            assert!(
                cherry_pick
                    .resolution_hint()
                    .contains("git cherry-pick --continue")
                    && cherry_pick
                        .resolution_hint()
                        .contains("git cherry-pick --abort"),
                "{language:?} must keep the commands to run: {hint}",
                hint = cherry_pick.resolution_hint()
            );
        }
    }

    #[test]
    fn the_cherry_pick_wording_is_translated() {
        let japanese = Language::Japanese.messages().cherry_pick();
        let english = Language::English.messages().cherry_pick();

        assert_ne!(
            japanese.selection_not_found("aaaa"),
            english.selection_not_found("aaaa")
        );
        assert_ne!(japanese.resolution_hint(), english.resolution_hint());
    }
}
