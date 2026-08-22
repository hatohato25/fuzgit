//! `gz revert` — コミットを複数選択して打ち消す（FR-15）。
//!
//! 新しいコミットを作る操作であり既存の履歴・作業ツリーを壊さないため、
//! `gz cherry-pick` と同じく確認プロンプトは設けない（design.md セキュリティ設計）。

use anyhow::{Context as _, Result, bail};

use crate::cli::DEFAULT_COMMIT_LIMIT;
use crate::commands::{commit_highlights, commit_line};
use crate::finder::{FinderItem, PreviewSource, select_many};
use crate::git::exec::run_git;
use crate::git::read::{CommitInfo, CommitScope, commit_by_id, commits};
use crate::i18n::{Language, Messages};

/// `git revert` にエディタを起動させないオプション。
const NO_EDIT_OPTION: &str = "--no-edit";

/// マージコミットとみなす親の数。
///
/// 親が 2 つ以上のコミットは、どちらの親を「打ち消す側の履歴」とみなすかが定まらないため
/// `git revert` が `-m <parent-number>` を要求する。
const MERGE_PARENT_COUNT: usize = 2;

/// revert コミットのメッセージをエディタで編集するかどうか。
///
/// `git revert` は端末では既定でエディタを起動するため、複数件を選んだ場合は
/// その回数だけエディタが開く。真偽値を持ち回さず、意味のある型で受け取る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageEditing {
    /// git の既定どおりエディタでメッセージを編集する。
    Interactive,
    /// エディタを起動せず既定メッセージのままコミットする（`--no-edit`）。
    Skip,
}

impl MessageEditing {
    /// `git revert` に付けるオプション。
    fn option(self) -> Option<&'static str> {
        match self {
            MessageEditing::Interactive => None,
            MessageEditing::Skip => Some(NO_EDIT_OPTION),
        }
    }
}

/// コミットを複数選択し、選択順ではなく新しい順に revert する。
///
/// # Errors
///
/// コミット履歴の取得、選択（中断を含む）、マージコミットが選択に含まれる場合、
/// `git revert` の実行に失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    editing: MessageEditing,
) -> Result<()> {
    let candidates = commits(repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT)
        .context(messages.common().commit_history_read_failed())?;

    let items = candidates
        .iter()
        .map(|commit| to_item(language, commit))
        .collect();
    let selected = select_many(items)?;

    let ordered = newest_first(messages, &candidates, &selected)?;

    apply(language, messages, repository, editing, &ordered)
}

/// 選択済みの 1 コミットを revert する。
///
/// コミット選択後のアクションメニュー（FR-32）から呼ぶための入口であり、`gz revert` を
/// 直接使った場合と同じ実行部（[`apply`]）を通る。**マージコミットの事前停止もここで働く**。
///
/// # Errors
///
/// コミットの読み取り、マージコミットの判定、`git revert` の実行に失敗した場合、
/// および対象がマージコミットだった場合にエラーを返す。
pub fn run_on_commit(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    editing: MessageEditing,
    id: &str,
) -> Result<()> {
    // メニューから渡るのはフルハッシュだけなので、マージ判定と案内文のために読み直す
    let commit =
        commit_by_id(repository, id).with_context(|| messages.common().commit_read_failed(id))?;

    apply(language, messages, repository, editing, &[&commit])
}

