//! `gz sync` — 現在のブランチを upstream と同期する（FR-19）。
//!
//! 対象は「現在のブランチの upstream」に固定する。remote × branch を選ばせる
//! `gz pull` 相当のフローは提供せず（requirements.md「スコープ外」）、任意のリモート・
//! 任意のブランチからの取り込みは `gz fetch` ＋ `gz merge` / `gz rebase` で行う。
//!
//! ネットワークへ出るのは `git fetch <remote>` の 1 回だけで、候補生成や表示のために
//! リモートへ問い合わせることはない（design.md「候補生成・プレビューでネットワーク
//! アクセスを行わない」）。タイムアウト・リトライ・認証情報の取り扱いも行わず、
//! 到達不能・認証拒否は git の標準メッセージのまま非ゼロ終了する。
//!
//! 取り込み方式の既定は fast-forward のみ。fast-forward できない（diverged）場合は
//! git のエラーをそのまま表示して停止し、暗黙に merge / rebase へ倒さない
//! （履歴の統合方法はユーザーの明示指定に限る。design.md セキュリティ設計）。

use anyhow::{Context as _, Result, bail};

use crate::commands::command_display;
use crate::commands::confirmation::confirm;
use crate::commands::in_progress;
use crate::git::exec::run_git;
use crate::git::read::{
    ahead_behind, current_branch, operation_in_progress, remotes, upstream as read_upstream,
};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};

/// 取り込み方式（FR-19）。
///
/// clap の排他フラグ 2 つをそのまま `commands` 層へ持ち回さず、取り得る 3 通りを型で表す
/// （[`crate::commands::merge::MergeMode`] と同方針）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// fast-forward できる場合のみ取り込む（既定）。
    ///
    /// 既定をこれにするのは「指定が無いので安全側へ倒す」ためではなく、履歴の統合方法
    /// （merge コミットを作るのか、作り直すのか）をユーザーの明示指定に限るため。
    /// fast-forward できない場合は git のエラーをそのまま見せて停止する。
    FfOnly,
    /// upstream の上へ rebase して取り込む（`--rebase`）。
    Rebase,
    /// upstream を merge して取り込む（`--merge`）。
    Merge,
}

impl SyncMode {
    /// `--rebase` / `--merge` の指定から取り込み方式を決める。
    ///
    /// 排他性は `clap` の `conflicts_with_all` でも担保しているが、両方が立った状態を
    /// 暗黙にどちらかへ倒すことがないよう、ここでも明示的に拒否する。
    ///
    /// # Errors
    ///
    /// 両方が同時に指定された場合にエラーを返す。
    pub fn from_flags(messages: &dyn Messages, rebase: bool, merge: bool) -> Result<Self> {
        match (rebase, merge) {
            (false, false) => Ok(Self::FfOnly),
            (true, false) => Ok(Self::Rebase),
            (false, true) => Ok(Self::Merge),
            (true, true) => bail!(messages.sync().conflicting_modes()),
        }
    }

    /// 確認プロンプトに添える注記。履歴を書き換える方式の場合のみ返す。
    ///
    /// 注記は `gz rebase` と同じ内容であるため共有語彙から引く
    /// （[`crate::i18n::messages::CommonMessages::history_rewrite_note`]）。
    fn note(self, messages: &dyn Messages) -> Option<&'static str> {
        match self {
            SyncMode::FfOnly | SyncMode::Merge => None,
            SyncMode::Rebase => Some(messages.common().history_rewrite_note()),
        }
    }
}

/// 同期の対象（現在のブランチと、その upstream）。
///
/// `remote` は [`remotes`] が列挙した名前と一致することを確認済みの値、`reference` は
/// gix が組み立てたローカルの追跡参照であり、いずれもユーザーの自由入力ではない
/// （`git fetch` / `git merge` / `git rebase` の位置引数は `--` で保護できないため、
/// 値の由来で担保する。design.md セキュリティ設計）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SyncTarget {
    /// 現在のブランチの短縮名。
    branch: String,
    /// fetch するリモート名。
    remote: String,
    /// upstream のローカル追跡参照（`refs/remotes/<remote>/<branch>`）。
    reference: String,
}

