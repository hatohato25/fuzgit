//! `gz fixup` — 修正対象のコミットを選んで fixup / squash コミットを作成する（FR-11）。
//!
//! 作成するのはコミットまでで、autosquash の rebase は自動実行しない。
//! 履歴改変はユーザーの明示操作に委ねる（requirements.md「スコープ外」）。

use std::io::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::cli::DEFAULT_COMMIT_LIMIT;
use crate::commands::{commit_highlights, commit_line};
use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::run_git;
use crate::git::read::{ChangeScope, CommitInfo, CommitScope, changes, commits};
use crate::i18n::{Language, Messages};

/// 作成するコミットの種類。
///
/// `--fixup` と `--squash` は同時に指定できないため、真偽値ではなく列挙型で受け取る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixupKind {
    /// 対象コミットのメッセージを引き継ぐ `fixup!` コミット（既定）。
    Fixup,
    /// 対象コミットにメッセージを結合する `squash!` コミット（`--squash`）。
    Squash,
}

impl FixupKind {
    /// `git commit` に渡すオプション（`--fixup=<hash>` / `--squash=<hash>`）。
    ///
    /// git は値をオプションと同じ引数に含める形式（`--fixup=<hash>`）を受け付けるため、
    /// ハッシュが独立した位置引数として解釈される余地が無い。
    fn option(self, id: &str) -> String {
        match self {
            FixupKind::Fixup => format!("--fixup={id}"),
            FixupKind::Squash => format!("--squash={id}"),
        }
    }

    /// メッセージ中でこの種類を指す語（作成されるコミットの接頭辞と同じ綴り）。
    fn label(self) -> &'static str {
        match self {
            FixupKind::Fixup => "fixup",
            FixupKind::Squash => "squash",
        }
    }
}

/// autosquash の rebase 起点。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RebaseStart {
    /// 選択したコミットの親（`<hash>^`）。
    Parent(String),
    /// 履歴の先頭（`--root`）。
    ///
    /// ルートコミットには親が無く `<hash>^` を解決できないため、起点の指定方法が変わる。
    Root,
}

impl RebaseStart {
    /// `git rebase` に渡す起点の指定。
    fn argument(&self) -> String {
        match self {
            RebaseStart::Parent(id) => format!("{id}^"),
            RebaseStart::Root => "--root".to_owned(),
        }
    }
}

