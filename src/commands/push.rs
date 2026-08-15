//! `gz push` — push 先（リモート × 現在のブランチ）を選択して push する（FR-10）。
//!
//! force push（`--force` / `--force-with-lease`）は提供しない（requirements.md「スコープ外」）。

use anyhow::{Context as _, Result, anyhow, bail};

use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::run_git;
use crate::git::read::{PushTarget, push_targets};
use crate::i18n::{Language, Messages};

/// `git push` に upstream を設定させるオプション（`-u` の長い綴り）。
const SET_UPSTREAM_OPTION: &str = "--set-upstream";

/// 現在のブランチの upstream に対応する候補であることを示す注記。
///
/// 記号ではなく語で示すのは、凡例なしで意味が分かるようにするため
/// （絞り込みのクエリとしても使える）。
const UPSTREAM_NOTE: &str = "  (upstream)";

/// プレビューに表示する最大コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// push 先を現在のブランチの upstream として設定するかどうか。
///
/// upstream の設定はユーザーの明示指定に限り、暗黙に設定を書き換えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamUpdate {
    /// 既存の upstream 設定に触れない（既定）。
    Keep,
    /// push 先を upstream として設定する（`-u` / `--set-upstream`）。
    Set,
}

impl UpstreamUpdate {
    /// `git push` に付けるオプション。
    fn option(self) -> Option<&'static str> {
        match self {
            UpstreamUpdate::Keep => None,
            UpstreamUpdate::Set => Some(SET_UPSTREAM_OPTION),
        }
    }
}

/// push 先を 1 件選び、`git push <remote> <branch>` を実行する。
///
/// 実行は継承 stdio で行い、認証プロンプトと進捗表示は git に委ねる。
///
/// # Errors
///
/// 候補の取得（detached HEAD を含む）、選択（中断を含む）、`git push` の実行に失敗した場合に
/// エラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    upstream: UpstreamUpdate,
) -> Result<()> {
    let candidates = push_targets(repository).context(messages.push().targets_read_failed())?;
    if candidates.is_empty() {
        bail!(messages.push().no_remotes());
    }

    let items = candidates
        .iter()
        .map(|target| to_item(language, messages, target))
        .collect();
    let selected = select_one(items)?;

    // `git push` はパス以外の位置引数を取り `--` で保護できないため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let target = candidates
        .iter()
        .find(|candidate| candidate.tracking_name() == selected)
        .ok_or_else(|| anyhow!(messages.push().selection_not_found(&selected)))?;

    let arguments = push_args(target, upstream);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments)
        .with_context(|| messages.push().push_failed(&target.tracking_name()))?;

    Ok(())
}

/// 一覧に表示する 1 行を組み立てる。この文字列がそのまま絞り込みの対象になる。
fn display_line(messages: &dyn Messages, target: &PushTarget) -> String {
    let mut line = format!(
        "{name}  {counts}",
        name = target.tracking_name(),
        counts = counts(messages, target)
    );
    if target.is_upstream {
        line.push_str(UPSTREAM_NOTE);
    }
    line
}