/// upstream を fetch し、確認のうえ現在のブランチへ取り込む。
///
/// merge / rebase が進行中の場合は同期を始めず、復帰メニュー（FR-14）を表示する。
///
/// # Errors
///
/// upstream が定まらない場合（detached HEAD・upstream 未設定・追跡参照を組み立てられない
/// 設定・リモートが未登録）、`git fetch` の実行、ahead/behind の算出、取り込みの実行に
/// 失敗した場合にエラーを返す。確認プロンプトで承認が得られなかった場合は
/// [`crate::error::Error::Cancelled`]。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    mode: SyncMode,
) -> Result<()> {
    // 進行中の merge / rebase を残したまま新しい取り込みは開始できないため、
    // fetch する前に復帰メニューへ委譲する（`gz merge` / `gz rebase` と同じ）
    if let Some(operation) = operation_in_progress(repository) {
        return in_progress::run(language, messages, repository, operation);
    }

    let target = resolve_target(messages, repository)?;

    let arguments = fetch_args(&target.remote);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).with_context(|| messages.sync().fetch_failed(&target.remote))?;

    // fetch で追跡参照が更新されているため、取り込む量はここで初めて確定する
    let position = ahead_behind(workdir(repository)?, &target.branch, &target.reference)
        .with_context(|| messages.common().ahead_behind_failed(&target.reference))?;
    let Some((ahead, behind)) = position else {
        bail!(missing_tracking_message(messages, &target));
    };

    if behind == 0 {
        return report_up_to_date(messages, &mut std::io::stderr(), &target, ahead);
    }

    let arguments = integrate_args(mode, &target.reference);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    confirm(
        messages,
        &confirmation_header(messages, &target, behind, mode),
        &[&command_display(&arguments)],
    )?;

    run_git(language, &arguments)
        .with_context(|| messages.sync().integration_failed(&target.reference))?;

    Ok(())
}

/// 同期の対象を確定する。
///
/// # Errors
///
/// detached HEAD の場合、upstream が設定されていない場合、追跡参照を組み立てられない
/// 設定の場合、upstream のリモートが登録されていない場合に、それぞれの原因を示す
/// エラーを返す（対象を推測して別のリモート・ブランチへ倒さない）。
fn resolve_target(messages: &dyn Messages, repository: &gix::Repository) -> Result<SyncTarget> {
    let Some(branch) =
        current_branch(repository).context(messages.common().current_branch_read_failed())?
    else {
        bail!(messages.common().detached_head_without_upstream());
    };

    let Some(upstream) = read_upstream(repository, &branch)
        .with_context(|| messages.common().upstream_read_failed(&branch))?
    else {
        bail!(messages.common().upstream_not_configured(&branch));
    };

    let Some(reference) = upstream.tracking_ref() else {
        bail!(messages.sync().tracking_ref_unavailable(
            &branch,
            &upstream.remote,
            &upstream.merge_ref
        ));
    };

    // `branch.<name>.remote` には URL を直接書くこともできる。その値をそのまま
    // `git fetch` へ渡すと、fuzgit が列挙していない対象へネットワーク接続することになるため、
    // 登録済みのリモート名であることを確かめてから使う（design.md セキュリティ設計）
    let remotes = remotes(repository).context(messages.common().remote_list_read_failed())?;
    if !remotes.contains(&upstream.remote) {
        bail!(messages.sync().unknown_remote(&branch, &upstream.remote));
    }

    Ok(SyncTarget {
        branch,
        remote: upstream.remote,
        reference,
    })
}

/// `git fetch <remote>` の引数を組み立てる。
///
/// `--prune` は付けない。追跡参照の削除は `gz fetch --prune` としてユーザーが明示指定する
/// 操作であり、同期のついでに行うものではない。
fn fetch_args(remote: &str) -> Vec<String> {
    vec!["fetch".to_owned(), remote.to_owned()]
}