/// 修正対象のコミットを 1 件選び、`git commit --fixup=<hash>` を実行する。
///
/// ステージ済みの変更が無い場合は finder を起動する前にエラーとする
/// （選択操作を終えてから git に失敗させるのは無駄な操作を強いるため）。
///
/// # Errors
///
/// ステージ済みの変更が無い場合、変更・コミット履歴の取得、選択（中断を含む）、
/// `git commit` の実行、ヒントの出力に失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    kind: FixupKind,
) -> Result<()> {
    let staged = changes(repository, ChangeScope::Staged)
        .context(messages.fixup().staged_changes_read_failed())?;
    if staged.is_empty() {
        bail!(messages.fixup().staged_required(kind.label()));
    }

    let candidates = commits(repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT)
        .context(messages.common().commit_history_read_failed())?;

    let items = candidates
        .iter()
        .map(|commit| to_item(language, commit))
        .collect();
    let selected = select_one(items)?;

    // ハッシュは `--fixup=<hash>` のオプション値として渡るため `--` で保護できない。
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let commit = candidates
        .iter()
        .find(|candidate| candidate.id == selected)
        .ok_or_else(|| anyhow!(messages.fixup().selection_not_found(&selected)))?;

    // 親の有無の判定に失敗したときにコミットだけが作られることのないよう、実行前に解決する
    let start = rebase_start(messages, repository, commit)?;

    let arguments = commit_args(kind, &commit.id);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments)
        .with_context(|| messages.fixup().commit_creation_failed(kind.label()))?;

    // 標準出力はパイプ用途のために空けておく
    writeln!(
        std::io::stderr(),
        "{hint}",
        hint = autosquash_hint(messages, &start)
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
///
/// コミットメッセージでの絞り込みを主用途とするため、サマリを作者より前に置く（`gz log` と同形式）。
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

/// `git commit --fixup=<hash>` / `--squash=<hash>` の引数を組み立てる。
///
/// パス指定はしない。`git commit` は index の内容をそのままコミットし、
/// メッセージは対象コミットから自動生成されるためエディタも起動しない。
fn commit_args(kind: FixupKind, id: &str) -> Vec<String> {
    vec!["commit".to_owned(), kind.option(id)]
}

/// 対象コミットから autosquash の rebase 起点を決める。
///
/// # Errors
///
/// 対象コミットのハッシュを解釈できない場合、コミットオブジェクトを取得できない場合に
/// エラーを返す。
fn rebase_start(
    messages: &dyn Messages,
    repository: &gix::Repository,
    commit: &CommitInfo,
) -> Result<RebaseStart> {
    let id = gix::ObjectId::from_hex(commit.id.as_bytes())
        .with_context(|| messages.common().commit_hash_parse_failed(&commit.id))?;
    let object = repository
        .find_commit(id)
        .with_context(|| messages.common().commit_read_failed(&commit.id))?;

    // ルートコミットには親が無く `<hash>^` が解決できないため、起点を `--root` に切り替える
    if object.parent_ids().next().is_some() {
        Ok(RebaseStart::Parent(commit.id.clone()))
    } else {
        Ok(RebaseStart::Root)
    }
}

/// 作成したコミットを履歴へ取り込む手順の案内。
///
/// rebase は自動実行せず手順の提示に留める（履歴改変はユーザーの明示操作に委ねる）。
/// ハッシュは短縮せずフルハッシュで示す。履歴改変のコマンドを取り違えないよう、
/// 曖昧さの無い表記でそのままコピーできるようにするため。
fn autosquash_hint(messages: &dyn Messages, start: &RebaseStart) -> String {
    let mut hint = messages.fixup().autosquash_hint(&start.argument());

    if matches!(start, RebaseStart::Root) {
        // 本文と注記を区切る改行は装飾であるため、文言ではなくここで付ける
        hint.push('\n');
        hint.push_str(messages.fixup().root_start_note());
    }

    hint
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{TempDir, commit, init_repository, write_file};

    /// テスト用のダミーコミット（40 桁のハッシュを持つ）。
    fn commit_info() -> CommitInfo {
        CommitInfo {
            id: "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345".to_owned(),
            short_id: "1f0c9a4".to_owned(),
            summary: "認証まわりを整理する".to_owned(),
            author: "fuzgit test".to_owned(),
            time: "2024-01-02".to_owned(),
        }
    }

    /// 2 件のコミットを持つテストリポジトリを用意する。
    fn repository_with_two_commits(label: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        write_file(dir.path(), "a.txt", "first\n");
        commit(dir.path(), "first commit");
        write_file(dir.path(), "a.txt", "second\n");
        commit(dir.path(), "second commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn the_default_creates_a_fixup_commit() {
        assert_eq!(
            commit_args(FixupKind::Fixup, &commit_info().id),
            ["commit", "--fixup=1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345"]
        );
    }

    #[test]
    fn the_squash_option_creates_a_squash_commit() {
        assert_eq!(
            commit_args(FixupKind::Squash, &commit_info().id),
            [
                "commit",
                "--squash=1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345"
            ]
        );
    }

    #[test]
    fn the_two_kinds_never_appear_together() {
        // clap 側でも排他だが、引数組み立て時点でも一方しか現れないことを担保する
        for kind in [FixupKind::Fixup, FixupKind::Squash] {
            let arguments = commit_args(kind, &commit_info().id);

            assert_eq!(arguments.len(), 2, "unexpected arguments: {arguments:?}");
            assert_eq!(
                arguments
                    .iter()
                    .filter(|argument| argument.starts_with("--fixup=")
                        || argument.starts_with("--squash="))
                    .count(),
                1,
                "exactly one of the two options must be passed: {arguments:?}"
            );
        }
    }

    #[test]
    fn the_hash_is_carried_by_the_option_itself() {
        // ハッシュを独立した位置引数にしないため、`=` の後ろに埋め込む
        let arguments = commit_args(FixupKind::Fixup, &commit_info().id);

        assert!(
            !arguments.contains(&commit_info().id),
            "the hash must not be a standalone argument: {arguments:?}"
        );
        assert_eq!(
            FixupKind::Fixup.option(&commit_info().id),
            format!("--fixup={id}", id = commit_info().id)
        );
    }

    #[test]
    fn a_path_commit_is_never_requested() {
        // `git commit --fixup` は index の内容をコミットするため pathspec を渡さない
        let arguments = commit_args(FixupKind::Squash, &commit_info().id);

        assert!(
            !arguments.iter().any(|argument| argument == "--"),
            "no path separator should be present: {arguments:?}"
        );
    }

    #[test]
    fn a_line_shows_the_short_hash_date_summary_and_author() {
        assert_eq!(
            display_line(&commit_info()),
            "1f0c9a4 2024-01-02 認証まわりを整理する (fuzgit test)"
        );
    }

    #[test]
    fn the_preview_shows_the_commit_and_ends_with_a_path_separator() {
        assert_eq!(
            preview_args(&commit_info()),
            [
                "show",
                "--color=always",
                "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345",
                "--"
            ]
        );
    }

    #[test]
    fn an_item_keeps_the_full_hash_as_its_key() {
        let item = to_item(Language::Japanese, &commit_info());

        assert_eq!(item.key(), "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345");
    }

    #[test]
    fn the_hint_rebases_from_the_parent_of_the_target() {
        let hint = autosquash_hint(
            Language::Japanese.messages(),
            &RebaseStart::Parent(commit_info().id),
        );

        assert_eq!(
            hint,
            "ヒント: 作成したコミットを履歴へ取り込むには次を実行してください。\n  \
             git rebase -i --autosquash 1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345^"
        );
    }

    #[test]
    fn the_hint_for_a_root_commit_uses_root_instead_of_a_parent() {
        // ルートコミットには親が無いため `<hash>^` は解決できない
        let hint = autosquash_hint(Language::Japanese.messages(), &RebaseStart::Root);
        let command = hint
            .lines()
            .find(|line| line.contains("git rebase"))
            .expect("the hint spells out the command");

        assert_eq!(command.trim(), "git rebase -i --autosquash --root");
        assert!(
            !command.contains('^'),
            "a parent reference cannot be resolved for a root commit: {command}"
        );
        assert!(
            hint.contains("最初のコミット"),
            "the reason for --root should be explained: {hint}"
        );
    }

    #[test]
    fn the_hint_only_shows_the_command_and_never_runs_it() {
        // 履歴改変はユーザーの明示操作に委ねる（requirements.md「スコープ外」）
        for start in [RebaseStart::Parent(commit_info().id), RebaseStart::Root] {
            assert!(
                autosquash_hint(Language::Japanese.messages(), &start)
                    .contains("git rebase -i --autosquash"),
                "the hint must spell out the command"
            );
        }
    }

    #[test]
    fn a_commit_with_a_parent_is_rebased_from_that_parent() {
        let (_dir, repository) = repository_with_two_commits("fixup-parent");
        let candidates =
            commits(&repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT).expect("history is read");

        let start = rebase_start(Language::Japanese.messages(), &repository, &candidates[0])
            .expect("the commit exists");

        assert_eq!(start, RebaseStart::Parent(candidates[0].id.clone()));
        assert_eq!(
            start.argument(),
            format!("{id}^", id = candidates[0].id),
            "the rebase starts one commit before the target"
        );
    }

    #[test]
    fn the_root_commit_is_recognised_by_its_missing_parent() {
        let (_dir, repository) = repository_with_two_commits("fixup-root");
        let candidates =
            commits(&repository, CommitScope::Head, DEFAULT_COMMIT_LIMIT).expect("history is read");
        let root = candidates.last().expect("the history has two commits");

        let start = rebase_start(Language::Japanese.messages(), &repository, root)
            .expect("the commit exists");

        assert_eq!(start, RebaseStart::Root);
        assert_eq!(start.argument(), "--root");
    }

    #[test]
    fn an_unknown_hash_is_reported_instead_of_being_ignored() {
        let (_dir, repository) = repository_with_two_commits("fixup-unknown");

        let error = rebase_start(Language::Japanese.messages(), &repository, &commit_info())
            .expect_err("a commit that is not in the repository cannot be resolved");

        assert!(
            error.to_string().contains(&commit_info().id),
            "the hash should be named: {error}"
        );
    }

    #[test]
    fn the_missing_stage_message_names_the_kind_and_the_next_step() {
        for (kind, label) in [(FixupKind::Fixup, "fixup"), (FixupKind::Squash, "squash")] {
            let message = Language::Japanese
                .messages()
                .fixup()
                .staged_required(kind.label());

            assert!(
                message.contains("ステージ済みの変更がありません"),
                "the cause should be stated: {message}"
            );
            assert!(
                message.contains(label),
                "the message should name the {label} commit: {message}"
            );
            assert!(
                message.contains("gz add"),
                "the next step should be offered: {message}"
            );
        }
    }

    #[test]
    fn every_fixup_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let fixup = language.messages().fixup();

            for text in [fixup.staged_changes_read_failed(), fixup.root_start_note()] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            for label in ["fixup", "squash"] {
                assert!(
                    fixup.staged_required(label).contains(label),
                    "{language:?} must name the {label} commit"
                );
                assert!(
                    fixup.commit_creation_failed(label).contains(label),
                    "{language:?} must name the {label} commit"
                );
            }
            // 次に取れる操作はユーザーがそのまま打ち込むため、どの言語でも同じ綴りで現れる
            assert!(
                fixup.staged_required("fixup").contains("gz add"),
                "{language:?} must offer the next step: {text}",
                text = fixup.staged_required("fixup")
            );
            assert!(
                fixup
                    .autosquash_hint("--root")
                    .contains("git rebase -i --autosquash --root"),
                "{language:?} must spell out the command: {text}",
                text = fixup.autosquash_hint("--root")
            );

            assert!(
                fixup
                    .selection_not_found(&commit_info().id)
                    .contains(&commit_info().id),
                "{language:?} must name the missing hash"
            );
        }
    }

    #[test]
    fn the_fixup_wording_is_translated() {
        let japanese = Language::Japanese.messages().fixup();
        let english = Language::English.messages().fixup();

        assert_ne!(
            japanese.staged_changes_read_failed(),
            english.staged_changes_read_failed()
        );
        assert_ne!(
            japanese.staged_required("fixup"),
            english.staged_required("fixup")
        );
        assert_ne!(
            japanese.selection_not_found(&commit_info().id),
            english.selection_not_found(&commit_info().id)
        );
        assert_ne!(
            japanese.commit_creation_failed("squash"),
            english.commit_creation_failed("squash")
        );
        assert_ne!(
            japanese.autosquash_hint("--root"),
            english.autosquash_hint("--root")
        );
        assert_ne!(japanese.root_start_note(), english.root_start_note());
    }

    #[test]
    fn the_english_hint_still_explains_why_root_is_used() {
        let hint = autosquash_hint(Language::English.messages(), &RebaseStart::Root);

        assert!(
            hint.contains("git rebase -i --autosquash --root"),
            "the command must not depend on the display language: {hint}"
        );
        assert!(
            hint.contains("first commit"),
            "the reason for --root should be explained: {hint}"
        );
    }

    #[test]
    fn a_repository_without_staged_changes_is_detected_before_the_finder() {
        let (dir, repository) = repository_with_two_commits("fixup-staged-check");
        write_file(dir.path(), "a.txt", "unstaged\n");

        let staged = changes(&repository, ChangeScope::Staged).expect("status should be read");

        assert!(
            staged.is_empty(),
            "an unstaged change must not count as staged: {staged:?}"
        );
    }
}