/// 新しい順に並べ替え済みのコミットを 1 回の `git revert` で打ち消す。
///
/// # Errors
///
/// マージコミットの判定、`git revert` の実行に失敗した場合、および選択に
/// マージコミットが含まれる場合にエラーを返す。
fn apply(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    editing: MessageEditing,
    ordered: &[&CommitInfo],
) -> Result<()> {
    // マージコミットが 1 件でも含まれていれば、他のコミットも revert せずに停止する。
    // 一部だけ適用してから失敗すると、どこまで進んだのかをユーザーが追う必要が出るため
    let merges = merge_commits(messages, repository, ordered)?;
    if !merges.is_empty() {
        bail!(merge_commit_message(messages, &merges));
    }

    let hashes: Vec<String> = ordered.iter().map(|commit| commit.id.clone()).collect();
    let arguments = revert_args(editing, &hashes);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    // 継承 stdio で実行するため、コンフリクト時の git のメッセージはそのまま端末へ表示される
    run_git(language, &arguments).context(messages.revert().resolution_hint())?;

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// コミットメッセージでの検索を主用途とするため、サマリを作者より前に置く（`gz log` と同形式）。
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

/// 選択されたコミットを新しい順に並べ替える。
///
/// finder の選択結果はユーザーが選んだ順で返るため、そのまま渡すと適用順が定まらない。
/// `git revert` は指定された順に打ち消すコミットを作るため、古い方から打ち消すと
/// その上に積まれた後続のコミットと衝突しやすい。候補一覧は新しい順に並んでいるので、
/// 先頭から辿ることで新しい順を復元する（`gz cherry-pick` の古い順ソートの逆）。
///
/// # Errors
///
/// 選択されたハッシュが候補一覧に含まれない場合にエラーを返す。
fn newest_first<'a>(
    messages: &dyn Messages,
    candidates: &'a [CommitInfo],
    selected: &[String],
) -> Result<Vec<&'a CommitInfo>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|hash| !candidates.iter().any(|candidate| &candidate.id == *hash))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(messages.revert().selection_not_found(&missing.join(", ")));
    }

    Ok(candidates
        .iter()
        .filter(|candidate| selected.contains(&candidate.id))
        .collect())
}

/// コミットの親の数を数える。
///
/// # Errors
///
/// ハッシュを解釈できない場合、コミットオブジェクトを取得できない場合にエラーを返す。
fn parent_count(
    messages: &dyn Messages,
    repository: &gix::Repository,
    commit: &CommitInfo,
) -> Result<usize> {
    let id = gix::ObjectId::from_hex(commit.id.as_bytes())
        .with_context(|| messages.common().commit_hash_parse_failed(&commit.id))?;
    let object = repository
        .find_commit(id)
        .with_context(|| messages.common().commit_read_failed(&commit.id))?;

    Ok(object.parent_ids().count())
}

/// 選択されたコミットのうち、マージコミット（親が 2 つ以上）を選択順のまま抽出する。
///
/// 判定は `git revert` を実行する前に行う。マージコミットを暗黙に候補から除外すると
/// 「選んだのに打ち消されない」ことに気づけないため、除外ではなく停止に倒す
/// （requirements.md FR-15）。
///
/// # Errors
///
/// コミットオブジェクトの取得に失敗した場合にエラーを返す。
fn merge_commits<'a>(
    messages: &dyn Messages,
    repository: &gix::Repository,
    selected: &[&'a CommitInfo],
) -> Result<Vec<&'a CommitInfo>> {
    let mut merges = Vec::new();
    for commit in selected {
        if parent_count(messages, repository, commit)? >= MERGE_PARENT_COUNT {
            merges.push(*commit);
        }
    }

    Ok(merges)
}

/// マージコミットが選択に含まれていることを伝えるメッセージ。
///
/// mainline（どちらの親を残すか）の指定はリポジトリの履歴構造の理解を要するため
/// fuzgit では扱わない（requirements.md「スコープ外」）。原因と、素の git で実行する
/// 手順をコピーできる形で示す。
fn merge_commit_message(messages: &dyn Messages, merges: &[&CommitInfo]) -> String {
    let mut message = String::from(messages.revert().merge_commit_selected());

    for commit in merges {
        message.push_str(&format!(
            "\n  git revert -m 1 {id}  # {short_id} {summary}",
            id = commit.id,
            short_id = commit.short_id,
            summary = commit.summary
        ));
    }

    message
}