/// 取り込みに用いる git コマンドの引数を組み立てる。
///
/// `gz pull`（FR-24）が現在のブランチへ fast-forward で取り込む経路からも呼ぶ。
/// 「現在のブランチ 1 本を ff-only で取り込む」場合だけが両コマンドの重なりであり、
/// 引数の組み立てを 2 か所に持つと片方だけが変わり得るため、ここへ集約する
/// （両者が一致することは `commands::pull` の単体テストでも固定している）。
pub(crate) fn integrate_args(mode: SyncMode, reference: &str) -> Vec<String> {
    let mut args = match mode {
        SyncMode::FfOnly => vec!["merge".to_owned(), "--ff-only".to_owned()],
        SyncMode::Merge => vec!["merge".to_owned()],
        SyncMode::Rebase => vec!["rebase".to_owned()],
    };
    args.push(reference.to_owned());
    args
}

/// fetch しても追跡参照が現れなかった場合のエラーメッセージを組み立てる。
///
/// upstream の設定はあるがリモート側にブランチが無い（削除された・まだ push していない）
/// 場合に起きる。取り込む対象が無い以上、別の参照へ倒さず原因を示して停止する。
fn missing_tracking_message(messages: &dyn Messages, target: &SyncTarget) -> String {
    messages
        .sync()
        .missing_tracking_ref(&target.reference, &target.remote, &target.branch)
}

/// 取り込むコミットが無いことを伝える文言を組み立てる。
///
/// ahead が残っている場合はその件数も添える。「最新である」だけでは、まだ push していない
/// コミットを抱えていることに気付けないため。
fn up_to_date_message(messages: &dyn Messages, target: &SyncTarget, ahead: usize) -> String {
    let mut message = messages
        .sync()
        .up_to_date(&target.branch, &target.reference);

    if ahead > 0 {
        // 本文と注記を区切る改行は装飾であるため、文言ではなくここで付ける
        message.push('\n');
        message.push_str(&messages.sync().unpushed_commits(ahead));
    }

    message
}

