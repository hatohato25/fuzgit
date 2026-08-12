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

/// detached HEAD で実行した場合の案内（`gz diff --upstream` と同じ扱い）。
const DETACHED_MESSAGE: &str = "detached HEAD には upstream がありません。\
`gz branch` でブランチへ切り替えてから実行してください";

/// `--rebase` の確認プロンプトに添える履歴改変の注記（`gz rebase` と同じ内容）。
const HISTORY_REWRITE_NOTE: &str = "rebase は replay したコミットを作り直すため、\
コミットハッシュが変わります（push 済みのコミットを含む場合は特に注意してください）";

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
    pub fn from_flags(rebase: bool, merge: bool) -> Result<Self> {
        match (rebase, merge) {
            (false, false) => Ok(Self::FfOnly),
            (true, false) => Ok(Self::Rebase),
            (false, true) => Ok(Self::Merge),
            (true, true) => bail!("`--rebase` / `--merge` は同時に指定できません"),
        }
    }

    /// 確認プロンプトに添える注記。履歴を書き換える方式の場合のみ返す。
    fn note(self) -> Option<&'static str> {
        match self {
            SyncMode::FfOnly | SyncMode::Merge => None,
            SyncMode::Rebase => Some(HISTORY_REWRITE_NOTE),
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
pub fn run(repository: &gix::Repository, mode: SyncMode) -> Result<()> {
    // 進行中の merge / rebase を残したまま新しい取り込みは開始できないため、
    // fetch する前に復帰メニューへ委譲する（`gz merge` / `gz rebase` と同じ）
    if let Some(operation) = operation_in_progress(repository) {
        return in_progress::run(repository, operation);
    }

    let target = resolve_target(repository)?;

    let arguments = fetch_args(&target.remote);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).with_context(|| {
        format!(
            "リモート `{remote}` からの取得に失敗しました",
            remote = target.remote
        )
    })?;

    // fetch で追跡参照が更新されているため、取り込む量はここで初めて確定する
    let position = ahead_behind(workdir(repository)?, &target.branch, &target.reference)
        .with_context(|| {
            format!(
                "`{reference}` との差の算出に失敗しました",
                reference = target.reference
            )
        })?;
    let Some((ahead, behind)) = position else {
        bail!(missing_tracking_message(&target));
    };

    if behind == 0 {
        return report_up_to_date(&mut std::io::stderr(), &target, ahead);
    }

    let arguments = integrate_args(mode, &target.reference);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    confirm(
        &confirmation_header(&target, behind, mode),
        &[&command_display(&arguments)],
    )?;

    run_git(&arguments).with_context(|| {
        format!(
            "`{reference}` の取り込みに失敗しました",
            reference = target.reference
        )
    })?;

    Ok(())
}