/// `git revert [--no-edit] <hash>...` の引数を組み立てる。
///
/// ハッシュは候補一覧に由来する 16 進表記のみであり、オプションとして解釈される余地はない。
fn revert_args(editing: MessageEditing, hashes: &[String]) -> Vec<String> {
    let mut args = vec!["revert".to_owned()];
    if let Some(option) = editing.option() {
        args.push(option.to_owned());
    }
    args.extend(hashes.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{
        TempDir, commit, create_branch, git_in, init_repository, write_file,
    };

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

    fn ids(commits: &[&CommitInfo]) -> Vec<String> {
        commits.iter().map(|commit| commit.id.clone()).collect()
    }

    /// マージコミットを持つテストリポジトリを用意する。
    ///
    /// `main` と `other` で別のファイルを変更するため、merge はコンフリクトせずに完了する。
    fn repository_with_a_merge(label: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        commit(dir.path(), "first commit");

        create_branch(dir.path(), "other");
        write_file(dir.path(), "main.txt", "main side\n");
        git_in(dir.path(), &["add", "--", "main.txt"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "main side"]);

        git_in(dir.path(), &["switch", "--quiet", "other"]);
        write_file(dir.path(), "other.txt", "other side\n");
        git_in(dir.path(), &["add", "--", "other.txt"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "other side"]);

        git_in(dir.path(), &["switch", "--quiet", "main"]);
        git_in(dir.path(), &["merge", "--no-ff", "--no-edit", "other"]);

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn selections_are_reordered_from_the_newest_commit() {
        // 古い方から revert すると、その上に積まれた後続のコミットと衝突しやすい
        let candidates = candidates();
        let selected = vec![id("aaaa"), id("cccc")];

        let ordered = newest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ids(&ordered), [id("cccc"), id("aaaa")]);
    }

    #[test]
    fn an_already_ordered_selection_is_kept_as_is() {
        let candidates = candidates();
        let selected = vec![id("cccc"), id("bbbb"), id("aaaa")];

        let ordered = newest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ids(&ordered), [id("cccc"), id("bbbb"), id("aaaa")]);
    }

    #[test]
    fn the_order_never_depends_on_the_order_of_the_selection() {
        // skim が返す選択結果の順序は候補の並び順とは限らないため、どの順で選ばれても揃える
        let candidates = candidates();
        let selections = [
            vec![id("bbbb"), id("aaaa"), id("cccc")],
            vec![id("aaaa"), id("cccc"), id("bbbb")],
            vec![id("cccc"), id("aaaa"), id("bbbb")],
        ];

        for selected in selections {
            let ordered = newest_first(Language::Japanese.messages(), &candidates, &selected)
                .expect("all hashes are candidates");

            assert_eq!(
                ids(&ordered),
                [id("cccc"), id("bbbb"), id("aaaa")],
                "unexpected order for {selected:?}"
            );
        }
    }

    #[test]
    fn a_single_selection_is_returned_unchanged() {
        let candidates = candidates();
        let selected = vec![id("bbbb")];

        let ordered = newest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ids(&ordered), [id("bbbb")]);
    }

    #[test]
    fn unselected_candidates_are_dropped() {
        let candidates = candidates();
        let selected = vec![id("aaaa")];

        let ordered = newest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect("all hashes are candidates");

        assert_eq!(ids(&ordered), [id("aaaa")]);
    }

    #[test]
    fn a_hash_outside_of_the_candidates_is_rejected() {
        let candidates = candidates();
        let selected = vec![id("cccc"), id("dddd")];

        let err = newest_first(Language::Japanese.messages(), &candidates, &selected)
            .expect_err("unknown hash must be rejected");

        assert!(
            err.to_string().contains(&id("dddd")),
            "the unknown hash should be named: {err:#}"
        );
    }

    #[test]
    fn the_default_leaves_the_editor_to_git() {
        assert_eq!(
            revert_args(MessageEditing::Interactive, &[id("cccc")]),
            ["revert", &id("cccc")]
        );
    }

    #[test]
    fn the_no_edit_option_is_placed_before_the_hashes() {
        assert_eq!(
            revert_args(MessageEditing::Skip, &[id("cccc"), id("aaaa")]),
            ["revert", NO_EDIT_OPTION, &id("cccc"), &id("aaaa")]
        );
    }

    #[test]
    fn the_given_order_of_the_hashes_is_preserved() {
        // 並べ替えは newest_first の責務であり、引数組み立ては順序を変えない
        let hashes = vec![id("cccc"), id("bbbb"), id("aaaa")];

        assert_eq!(
            revert_args(MessageEditing::Interactive, &hashes),
            ["revert", &id("cccc"), &id("bbbb"), &id("aaaa")]
        );
    }

    #[test]
    fn a_single_hash_produces_two_arguments() {
        assert_eq!(
            revert_args(MessageEditing::Interactive, &[id("bbbb")]),
            ["revert", &id("bbbb")]
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
    fn a_merge_commit_is_recognised_by_its_second_parent() {
        let (_dir, repository) = repository_with_a_merge("revert-merge");
        let history =
            commits(&repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT).expect("history is read");
        let merge = history.first().expect("the merge is the newest commit");

        assert_eq!(
            parent_count(Language::Japanese.messages(), &repository, merge)
                .expect("the commit exists"),
            2
        );
        assert_eq!(
            ids(
                &merge_commits(Language::Japanese.messages(), &repository, &[merge])
                    .expect("the commit exists")
            ),
            [merge.id.as_str()],
            "a merge commit must be reported before git revert runs"
        );
    }

    #[test]
    fn ordinary_commits_are_left_alone() {
        let (_dir, repository) = repository_with_a_merge("revert-ordinary");
        let history =
            commits(&repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT).expect("history is read");
        let ordinary: Vec<&CommitInfo> = history
            .iter()
            .filter(|commit| commit.summary != "Merge branch 'other'")
            .collect();

        assert!(
            merge_commits(Language::Japanese.messages(), &repository, &ordinary)
                .expect("the commits exist")
                .is_empty(),
            "only merge commits need the parent number"
        );
    }

    #[test]
    fn the_root_commit_is_not_a_merge() {
        // 親が 0 件のコミットも `-m` を必要としない
        let (_dir, repository) = repository_with_a_merge("revert-root");
        let history =
            commits(&repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT).expect("history is read");
        let root = history.last().expect("the history has commits");

        assert_eq!(
            parent_count(Language::Japanese.messages(), &repository, root)
                .expect("the commit exists"),
            0
        );
    }

    #[test]
    fn an_unknown_hash_is_reported_instead_of_being_ignored() {
        let (_dir, repository) = repository_with_a_merge("revert-unknown");
        let candidates = candidates();

        let error = parent_count(Language::Japanese.messages(), &repository, &candidates[0])
            .expect_err("a commit that is not in the repository cannot be resolved");

        assert!(
            error.to_string().contains(&id("cccc")),
            "the hash should be named: {error}"
        );
    }

    #[test]
    fn the_merge_message_names_the_commit_and_the_command_to_run() {
        let candidates = candidates();
        let message = merge_commit_message(Language::Japanese.messages(), &[&candidates[0]]);

        assert!(
            message.contains("マージコミット"),
            "the cause should be stated: {message}"
        );
        assert!(
            message.contains(&format!("git revert -m 1 {id}", id = id("cccc"))),
            "the plain git command should be spelled out: {message}"
        );
        assert!(
            message.contains("commit 0"),
            "the summary helps to identify the commit: {message}"
        );
    }

    #[test]
    fn every_merge_commit_of_the_selection_is_listed() {
        let candidates = candidates();

        let message = merge_commit_message(
            Language::Japanese.messages(),
            &[&candidates[0], &candidates[2]],
        );

        assert_eq!(
            message
                .lines()
                .filter(|line| line.contains("git revert -m 1"))
                .count(),
            2,
            "each merge commit needs its own command: {message}"
        );
    }

    #[test]
    fn every_revert_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let revert = language.messages().revert();

            for text in [revert.resolution_hint(), revert.merge_commit_selected()] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            // ユーザーがそのまま打ち込むコマンド列であるため、どの言語でも同じ綴りで現れる
            assert!(
                revert.resolution_hint().contains("git revert --continue")
                    && revert.resolution_hint().contains("git revert --abort"),
                "{language:?} must spell out both commands: {text}",
                text = revert.resolution_hint()
            );
            assert!(
                revert
                    .merge_commit_selected()
                    .contains("-m <parent-number>"),
                "{language:?} must name the option git would need: {text}",
                text = revert.merge_commit_selected()
            );

            assert!(
                revert
                    .selection_not_found(&id("dddd"))
                    .contains(&id("dddd")),
                "{language:?} must name the missing hash"
            );
        }
    }

    #[test]
    fn the_revert_wording_is_translated() {
        let japanese = Language::Japanese.messages().revert();
        let english = Language::English.messages().revert();

        assert_ne!(japanese.resolution_hint(), english.resolution_hint());
        assert_ne!(
            japanese.merge_commit_selected(),
            english.merge_commit_selected()
        );
        assert_ne!(
            japanese.selection_not_found(&id("dddd")),
            english.selection_not_found(&id("dddd"))
        );
    }

    #[test]
    fn the_english_merge_message_still_lists_every_command() {
        let candidates = candidates();

        let message = merge_commit_message(
            Language::English.messages(),
            &[&candidates[0], &candidates[2]],
        );

        assert_eq!(
            message
                .lines()
                .filter(|line| line.contains("git revert -m 1"))
                .count(),
            2,
            "the commands must not depend on the display language: {message}"
        );
    }
}