/// 取り込むコミットが無いことを伝えて正常終了する。
///
/// 最新であることは正常な結果であるためエラーにしない（requirements.md FR-19）。
/// 標準出力はパイプ用途のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_up_to_date(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
    target: &SyncTarget,
    ahead: usize,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = up_to_date_message(messages, target, ahead)
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// 確認プロンプトの見出しを組み立てる。
///
/// 取り込まれるコミット数を示し、`--rebase` の場合は履歴改変であることを添える
/// （実行するコマンドは対象行として別に提示する）。
fn confirmation_header(
    messages: &dyn Messages,
    target: &SyncTarget,
    count: usize,
    mode: SyncMode,
) -> String {
    let mut header = messages
        .sync()
        .confirmation(&target.reference, count, &target.branch);

    if let Some(note) = mode.note(messages) {
        header.push('\n');
        header.push_str(note);
    }

    header
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 既定（日本語）の文言一式。文言そのものを固定するテスト以外はこれを使う。
    fn messages() -> &'static dyn Messages {
        Language::Japanese.messages()
    }

    fn target() -> SyncTarget {
        SyncTarget {
            branch: "main".to_owned(),
            remote: "origin".to_owned(),
            reference: "refs/remotes/origin/main".to_owned(),
        }
    }

    #[test]
    fn no_flag_integrates_by_fast_forward_only() {
        assert_eq!(
            SyncMode::from_flags(messages(), false, false).expect("no flag is a valid combination"),
            SyncMode::FfOnly
        );
    }

    #[test]
    fn each_integration_flag_selects_its_own_mode() {
        assert_eq!(
            SyncMode::from_flags(messages(), true, false).expect("--rebase is valid on its own"),
            SyncMode::Rebase
        );
        assert_eq!(
            SyncMode::from_flags(messages(), false, true).expect("--merge is valid on its own"),
            SyncMode::Merge
        );
    }

    #[test]
    fn combining_the_integration_flags_is_rejected() {
        let err = SyncMode::from_flags(messages(), true, true)
            .expect_err("the modes are mutually exclusive");

        assert!(
            err.to_string().contains("同時に指定できません"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn the_default_never_falls_back_to_merge_or_rebase() {
        // fast-forward できない場合に暗黙で別の方式へ倒さないことを、引数の形で固定する
        let arguments = integrate_args(SyncMode::FfOnly, "refs/remotes/origin/main");

        assert_eq!(
            arguments,
            ["merge", "--ff-only", "refs/remotes/origin/main"]
        );
    }

    #[test]
    fn merging_passes_the_tracking_reference_without_the_fast_forward_option() {
        assert_eq!(
            integrate_args(SyncMode::Merge, "refs/remotes/origin/main"),
            ["merge", "refs/remotes/origin/main"]
        );
    }

    #[test]
    fn rebasing_replays_onto_the_tracking_reference() {
        assert_eq!(
            integrate_args(SyncMode::Rebase, "refs/remotes/origin/feature/login"),
            ["rebase", "refs/remotes/origin/feature/login"]
        );
    }

    #[test]
    fn fetching_names_the_remote_and_nothing_else() {
        // `--prune` は `gz fetch --prune` の明示指定に限る操作であり、同期では付けない
        assert_eq!(fetch_args("origin"), ["fetch", "origin"]);
    }

    #[test]
    fn the_confirmation_names_the_upstream_the_count_and_the_branch() {
        let header = confirmation_header(messages(), &target(), 3, SyncMode::FfOnly);

        assert!(
            header.contains("`refs/remotes/origin/main`"),
            "unexpected header: {header}"
        );
        assert!(header.contains("3 件"), "unexpected header: {header}");
        assert!(header.contains("`main`"), "unexpected header: {header}");
    }

    #[test]
    fn only_the_rebase_confirmation_warns_about_the_rewritten_history() {
        let rebase = confirmation_header(messages(), &target(), 2, SyncMode::Rebase);
        assert!(
            rebase.contains("コミットハッシュが変わります"),
            "a history rewrite must be spelled out: {rebase}"
        );

        for mode in [SyncMode::FfOnly, SyncMode::Merge] {
            let header = confirmation_header(messages(), &target(), 2, mode);
            assert!(
                !header.contains("コミットハッシュが変わります"),
                "{mode:?} does not rewrite history: {header}"
            );
        }
    }

    #[test]
    fn the_history_note_belongs_to_the_rebase_mode_only() {
        let messages = messages();

        assert_eq!(
            SyncMode::Rebase.note(messages),
            Some(messages.common().history_rewrite_note())
        );
        assert_eq!(SyncMode::FfOnly.note(messages), None);
        assert_eq!(SyncMode::Merge.note(messages), None);
    }

    #[test]
    fn the_confirmation_shows_the_command_that_runs() {
        let arguments = integrate_args(SyncMode::Rebase, "refs/remotes/origin/main");
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

        assert_eq!(
            command_display(&arguments),
            "git rebase refs/remotes/origin/main"
        );
    }

    #[test]
    fn being_up_to_date_is_reported_without_an_error() {
        let mut output = Vec::new();

        report_up_to_date(messages(), &mut output, &target(), 0)
            .expect("writing to a buffer cannot fail");

        let text = String::from_utf8(output).expect("the message should be utf-8");
        assert!(text.contains("最新です"), "unexpected message: {text}");
        assert!(
            !text.contains("push していない"),
            "there is nothing to push: {text}"
        );
    }

    #[test]
    fn the_commits_that_are_not_pushed_yet_are_mentioned() {
        let message = up_to_date_message(messages(), &target(), 2);

        assert!(
            message.contains("最新です"),
            "unexpected message: {message}"
        );
        assert!(
            message.contains("2 件"),
            "the local commits should be counted: {message}"
        );
    }

    #[test]
    fn every_sync_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let sync = language.messages().sync();

            assert!(
                !sync.conflicting_modes().trim().is_empty(),
                "{language:?} left a message empty"
            );

            for (text, argument) in [
                (sync.fetch_failed("origin"), "origin"),
                (
                    sync.tracking_ref_unavailable("main", "origin", "refs/heads/main"),
                    "refs/heads/main",
                ),
                (
                    sync.unknown_remote("main", "https://example.com/x.git"),
                    "https://example.com/x.git",
                ),
                (
                    sync.missing_tracking_ref("refs/remotes/origin/main", "origin", "main"),
                    "refs/remotes/origin/main",
                ),
                (
                    sync.up_to_date("main", "refs/remotes/origin/main"),
                    "refs/remotes/origin/main",
                ),
                (sync.unpushed_commits(2), "2"),
                (
                    sync.integration_failed("refs/remotes/origin/main"),
                    "refs/remotes/origin/main",
                ),
                (
                    sync.confirmation("refs/remotes/origin/main", 3, "main"),
                    "3",
                ),
            ] {
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_sync_wording_is_translated() {
        let japanese = Language::Japanese.messages().sync();
        let english = Language::English.messages().sync();

        assert_ne!(japanese.conflicting_modes(), english.conflicting_modes());
        assert_ne!(
            japanese.fetch_failed("origin"),
            english.fetch_failed("origin")
        );
        assert_ne!(
            japanese.tracking_ref_unavailable("main", "origin", "refs/heads/main"),
            english.tracking_ref_unavailable("main", "origin", "refs/heads/main")
        );
        assert_ne!(
            japanese.unknown_remote("main", "origin"),
            english.unknown_remote("main", "origin")
        );
        assert_ne!(
            japanese.missing_tracking_ref("refs/remotes/origin/main", "origin", "main"),
            english.missing_tracking_ref("refs/remotes/origin/main", "origin", "main")
        );
        assert_ne!(
            japanese.up_to_date("main", "refs/remotes/origin/main"),
            english.up_to_date("main", "refs/remotes/origin/main")
        );
        assert_ne!(japanese.unpushed_commits(2), english.unpushed_commits(2));
        assert_ne!(
            japanese.integration_failed("refs/remotes/origin/main"),
            english.integration_failed("refs/remotes/origin/main")
        );
        assert_ne!(
            japanese.confirmation("refs/remotes/origin/main", 3, "main"),
            english.confirmation("refs/remotes/origin/main", 3, "main")
        );
    }

    #[test]
    fn the_english_count_agrees_with_the_noun_it_qualifies() {
        let english = Language::English.messages().sync();

        assert!(
            english
                .confirmation("refs/remotes/origin/main", 1, "main")
                .contains("1 commit "),
            "a single commit must not be pluralised: {text}",
            text = english.confirmation("refs/remotes/origin/main", 1, "main")
        );
        assert!(
            english
                .confirmation("refs/remotes/origin/main", 2, "main")
                .contains("2 commits "),
            "several commits must be pluralised: {text}",
            text = english.confirmation("refs/remotes/origin/main", 2, "main")
        );
        assert!(
            english.unpushed_commits(1).contains("1 commit has"),
            "the verb must agree as well: {text}",
            text = english.unpushed_commits(1)
        );
        assert!(
            english.unpushed_commits(3).contains("3 commits have"),
            "the verb must agree as well: {text}",
            text = english.unpushed_commits(3)
        );
    }

    #[test]
    fn a_missing_tracking_reference_names_the_remote_and_the_branch() {
        let message = missing_tracking_message(messages(), &target());

        assert!(
            message.contains("refs/remotes/origin/main"),
            "unexpected message: {message}"
        );
        assert!(message.contains("origin"), "unexpected message: {message}");
        assert!(message.contains("main"), "unexpected message: {message}");
    }
}