/// 同期の対象を確定する。
///
/// # Errors
///
/// detached HEAD の場合、upstream が設定されていない場合、追跡参照を組み立てられない
/// 設定の場合、upstream のリモートが登録されていない場合に、それぞれの原因を示す
/// エラーを返す（対象を推測して別のリモート・ブランチへ倒さない）。
fn resolve_target(repository: &gix::Repository) -> Result<SyncTarget> {
    let Some(branch) = current_branch(repository).context("現在のブランチの取得に失敗しました")?
    else {
        bail!("{DETACHED_MESSAGE}");
    };

    let Some(upstream) = read_upstream(repository, &branch)
        .with_context(|| format!("`{branch}` の upstream の取得に失敗しました"))?
    else {
        bail!(
            "`{branch}` に upstream が設定されていません。\
`gz push -u` で push するか、`git branch --set-upstream-to=<remote>/<branch>` で設定してください"
        );
    };

    let Some(reference) = upstream.tracking_ref() else {
        bail!(
            "`{branch}` の upstream（{remote} / {merge_ref}）からリモート追跡参照を組み立てられません。\
`git branch --set-upstream-to=<remote>/<branch>` で設定し直してください",
            remote = upstream.remote,
            merge_ref = upstream.merge_ref
        );
    };

    // `branch.<name>.remote` には URL を直接書くこともできる。その値をそのまま
    // `git fetch` へ渡すと、fuzgit が列挙していない対象へネットワーク接続することになるため、
    // 登録済みのリモート名であることを確かめてから使う（design.md セキュリティ設計）
    let remotes = remotes(repository).context("リモート一覧の取得に失敗しました")?;
    if !remotes.contains(&upstream.remote) {
        bail!(
            "`{branch}` の upstream に設定された `{remote}` は登録済みのリモートではありません。\
`git remote add <名前> <URL>` で登録するか、\
`git branch --set-upstream-to=<remote>/<branch>` で設定し直してください",
            remote = upstream.remote
        );
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
fn missing_tracking_message(target: &SyncTarget) -> String {
    format!(
        "リモート追跡参照 `{reference}` が見つかりません。\
`{remote}` に `{branch}` の upstream が存在しない可能性があります\
（`gz push -u` で作成するか、`git branch --set-upstream-to=<remote>/<branch>` で設定し直してください）",
        reference = target.reference,
        remote = target.remote,
        branch = target.branch
    )
}

/// 取り込むコミットが無いことを伝える文言を組み立てる。
///
/// ahead が残っている場合はその件数も添える。「最新である」だけでは、まだ push していない
/// コミットを抱えていることに気付けないため。
fn up_to_date_message(target: &SyncTarget, ahead: usize) -> String {
    let mut message = format!(
        "`{branch}` は最新です（`{reference}` から取り込むコミットはありません）",
        branch = target.branch,
        reference = target.reference
    );

    if ahead > 0 {
        message.push_str(&format!(
            "\npush していないコミットが {ahead} 件あります（`gz push` で push できます）"
        ));
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
    writer: &mut impl std::io::Write,
    target: &SyncTarget,
    ahead: usize,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = up_to_date_message(target, ahead)
    )
    .context("標準エラー出力への書き込みに失敗しました")?;

    Ok(())
}

/// 確認プロンプトの見出しを組み立てる。
///
/// 取り込まれるコミット数を示し、`--rebase` の場合は履歴改変であることを添える
/// （実行するコマンドは対象行として別に提示する）。
fn confirmation_header(target: &SyncTarget, count: usize, mode: SyncMode) -> String {
    let mut header = format!(
        "`{reference}` から {count} 件のコミットを `{branch}` へ取り込みます",
        reference = target.reference,
        branch = target.branch
    );

    if let Some(note) = mode.note() {
        header.push('\n');
        header.push_str(note);
    }

    header
}

#[cfg(test)]
mod tests {
    use super::*;

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
            SyncMode::from_flags(false, false).expect("no flag is a valid combination"),
            SyncMode::FfOnly
        );
    }

    #[test]
    fn each_integration_flag_selects_its_own_mode() {
        assert_eq!(
            SyncMode::from_flags(true, false).expect("--rebase is valid on its own"),
            SyncMode::Rebase
        );
        assert_eq!(
            SyncMode::from_flags(false, true).expect("--merge is valid on its own"),
            SyncMode::Merge
        );
    }

    #[test]
    fn combining_the_integration_flags_is_rejected() {
        let err = SyncMode::from_flags(true, true).expect_err("the modes are mutually exclusive");

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
        let header = confirmation_header(&target(), 3, SyncMode::FfOnly);

        assert!(
            header.contains("`refs/remotes/origin/main`"),
            "unexpected header: {header}"
        );
        assert!(header.contains("3 件"), "unexpected header: {header}");
        assert!(header.contains("`main`"), "unexpected header: {header}");
    }

    #[test]
    fn only_the_rebase_confirmation_warns_about_the_rewritten_history() {
        let rebase = confirmation_header(&target(), 2, SyncMode::Rebase);
        assert!(
            rebase.contains("コミットハッシュが変わります"),
            "a history rewrite must be spelled out: {rebase}"
        );

        for mode in [SyncMode::FfOnly, SyncMode::Merge] {
            let header = confirmation_header(&target(), 2, mode);
            assert!(
                !header.contains("コミットハッシュが変わります"),
                "{mode:?} does not rewrite history: {header}"
            );
        }
    }

    #[test]
    fn the_history_note_belongs_to_the_rebase_mode_only() {
        assert_eq!(SyncMode::Rebase.note(), Some(HISTORY_REWRITE_NOTE));
        assert_eq!(SyncMode::FfOnly.note(), None);
        assert_eq!(SyncMode::Merge.note(), None);
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

        report_up_to_date(&mut output, &target(), 0).expect("writing to a buffer cannot fail");

        let text = String::from_utf8(output).expect("the message should be utf-8");
        assert!(text.contains("最新です"), "unexpected message: {text}");
        assert!(
            !text.contains("push していない"),
            "there is nothing to push: {text}"
        );
    }

    #[test]
    fn the_commits_that_are_not_pushed_yet_are_mentioned() {
        let message = up_to_date_message(&target(), 2);

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
    fn a_missing_tracking_reference_names_the_remote_and_the_branch() {
        let message = missing_tracking_message(&target());

        assert!(
            message.contains("refs/remotes/origin/main"),
            "unexpected message: {message}"
        );
        assert!(message.contains("origin"), "unexpected message: {message}");
        assert!(message.contains("main"), "unexpected message: {message}");
    }
}