/// リモート追跡参照に対する進み・遅れの表示。
///
/// 進み・遅れは数値であり訳す余地が無いが、追跡参照が無い場合の注記は fuzgit 自身の
/// 説明語であるため表示言語に合わせる。
fn counts(messages: &dyn Messages, target: &PushTarget) -> String {
    match target.ahead_behind {
        Some((ahead, behind)) => format!("ahead {ahead} / behind {behind}"),
        // まだ push していないリモートには比較対象が無く、進み・遅れを数えられない
        None => messages.push().no_tracking_ref().to_owned(),
    }
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
///
/// 追跡参照がある場合はそこから HEAD までの差分（= push されるコミット）を表示する。
/// 追跡参照が無い場合は push によって参照が新規作成され、HEAD から辿れるコミットがすべて
/// リモート側に現れるため、範囲を絞らず HEAD を起点にする。
fn preview_args(target: &PushTarget) -> Vec<String> {
    let range = match target.ahead_behind {
        Some(_) => format!("{reference}..HEAD", reference = target.tracking_ref()),
        None => "HEAD".to_owned(),
    };

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

/// push 先を finder の候補へ変換する。
fn to_item(language: Language, messages: &dyn Messages, target: &PushTarget) -> FinderItem {
    FinderItem::new(
        display_line(messages, target),
        target.tracking_name(),
        PreviewSource::Git(preview_args(target)),
        language.messages(),
    )
}

/// `git push [--set-upstream] <remote> <branch>` の引数を組み立てる。
///
/// リモート名・ブランチ名は gix が列挙した候補に由来する値だけを渡す
/// （`git push` の位置引数は `--` で保護できないため、値の由来で担保する）。
fn push_args(target: &PushTarget, upstream: UpstreamUpdate) -> Vec<String> {
    let mut args = vec!["push".to_owned()];
    if let Some(option) = upstream.option() {
        args.push(option.to_owned());
    }
    args.push(target.remote.clone());
    args.push(target.branch.clone());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(remote: &str, ahead_behind: Option<(usize, usize)>) -> PushTarget {
        PushTarget {
            remote: remote.to_owned(),
            branch: "main".to_owned(),
            is_upstream: false,
            ahead_behind,
        }
    }

    #[test]
    fn a_line_shows_the_destination_with_its_counts() {
        assert_eq!(
            display_line(
                Language::Japanese.messages(),
                &target("origin", Some((2, 1)))
            ),
            "origin/main  ahead 2 / behind 1"
        );
    }

    #[test]
    fn a_destination_without_a_tracking_reference_says_so() {
        assert_eq!(
            display_line(Language::Japanese.messages(), &target("backup", None)),
            "backup/main  追跡参照なし"
        );
    }

    #[test]
    fn the_upstream_destination_is_marked() {
        let upstream = PushTarget {
            is_upstream: true,
            ..target("origin", Some((0, 0)))
        };

        assert_eq!(
            display_line(Language::Japanese.messages(), &upstream),
            "origin/main  ahead 0 / behind 0  (upstream)"
        );
        assert!(
            !display_line(
                Language::Japanese.messages(),
                &target("origin", Some((0, 0)))
            )
            .contains("upstream"),
            "a destination that is not the upstream must not be marked"
        );
    }

    #[test]
    fn the_preview_lists_the_commits_that_would_be_pushed() {
        assert_eq!(
            preview_args(&target("origin", Some((2, 0)))),
            [
                "log",
                "--color=always",
                "--oneline",
                "--decorate",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "refs/remotes/origin/main..HEAD",
                "--"
            ],
            "the tracking reference is addressed by its full name"
        );
    }

    #[test]
    fn the_preview_of_a_new_destination_starts_at_head() {
        let arguments = preview_args(&target("backup", None));

        assert!(
            arguments.contains(&"HEAD".to_owned()),
            "a destination without a tracking reference has no range: {arguments:?}"
        );
        assert!(
            !arguments.iter().any(|argument| argument.contains("..")),
            "a missing reference must not be used as a range: {arguments:?}"
        );
    }

    #[test]
    fn an_item_keeps_the_destination_as_its_key() {
        let item = to_item(
            Language::Japanese,
            Language::Japanese.messages(),
            &target("origin", Some((1, 0))),
        );

        assert_eq!(item.key(), "origin/main");
    }

    #[test]
    fn pushing_passes_the_remote_and_the_branch_as_they_were_listed() {
        assert_eq!(
            push_args(&target("origin", Some((1, 0))), UpstreamUpdate::Keep),
            ["push", "origin", "main"]
        );
    }

    #[test]
    fn setting_the_upstream_adds_its_option() {
        assert_eq!(
            push_args(&target("origin", None), UpstreamUpdate::Set),
            ["push", SET_UPSTREAM_OPTION, "origin", "main"]
        );
    }

    #[test]
    fn the_upstream_is_left_alone_unless_it_was_asked_for() {
        assert_eq!(UpstreamUpdate::Keep.option(), None);
        assert_eq!(UpstreamUpdate::Set.option(), Some(SET_UPSTREAM_OPTION));
    }

    #[test]
    fn a_branch_containing_a_slash_keeps_its_full_name() {
        let target = PushTarget {
            branch: "feature/login".to_owned(),
            ..target("origin", None)
        };

        assert_eq!(
            push_args(&target, UpstreamUpdate::Keep),
            ["push", "origin", "feature/login"]
        );
        assert_eq!(target.tracking_name(), "origin/feature/login");
    }

    #[test]
    fn every_push_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let push = language.messages().push();

            for text in [
                push.targets_read_failed(),
                push.no_remotes(),
                push.no_tracking_ref(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            // ユーザーがそのまま打ち込むコマンドであるため、どの言語でも同じ綴りで現れる
            assert!(
                push.no_remotes().contains("git remote add"),
                "{language:?} must spell out the command: {text}",
                text = push.no_remotes()
            );

            assert!(
                push.selection_not_found("origin/main")
                    .contains("origin/main"),
                "{language:?} must name the selection"
            );
            assert!(
                push.push_failed("origin/main").contains("origin/main"),
                "{language:?} must name the destination"
            );
        }
    }

    #[test]
    fn the_push_wording_is_translated() {
        let japanese = Language::Japanese.messages().push();
        let english = Language::English.messages().push();

        assert_ne!(
            japanese.targets_read_failed(),
            english.targets_read_failed()
        );
        assert_ne!(japanese.no_remotes(), english.no_remotes());
        assert_ne!(japanese.no_tracking_ref(), english.no_tracking_ref());
        assert_ne!(
            japanese.selection_not_found("origin/main"),
            english.selection_not_found("origin/main")
        );
        assert_ne!(
            japanese.push_failed("origin/main"),
            english.push_failed("origin/main")
        );
    }

    #[test]
    fn a_destination_without_a_tracking_reference_says_so_in_english() {
        // 候補行の主たる内容（リモート名・ブランチ名）は訳さないが、注記は表示言語に従う
        assert_eq!(
            display_line(Language::English.messages(), &target("backup", None)),
            "backup/main  no tracking ref"
        );
    }
}
