//! `gz branch create` / `delete` / `cleanup` — ブランチの作成・削除・整理（FR-20）。
//!
//! ブランチの切替（FR-1）は [`crate::commands::branch`] が担う。切替は引数なしの
//! `gz branch` として従来どおり維持し、このモジュールはサブコマンドで指定された
//! 管理操作だけを扱う。
//!
//! 削除は取り消せない操作であるため、対象を全件列挙した確認プロンプトを必ず挟む。
//! 既定は `git branch -d`（merged なブランチのみ削除できる）で、merged でない
//! ブランチの削除は `--force` の明示指定と警告付きの確認が揃って初めて実行する
//! （design.md セキュリティ設計）。

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::cli::BranchCommand;
use crate::commands::aligned_candidates;
use crate::commands::confirmation::confirm;
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_many_with, select_one,
};
use crate::git::exec::run_git;
use crate::git::read::{
    BranchInfo, BranchScope, TagInfo, branch_activity, branches, checked_out_branches,
    merged_branches, tags, upstream, worktrees,
};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};

/// merged 判定の既定の基準（`--into` 未指定時）。
const DEFAULT_MERGE_BASE: &str = "HEAD";

/// プレビューに表示する直近コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// 作成元候補の種別ラベルを揃える桁数（`branch` の 6 文字に合わせる）。
const KIND_WIDTH: usize = 6;

/// upstream 設定が指すリモート側ブランチの参照名の前置き。
const BRANCH_REF_PREFIX: &str = "refs/heads/";

/// `git branch -d`（merged なブランチのみ削除する）のオプション。
const SAFE_DELETE_OPTION: &str = "-d";

/// `git branch -D`（merged でないブランチも削除する）のオプション。
const FORCE_DELETE_OPTION: &str = "-D";

/// merged なブランチの表示。
const MERGED_LABEL: &str = "merged";

/// merged でないブランチの表示。
const UNMERGED_LABEL: &str = "unmerged";

/// ブランチ作成後に切り替えるかどうか。
///
/// 作成と切替は別の操作であり、`gz branch create` は既定では作成だけを行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchAfterCreate {
    /// 作成のみ行い、現在のブランチのままにする（既定）。
    Stay,
    /// 作成後にそのブランチへ切り替える（`--switch`）。
    Switch,
}

/// 削除に用いる `git branch` のオプション。
///
/// `-d` と `-D` は「merged でないブランチを消せるかどうか」だけが異なるため、
/// 真偽値を持ち回さず、どちらを実行するのかを型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    /// merged なブランチのみ削除する（`git branch -d`。既定）。
    MergedOnly,
    /// merged でないブランチも削除する（`git branch -D`）。
    Force,
}

impl DeleteMode {
    /// `git branch` に付けるオプション。
    fn option(self) -> &'static str {
        match self {
            DeleteMode::MergedOnly => SAFE_DELETE_OPTION,
            DeleteMode::Force => FORCE_DELETE_OPTION,
        }
    }
}

/// 新しいブランチの作成元候補の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseKind {
    /// ローカル・リモート追跡ブランチ。
    Branch,
    /// タグ。
    Tag,
}

impl BaseKind {
    /// 表示および finder キーの前置きに用いる呼称。
    fn label(self) -> &'static str {
        match self {
            BaseKind::Branch => "branch",
            BaseKind::Tag => "tag",
        }
    }
}

/// 新しいブランチの作成元候補 1 件分。
#[derive(Debug, Clone, PartialEq, Eq)]
struct BaseCandidate {
    /// finder のキー。
    ///
    /// 同じ名前のブランチとタグは共存できるため（`v1.0` のブランチとタグなど）、
    /// 名前だけをキーにすると別の対象を掴んでしまう。種別を前置きして一意にする
    /// （参照名は空白を含めない規則のため、`<種別> <名前>` は曖昧にならない）。
    key: String,
    /// 一覧表示および絞り込み対象の文字列。
    display: String,
    /// `git branch` へ渡すリビジョン。
    ///
    /// ブランチは名前をそのまま、タグは解決済みのオブジェクト ID を渡す
    /// （annotated tag のタグオブジェクトは git 側でコミットまで peel される）。
    revision: String,
}

/// 削除候補 1 件分。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeleteCandidate {
    /// ローカルブランチの短縮名（finder のキー、かつ `git branch` へ渡す値）。
    name: String,
    /// 基準ブランチへ取り込み済みかどうか。
    is_merged: bool,
    /// 最終更新の相対日時（`3 days ago` 形式）。取得できなかった場合は `None`。
    relative_date: Option<String>,
    /// upstream（リモート追跡ブランチ）の表示名。設定が無い場合は `None`。
    tracking: Option<String>,
}

/// ブランチ管理のサブコマンドを対応する処理へ振り分ける。
///
/// # Errors
///
/// 各操作が失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    command: &BranchCommand,
) -> Result<()> {
    match command {
        BranchCommand::Create { name, switch } => {
            let switch = if *switch {
                SwitchAfterCreate::Switch
            } else {
                SwitchAfterCreate::Stay
            };
            create(language, messages, repository, name, switch)
        }
        BranchCommand::Delete { force, into } => {
            let mode = if *force {
                DeleteMode::Force
            } else {
                DeleteMode::MergedOnly
            };
            delete(language, messages, repository, mode, into.as_deref())
        }
        BranchCommand::Cleanup { into } => cleanup(language, messages, repository, into.as_deref()),
    }
}

/// 作成元を 1 件選び、新しいブランチを作成する。
///
/// ブランチ名の妥当性は `git check-ref-format` に委ね、fuzgit 側では検証しない
/// （git が持つ規則を二重に実装すると、git 側の更新から取り残されるため）。
///
/// # Errors
///
/// 候補の取得、選択（中断を含む）、`git branch` / `git switch` の実行に失敗した場合に
/// エラーを返す。
fn create(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    name: &str,
    switch: SwitchAfterCreate,
) -> Result<()> {
    let branches = branches(repository, BranchScope::All)
        .context(messages.common().branch_list_read_failed())?;
    let tags = tags(repository).context(messages.common().tag_list_read_failed())?;
    let candidates = base_candidates(&branches, &tags);

    let items = candidates
        .iter()
        .map(|candidate| to_base_item(language, candidate))
        .collect();
    let selected = select_one(items)?;

    // `git branch` の作成元は `--` の後ろに置いてもオプション扱いから守れる位置に無いため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let base = candidates
        .iter()
        .find(|candidate| candidate.key == selected)
        .ok_or_else(|| anyhow!(messages.branch_manage().base_selection_not_found(&selected)))?;

    let arguments = create_args(name, &base.revision);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments)
        .with_context(|| messages.branch_manage().creation_failed(name))?;

    report_created(&mut std::io::stderr(), messages, name, base, switch)?;

    if switch == SwitchAfterCreate::Switch {
        let arguments = switch_args(name);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        run_git(language, &arguments).with_context(|| messages.common().switch_failed(name))?;
    }

    Ok(())
}

/// 作成元候補（ブランチ＋タグ）を組み立てる。
///
/// ブランチを先に並べる。作成元は多くの場合ブランチであり、タグからの作成
/// （リリース時点からの枝分かれ）は相対的に少ないため。
fn base_candidates(branches: &[BranchInfo], tags: &[TagInfo]) -> Vec<BaseCandidate> {
    let from_branches = branches.iter().map(|branch| BaseCandidate {
        key: base_key(BaseKind::Branch, &branch.name),
        display: base_display(BaseKind::Branch, &branch.name, None),
        revision: branch.name.clone(),
    });

    let from_tags = tags.iter().map(|tag| BaseCandidate {
        key: base_key(BaseKind::Tag, &tag.name),
        display: base_display(BaseKind::Tag, &tag.name, tag.message.as_deref()),
        // annotated tag は参照が指すタグオブジェクトの ID。git が対象コミットまで peel する
        revision: tag.id.clone(),
    });

    from_branches.chain(from_tags).collect()
}

/// 作成元候補の finder キーを組み立てる。
fn base_key(kind: BaseKind, name: &str) -> String {
    format!("{kind} {name}", kind = kind.label())
}

/// 作成元候補の表示行を組み立てる。この文字列がそのまま絞り込みの対象になる。
fn base_display(kind: BaseKind, name: &str, message: Option<&str>) -> String {
    let mut line = format!(
        "{kind:<width$}  {name}",
        kind = kind.label(),
        width = KIND_WIDTH
    );
    // annotated tag のメッセージは、どのリリースなのかを名前だけで思い出せない場合の手掛かりになる
    if let Some(message) = message {
        line.push_str("  ");
        line.push_str(message);
    }
    line
}

/// 作成元候補を finder のアイテムへ変換する。
fn to_base_item(language: Language, candidate: &BaseCandidate) -> FinderItem {
    FinderItem::new(
        candidate.display.clone(),
        candidate.key.clone(),
        PreviewSource::Git(log_preview_args(&candidate.revision)),
        language.messages(),
    )
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
fn log_preview_args(revision: &str) -> Vec<String> {
    // 末尾の `--` により、リビジョンがパスとして解釈されることを防ぐ
    [
        "log",
        "--color=always",
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        revision,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// `git branch -- <name> <base>` の引数を組み立てる。
///
/// ブランチ名はユーザー入力のため `--` の後ろへ置き、オプションとして解釈される
/// 余地を排除する（`gz reflog --restore` と同方針）。
fn create_args(name: &str, base: &str) -> Vec<String> {
    ["branch", "--", name, base]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git switch -- <name>` の引数を組み立てる。
fn switch_args(name: &str) -> Vec<String> {
    ["switch", "--", name]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// 作成結果と、切り替えるための次の操作を標準エラーへ知らせる。
///
/// `git branch` は成功時に何も出力しない。標準出力はパイプ利用のために空けておく。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_created(
    writer: &mut impl std::io::Write,
    messages: &dyn Messages,
    name: &str,
    base: &BaseCandidate,
    switch: SwitchAfterCreate,
) -> Result<()> {
    writeln!(
        writer,
        "{}",
        messages.branch_manage().created(name, &base.key)
    )
    .context(messages.common().stderr_write_failed())?;

    // 切り替える場合は続けて `git switch` を実行するため、案内は重複させない
    if switch == SwitchAfterCreate::Stay {
        writeln!(writer, "{}", messages.branch_manage().switch_hint(name))
            .context(messages.common().stderr_write_failed())?;
    }

    Ok(())
}

/// ブランチを複数選び、確認のうえ削除する。
///
/// # Errors
///
/// 候補の取得、選択（中断を含む）、`git branch` の実行に失敗した場合にエラーを返す。
/// merged でないブランチが `--force` なしで選ばれた場合は、実行前に停止する。
/// 確認で承認が得られなかった場合は [`crate::error::Error::Cancelled`]。
fn delete(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    mode: DeleteMode,
    into: Option<&str>,
) -> Result<()> {
    let candidates = delete_candidates(messages, repository, into)?;
    if candidates.is_empty() {
        bail!(messages.branch_manage().no_delete_candidates());
    }

    let items = to_delete_items(language, &display_lines(messages, &references(&candidates)));
    let options = FinderOptions::new(SelectionMode::Multi)
        .with_header(messages.branch_manage().delete_header().to_owned());
    let selected = select_many_with(items, &options)?;
    let selected = in_candidate_order(messages, &candidates, &selected)?;

    let unmerged = unmerged_among(&selected);

    // 1 件でも merged でないものが含まれていれば、他のブランチも削除せずに停止する。
    // 一部だけ削除してから止まると、どこまで進んだのかをユーザーが追う必要が出るため
    if mode == DeleteMode::MergedOnly && !unmerged.is_empty() {
        bail!(unmerged_rejection(messages, &unmerged));
    }

    execute_delete(language, messages, mode, &selected, &unmerged)
}

/// merged なブランチを全件選択済みで提示し、確認のうえ一括削除する。
///
/// # Errors
///
/// [`delete`] と同じ。候補は merged のみに絞られるため、実行するのは常に `git branch -d`。
fn cleanup(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    into: Option<&str>,
) -> Result<()> {
    let candidates: Vec<DeleteCandidate> = delete_candidates(messages, repository, into)?
        .into_iter()
        .filter(|candidate| candidate.is_merged)
        .collect();
    if candidates.is_empty() {
        bail!(messages.branch_manage().no_cleanup_candidates());
    }

    // 候補行と事前選択は同じ組み立て結果から作る。事前選択は表示文字列の完全一致で
    // 判定される（`crate::finder::FinderOptions`）一方、列の幅は候補一覧全体で決まるため
    let lines = display_lines(messages, &references(&candidates));
    let options = FinderOptions::new(SelectionMode::Multi)
        .with_header(messages.branch_manage().cleanup_header().to_owned())
        .with_preselect(preselect(&lines));

    let selected = select_many_with(to_delete_items(language, &lines), &options)?;
    let selected = in_candidate_order(messages, &candidates, &selected)?;

    execute_delete(language, messages, DeleteMode::MergedOnly, &selected, &[])
}

/// 確認プロンプトを経て `git branch -d|-D -- <names>...` を実行する。
///
/// # Errors
///
/// 承認が得られなかった場合は [`crate::error::Error::Cancelled`]、
/// `git branch` の実行に失敗した場合はそのエラーを返す。
fn execute_delete(
    language: Language,
    messages: &dyn Messages,
    mode: DeleteMode,
    selected: &[&DeleteCandidate],
    unmerged: &[&DeleteCandidate],
) -> Result<()> {
    // 削除は取り消せないため、対象を全件列挙したうえで明示的な同意を求める。
    // 一覧と同じ行を見せることで、merged / 最終更新日時ごと確認できる
    let lines: Vec<String> = display_lines(messages, selected)
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    let targets: Vec<&str> = lines.iter().map(String::as_str).collect();
    confirm(messages, &confirm_header(messages, unmerged), &targets)?;

    let names: Vec<&str> = selected
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect();
    let arguments = delete_args(mode, &names);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).context(messages.branch_manage().deletion_failed())?;

    Ok(())
}

/// 削除候補を組み立てる。
///
/// 一括で取得できる情報だけを 1 回ずつ読む（`git branch --merged` / `git for-each-ref` /
/// `git worktree list` の 3 回と、gix によるプロセス内の upstream 参照）。候補ごとに
/// git を起動すると、ブランチ数に比例して初期表示が遅くなるため。
///
/// # Errors
///
/// `--into` に指定された名前がブランチ一覧に無い場合、各情報の取得に失敗した場合に
/// エラーを返す。
fn delete_candidates(
    messages: &dyn Messages,
    repository: &gix::Repository,
    into: Option<&str>,
) -> Result<Vec<DeleteCandidate>> {
    let all = branches(repository, BranchScope::All)
        .context(messages.common().branch_list_read_failed())?;
    let base = merge_base_revision(messages, &all, into)?;

    let workdir = workdir(repository)?;
    let merged = merged_branches(workdir, &base)
        .context(messages.branch_manage().merged_state_read_failed())?;
    let activity =
        branch_activity(workdir).context(messages.branch_manage().activity_read_failed())?;
    let worktrees = worktrees(workdir).context(messages.common().worktree_list_read_failed())?;
    let in_use = checked_out_branches(&worktrees);

    let locals: Vec<BranchInfo> = all.into_iter().filter(|branch| !branch.is_remote).collect();
    let tracking = tracking_names(messages, repository, &locals)?;

    Ok(build_candidates(
        &locals, &merged, &activity, &in_use, &tracking,
    ))
}

/// ローカルブランチごとの upstream（リモート追跡ブランチ）の表示名を集める。
///
/// gix の設定読み取りだけで済むプロセス内の処理であり、git の起動を伴わない。
///
/// # Errors
///
/// upstream の読み取りに失敗した場合にエラーを返す。
fn tracking_names(
    messages: &dyn Messages,
    repository: &gix::Repository,
    locals: &[BranchInfo],
) -> Result<HashMap<String, String>> {
    let mut tracking = HashMap::new();
    for branch in locals {
        let upstream = upstream(repository, &branch.name)
            .with_context(|| messages.common().upstream_read_failed(&branch.name))?;

        if let Some(upstream) = upstream {
            tracking.insert(
                branch.name.clone(),
                tracking_name(&upstream.remote, &upstream.merge_ref),
            );
        }
    }

    Ok(tracking)
}

/// upstream の設定から表示用のリモート追跡ブランチ名を組み立てる。
///
/// `branch.<name>.merge` は通常リモート側の完全な参照名（`refs/heads/main`）だが、
/// `refs/heads/` 配下でないこともある。その場合は短縮名を推測せず、設定値をそのまま示す。
fn tracking_name(remote: &str, merge_ref: &str) -> String {
    match merge_ref.strip_prefix(BRANCH_REF_PREFIX) {
        Some(branch) => format!("{remote}/{branch}"),
        None => format!("{remote} {merge_ref}"),
    }
}

/// 削除候補を組み立てる純関数。
///
/// 現在のブランチと worktree でチェックアウト中のブランチは候補から除外する。
/// git 自身がこれらの削除を拒否するため、選べてしまうと必ず失敗する選択肢になる。
fn build_candidates(
    locals: &[BranchInfo],
    merged: &HashSet<String>,
    activity: &HashMap<String, String>,
    in_use: &HashSet<String>,
    tracking: &HashMap<String, String>,
) -> Vec<DeleteCandidate> {
    locals
        .iter()
        .filter(|branch| !branch.is_current && !in_use.contains(&branch.name))
        .map(|branch| DeleteCandidate {
            is_merged: merged.contains(&branch.name),
            relative_date: activity.get(&branch.name).cloned(),
            tracking: tracking.get(&branch.name).cloned(),
            name: branch.name.clone(),
        })
        .collect()
}

/// 一覧に表示する 1 行を列へ分解する。連結した文字列がそのまま絞り込みの対象になる。
///
/// 削除の判断に必要な情報（取り込み済みか・いつ更新されたか・リモート側に残るか）を
/// 一覧の時点で並べる。いずれも全件を一括取得できる情報に限る
/// （最終コミットの詳細はプレビューに委ねる。requirements.md FR-20）。
///
/// ブランチ名・更新日時の長さは候補ごとにまちまちであるため、列として返して
/// [`aligned_candidates`] に幅を揃えさせる（右の列の開始位置が候補ごとにずれないように）。
fn cells(messages: &dyn Messages, candidate: &DeleteCandidate) -> Vec<String> {
    let state = if candidate.is_merged {
        MERGED_LABEL
    } else {
        UNMERGED_LABEL
    };
    let date = candidate
        .relative_date
        .as_deref()
        .unwrap_or_else(|| messages.branch_manage().unknown_date());
    let tracking = match &candidate.tracking {
        Some(tracking) => messages.branch_manage().tracking(tracking),
        None => messages.branch_manage().no_tracking().to_owned(),
    };

    vec![
        candidate.name.clone(),
        state.to_owned(),
        date.to_owned(),
        tracking,
    ]
}

/// 候補一覧を参照の並びへ移し替える。
///
/// 表示行の組み立ては、finder へ渡す全候補と確認プロンプトに載せる選択済みの候補
/// （既に `&DeleteCandidate` の並び）の双方から呼ばれるため、入口を 1 つに保つ。
fn references(candidates: &[DeleteCandidate]) -> Vec<&DeleteCandidate> {
    candidates.iter().collect()
}

/// 候補と、列の幅を候補一覧全体で揃えた表示行の対を組み立てる。
///
/// 候補一覧・事前選択・確認プロンプトのいずれもこの 1 つの入口から作り、同じ候補集合には
/// 常に同じ行を示す。
fn display_lines<'a>(
    messages: &dyn Messages,
    candidates: &[&'a DeleteCandidate],
) -> Vec<(&'a DeleteCandidate, String)> {
    aligned_candidates(candidates, |candidate| cells(messages, candidate))
        .into_iter()
        .map(|(candidate, line)| (*candidate, line))
        .collect()
}

/// 起動時に選択済みにする候補の表示文字列を集める（`cleanup` は候補全件が対象）。
///
/// 受け取るのは [`display_lines`] が組み立てた候補と表示行の対であり、
/// finder へ渡す候補行と同じ文字列がそのまま事前選択になる。
fn preselect(candidates: &[(&DeleteCandidate, String)]) -> Vec<String> {
    candidates.iter().map(|(_, line)| line.clone()).collect()
}

/// 削除候補を finder のアイテムへ変換する。
fn to_delete_items(
    language: Language,
    candidates: &[(&DeleteCandidate, String)],
) -> Vec<FinderItem> {
    candidates
        .iter()
        .map(|(candidate, line)| {
            FinderItem::new(
                line.clone(),
                candidate.name.clone(),
                PreviewSource::Git(log_preview_args(&candidate.name)),
                language.messages(),
            )
        })
        .collect()
}

/// `--into` の指定を merged 判定の基準リビジョンへ解決する。
///
/// 指定名は gix が列挙したブランチ一覧と照合してから使う。`git branch --merged=<base>` の
/// 値はパスではなく `--` で保護できないため、値の由来で担保する（design.md セキュリティ設計）。
///
/// # Errors
///
/// 指定された名前がブランチ一覧に無い場合にエラーを返す。
fn merge_base_revision(
    messages: &dyn Messages,
    branches: &[BranchInfo],
    into: Option<&str>,
) -> Result<String> {
    let Some(into) = into else {
        return Ok(DEFAULT_MERGE_BASE.to_owned());
    };

    branches
        .iter()
        .find(|branch| branch.name == into)
        .map(|branch| branch.name.clone())
        .ok_or_else(|| anyhow!(messages.branch_manage().unknown_merge_base(into)))
}

/// 選択されたブランチ名を候補一覧の順序へ並べ直す。
///
/// finder の選択結果はユーザーが選んだ順で返るため、そのまま使うと確認プロンプトの
/// 列挙順と `git branch` へ渡す順が実行ごとに変わってしまう。
///
/// # Errors
///
/// 選択された名前が候補一覧に含まれない場合にエラーを返す（対象を取り違えたまま
/// 削除しないよう、暗黙に読み飛ばさない）。
fn in_candidate_order<'a>(
    messages: &dyn Messages,
    candidates: &'a [DeleteCandidate],
    selected: &[String],
) -> Result<Vec<&'a DeleteCandidate>> {
    let missing: Vec<&str> = selected
        .iter()
        .filter(|name| !candidates.iter().any(|candidate| &candidate.name == *name))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            messages
                .branch_manage()
                .selection_not_found(&missing.join(", "))
        );
    }

    Ok(candidates
        .iter()
        .filter(|candidate| selected.contains(&candidate.name))
        .collect())
}

/// 選択のうち merged でないものを候補順のまま抽出する。
fn unmerged_among<'a>(selected: &[&'a DeleteCandidate]) -> Vec<&'a DeleteCandidate> {
    selected
        .iter()
        .filter(|candidate| !candidate.is_merged)
        .copied()
        .collect()
}

/// merged でないブランチが `--force` なしで選ばれた場合のエラーメッセージを組み立てる。
fn unmerged_rejection(messages: &dyn Messages, unmerged: &[&DeleteCandidate]) -> String {
    messages
        .branch_manage()
        .unmerged_rejection(&names_of(unmerged))
}

/// 確認プロンプトに示す説明を組み立てる。
///
/// merged でないブランチが含まれる場合のみ、失われるものを名指しで警告する
/// （`--force` を付けていても、実際に含まれていなければ根拠の無い警告は出さない）。
fn confirm_header(messages: &dyn Messages, unmerged: &[&DeleteCandidate]) -> String {
    if unmerged.is_empty() {
        return messages.branch_manage().delete_confirmation().to_owned();
    }

    messages
        .branch_manage()
        .unmerged_confirmation(&names_of(unmerged))
}

/// ブランチ名を読点区切りで並べる。
fn names_of(candidates: &[&DeleteCandidate]) -> String {
    candidates
        .iter()
        .map(|candidate| candidate.name.as_str())
        .collect::<Vec<&str>>()
        .join(", ")
}

/// `git branch -d|-D -- <names>...` の引数を組み立てる。
///
/// ブランチ名は gix が列挙した候補に由来する値だが、`-` で始まる名前が
/// オプションとして解釈される余地を残さないよう `--` の後ろへ置く。
fn delete_args(mode: DeleteMode, names: &[&str]) -> Vec<String> {
    ["branch", mode.option(), "--"]
        .into_iter()
        .chain(names.iter().copied())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use skim::prelude::SkimItem as _;

    use super::*;
    use crate::commands::COLUMN_SEPARATOR;

    const TAG_OBJECT_ID: &str = "1f0c9a4b3d2e5f60718293a4b5c6d7e8f9012345";

    fn local(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    fn current(name: &str) -> BranchInfo {
        BranchInfo {
            is_current: true,
            ..local(name)
        }
    }

    fn remote(name: &str) -> BranchInfo {
        BranchInfo {
            is_remote: true,
            ..local(name)
        }
    }

    fn tag(name: &str, message: Option<&str>) -> TagInfo {
        TagInfo {
            name: name.to_owned(),
            id: TAG_OBJECT_ID.to_owned(),
            message: message.map(str::to_owned),
        }
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// 候補 1 件だけの表示行（揃える相手が居ないため列を連結しただけの行）。
    fn display_line(messages: &dyn Messages, candidate: &DeleteCandidate) -> String {
        cells(messages, candidate).join(COLUMN_SEPARATOR)
    }

    fn candidate_names(candidates: &[DeleteCandidate]) -> Vec<&str> {
        candidates
            .iter()
            .map(|candidate| candidate.name.as_str())
            .collect()
    }

    fn candidate(name: &str, is_merged: bool) -> DeleteCandidate {
        DeleteCandidate {
            name: name.to_owned(),
            is_merged,
            relative_date: Some("3 days ago".to_owned()),
            tracking: None,
        }
    }

    // --- create -------------------------------------------------------------

    #[test]
    fn the_base_candidates_list_branches_before_tags() {
        let candidates = base_candidates(
            &[local("main"), remote("origin/main")],
            &[tag("v1.0", Some("リリース 1.0"))],
        );

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.key.as_str())
                .collect::<Vec<_>>(),
            ["branch main", "branch origin/main", "tag v1.0"]
        );
    }

    #[test]
    fn a_branch_and_a_tag_of_the_same_name_stay_distinguishable() {
        // 同名のブランチとタグは共存できるため、名前だけをキーにすると取り違える
        let candidates = base_candidates(&[local("v1.0")], &[tag("v1.0", None)]);

        assert_eq!(candidates[0].key, "branch v1.0");
        assert_eq!(candidates[1].key, "tag v1.0");
        assert_ne!(candidates[0].key, candidates[1].key);
    }

    #[test]
    fn a_branch_is_used_as_a_base_by_its_own_name() {
        let candidates = base_candidates(&[local("feature/login")], &[]);

        assert_eq!(candidates[0].revision, "feature/login");
    }

    #[test]
    fn a_tag_is_used_as_a_base_by_its_resolved_id() {
        // annotated tag は名前ではなく解決済みの ID を渡す（git がコミットまで peel する）
        let candidates = base_candidates(&[], &[tag("v1.0", Some("リリース 1.0"))]);

        assert_eq!(candidates[0].revision, TAG_OBJECT_ID);
    }

    #[test]
    fn a_base_line_names_its_kind_before_the_name() {
        assert_eq!(base_display(BaseKind::Branch, "main", None), "branch  main");
        assert_eq!(base_display(BaseKind::Tag, "v2.0", None), "tag     v2.0");
    }

    #[test]
    fn an_annotated_tag_shows_its_message_after_the_name() {
        assert_eq!(
            base_display(BaseKind::Tag, "v1.0", Some("リリース 1.0")),
            "tag     v1.0  リリース 1.0"
        );
    }

    #[test]
    fn a_base_item_keeps_the_prefixed_key() {
        let candidates = base_candidates(&[local("main")], &[]);

        assert_eq!(
            to_base_item(Language::Japanese, &candidates[0]).key(),
            "branch main"
        );
    }

    #[test]
    fn the_new_branch_name_is_placed_after_the_separator() {
        assert_eq!(
            create_args("--not-an-option", "main"),
            ["branch", "--", "--not-an-option", "main"]
        );
    }

    #[test]
    fn a_branch_is_created_from_the_selected_base() {
        assert_eq!(
            create_args("feature", TAG_OBJECT_ID),
            ["branch", "--", "feature", TAG_OBJECT_ID]
        );
    }

    #[test]
    fn switching_places_the_branch_name_after_the_separator() {
        assert_eq!(switch_args("feature"), ["switch", "--", "feature"]);
    }

    #[test]
    fn creating_without_switching_explains_how_to_switch() {
        let base = &base_candidates(&[local("main")], &[])[0];
        let mut output = Vec::new();

        report_created(
            &mut output,
            Language::Japanese.messages(),
            "feature",
            base,
            SwitchAfterCreate::Stay,
        )
        .expect("writing to a buffer should succeed");

        let text = String::from_utf8(output).expect("the message should be utf-8");
        assert!(text.contains("ブランチ `feature`"), "unexpected: {text}");
        assert!(text.contains("branch main"), "unexpected: {text}");
        assert!(
            text.contains("git switch feature"),
            "the way to switch should be suggested: {text}"
        );
    }

    #[test]
    fn creating_with_switching_does_not_repeat_the_suggestion() {
        let base = &base_candidates(&[local("main")], &[])[0];
        let mut output = Vec::new();

        report_created(
            &mut output,
            Language::Japanese.messages(),
            "feature",
            base,
            SwitchAfterCreate::Switch,
        )
        .expect("writing to a buffer should succeed");

        let text = String::from_utf8(output).expect("the message should be utf-8");
        assert!(text.contains("ブランチ `feature`"), "unexpected: {text}");
        assert!(
            !text.contains("git switch feature"),
            "the switch happens right away: {text}"
        );
    }

    // --- delete / cleanup の候補生成 -----------------------------------------

    #[test]
    fn the_current_branch_is_never_offered_for_deletion() {
        let locals = [current("main"), local("feature")];

        let candidates = build_candidates(
            &locals,
            &set(&["feature"]),
            &map(&[("main", "1 hour ago"), ("feature", "3 days ago")]),
            &HashSet::new(),
            &HashMap::new(),
        );

        assert_eq!(candidate_names(&candidates), ["feature"]);
    }

    #[test]
    fn a_branch_checked_out_by_a_worktree_is_never_offered_for_deletion() {
        let locals = [current("main"), local("feature"), local("hotfix")];

        let candidates = build_candidates(
            &locals,
            &set(&["feature", "hotfix"]),
            &HashMap::new(),
            &set(&["main", "hotfix"]),
            &HashMap::new(),
        );

        assert_eq!(candidate_names(&candidates), ["feature"]);
    }

    #[test]
    fn several_worktrees_exclude_all_of_their_branches() {
        let locals = [
            current("main"),
            local("a"),
            local("b"),
            local("c"),
            local("d"),
        ];

        let candidates = build_candidates(
            &locals,
            &HashSet::new(),
            &HashMap::new(),
            &set(&["main", "a", "c"]),
            &HashMap::new(),
        );

        assert_eq!(candidate_names(&candidates), ["b", "d"]);
    }

    #[test]
    fn merged_and_unmerged_branches_are_listed_side_by_side() {
        let locals = [local("done"), local("wip")];

        let candidates = build_candidates(
            &locals,
            &set(&["done"]),
            &map(&[("done", "3 days ago"), ("wip", "2 hours ago")]),
            &HashSet::new(),
            &map(&[("done", "origin/done")]),
        );

        assert_eq!(
            candidates,
            [
                DeleteCandidate {
                    name: "done".to_owned(),
                    is_merged: true,
                    relative_date: Some("3 days ago".to_owned()),
                    tracking: Some("origin/done".to_owned()),
                },
                DeleteCandidate {
                    name: "wip".to_owned(),
                    is_merged: false,
                    relative_date: Some("2 hours ago".to_owned()),
                    tracking: None,
                },
            ]
        );
    }

    #[test]
    fn the_candidates_keep_the_order_of_the_branch_listing() {
        let locals = [local("a"), local("b"), local("c")];

        let candidates = build_candidates(
            &locals,
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );

        assert_eq!(candidate_names(&candidates), ["a", "b", "c"]);
    }

    #[test]
    fn a_line_shows_the_merge_state_the_date_and_the_tracking_branch() {
        let candidates = build_candidates(
            &[local("done")],
            &set(&["done"]),
            &map(&[("done", "3 days ago")]),
            &HashSet::new(),
            &map(&[("done", "origin/done")]),
        );

        assert_eq!(
            display_line(Language::Japanese.messages(), &candidates[0]),
            "done  merged  3 days ago  追跡: origin/done"
        );
    }

    #[test]
    fn a_branch_without_a_tracking_branch_says_so() {
        let candidates = build_candidates(
            &[local("wip")],
            &HashSet::new(),
            &map(&[("wip", "2 hours ago")]),
            &HashSet::new(),
            &HashMap::new(),
        );

        assert_eq!(
            display_line(Language::Japanese.messages(), &candidates[0]),
            "wip  unmerged  2 hours ago  追跡なし"
        );
    }

    #[test]
    fn a_missing_date_is_shown_as_unknown_instead_of_being_invented() {
        let candidates = build_candidates(
            &[local("odd")],
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );

        let branch_manage = Language::Japanese.messages().branch_manage();
        assert_eq!(
            display_line(Language::Japanese.messages(), &candidates[0]),
            format!(
                "odd  unmerged  {date}  {tracking}",
                date = branch_manage.unknown_date(),
                tracking = branch_manage.no_tracking()
            )
        );
    }

    #[test]
    fn a_relative_date_is_kept_verbatim_as_git_reported_it() {
        for date in ["3 days ago", "2 hours ago", "1 year, 2 months ago", "now"] {
            let candidates = build_candidates(
                &[local("branch")],
                &HashSet::new(),
                &map(&[("branch", date)]),
                &HashSet::new(),
                &HashMap::new(),
            );

            assert!(
                display_line(Language::Japanese.messages(), &candidates[0]).contains(date),
                "the relative date should be shown as is: {date}"
            );
        }
    }

    #[test]
    fn a_delete_item_keeps_the_branch_name_as_its_key() {
        let candidates = build_candidates(
            &[local("feature/login")],
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashMap::new(),
        );

        let messages = Language::Japanese.messages();
        let items = to_delete_items(
            Language::Japanese,
            &display_lines(messages, &references(&candidates)),
        );

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            ["feature/login"]
        );
    }

    #[test]
    fn a_mixed_repository_lists_only_what_can_be_deleted() {
        // 現在ブランチ・worktree 使用中の除外、merged/unmerged、追跡有無、日時を一度に確かめる
        let locals = [
            current("main"),
            local("done"),
            local("in-worktree"),
            local("wip"),
        ];

        let candidates = build_candidates(
            &locals,
            &set(&["done", "in-worktree"]),
            &map(&[
                ("main", "1 hour ago"),
                ("done", "3 days ago"),
                ("in-worktree", "5 days ago"),
                ("wip", "10 minutes ago"),
            ]),
            &set(&["main", "in-worktree"]),
            &map(&[("done", "origin/done"), ("main", "origin/main")]),
        );

        assert_eq!(
            display_lines(Language::Japanese.messages(), &references(&candidates))
                .into_iter()
                .map(|(_, line)| line)
                .collect::<Vec<_>>(),
            [
                "done  merged    3 days ago      追跡: origin/done",
                "wip   unmerged  10 minutes ago  追跡なし",
            ]
        );
    }

    #[test]
    fn every_preselected_line_is_the_very_line_shown_in_the_list() {
        // 事前選択は表示文字列の完全一致で判定されるため、列を揃えたあとの行と
        // 一致していなければ機能しない（`crate::finder::FinderOptions::preselect`）
        let messages = Language::Japanese.messages();
        let candidates = build_candidates(
            &[local("done"), local("feature/very-long-name")],
            &set(&["done", "feature/very-long-name"]),
            &map(&[
                ("done", "3 days ago"),
                ("feature/very-long-name", "10 minutes ago"),
            ]),
            &HashSet::new(),
            &map(&[("done", "origin/done")]),
        );
        let lines = display_lines(messages, &references(&candidates));

        let displayed: Vec<String> = to_delete_items(Language::Japanese, &lines)
            .iter()
            .map(|item| item.text().into_owned())
            .collect();

        assert_eq!(preselect(&lines), displayed);
        // 列を揃える前の行では一致しないこと（＝別々に組み立てると壊れること）も確かめる
        assert!(
            !preselect(&lines).contains(&display_line(messages, &candidates[0])),
            "the padding is what makes the two ways differ: {displayed:?}"
        );
    }

    #[test]
    fn an_upstream_under_refs_heads_is_shown_as_a_tracking_branch() {
        assert_eq!(
            tracking_name("origin", "refs/heads/feature/login"),
            "origin/feature/login"
        );
    }

    #[test]
    fn an_upstream_outside_of_refs_heads_is_shown_as_configured() {
        // 短縮名を推測すると、実在しない追跡ブランチを表示してしまう
        assert_eq!(
            tracking_name("origin", "refs/tags/v1.0"),
            "origin refs/tags/v1.0"
        );
    }

    // --- `--into` の解決 -----------------------------------------------------

    #[test]
    fn the_merge_base_defaults_to_head() {
        let branches = [current("main"), local("feature")];

        assert_eq!(
            merge_base_revision(Language::Japanese.messages(), &branches, None)
                .expect("no base is always valid"),
            DEFAULT_MERGE_BASE
        );
    }

    #[test]
    fn a_known_branch_is_accepted_as_the_merge_base() {
        let branches = [current("main"), local("develop")];

        assert_eq!(
            merge_base_revision(Language::Japanese.messages(), &branches, Some("develop"))
                .expect("a listed branch resolves"),
            "develop"
        );
    }

    #[test]
    fn a_remote_tracking_branch_is_accepted_as_the_merge_base() {
        let branches = [current("main"), remote("origin/main")];

        assert_eq!(
            merge_base_revision(
                Language::Japanese.messages(),
                &branches,
                Some("origin/main")
            )
            .expect("a listed branch resolves"),
            "origin/main"
        );
    }

    #[test]
    fn an_unknown_merge_base_is_rejected_instead_of_being_passed_to_git() {
        let branches = [current("main")];

        let err = merge_base_revision(Language::Japanese.messages(), &branches, Some("nope"))
            .expect_err("an unknown branch must be rejected");

        assert!(
            err.to_string().contains("nope") && err.to_string().contains("--into"),
            "the option and the name should be named: {err:#}"
        );
    }

    #[test]
    fn an_option_like_merge_base_is_rejected_by_the_same_check() {
        // `git branch --merged=<base>` の値は `--` で保護できないため、由来で担保する
        let branches = [current("main")];

        assert!(
            merge_base_revision(Language::Japanese.messages(), &branches, Some("--all")).is_err(),
            "a value that was never listed must not reach git"
        );
    }

    // --- 選択結果の解決 ------------------------------------------------------

    #[test]
    fn the_selection_is_restored_to_the_order_of_the_candidates() {
        // skim の選択結果は候補順とは限らない
        let candidates = [
            candidate("a", true),
            candidate("b", true),
            candidate("c", true),
        ];
        let selected = ["c".to_owned(), "a".to_owned()];

        let ordered = in_candidate_order(Language::Japanese.messages(), &candidates, &selected)
            .expect("all names are listed");

        assert_eq!(
            ordered
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
    }

    #[test]
    fn a_name_outside_of_the_candidates_is_rejected() {
        let candidates = [candidate("a", true)];

        let err = in_candidate_order(
            Language::Japanese.messages(),
            &candidates,
            &["b".to_owned()],
        )
        .expect_err("an unknown branch must be rejected");

        assert!(
            err.to_string().contains('b'),
            "the unknown branch should be named: {err:#}"
        );
    }

    #[test]
    fn only_the_unmerged_branches_of_a_selection_are_extracted() {
        let candidates = [
            candidate("done", true),
            candidate("wip", false),
            candidate("old", false),
        ];
        let selected: Vec<&DeleteCandidate> = candidates.iter().collect();

        assert_eq!(
            unmerged_among(&selected)
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>(),
            ["wip", "old"]
        );
    }

    // --- 実行と確認 ----------------------------------------------------------

    #[test]
    fn the_safe_option_deletes_merged_branches_only() {
        assert_eq!(DeleteMode::MergedOnly.option(), SAFE_DELETE_OPTION);
        assert_eq!(DeleteMode::Force.option(), FORCE_DELETE_OPTION);
    }

    #[test]
    fn deleting_places_every_name_after_the_separator() {
        assert_eq!(
            delete_args(DeleteMode::MergedOnly, &["feature", "hotfix"]),
            ["branch", "-d", "--", "feature", "hotfix"]
        );
    }

    #[test]
    fn forcing_switches_the_option_without_moving_the_separator() {
        assert_eq!(
            delete_args(DeleteMode::Force, &["wip"]),
            ["branch", "-D", "--", "wip"]
        );
    }

    #[test]
    fn a_branch_name_that_looks_like_an_option_stays_an_operand() {
        let arguments = delete_args(DeleteMode::MergedOnly, &["-weird"]);

        assert_eq!(arguments, ["branch", "-d", "--", "-weird"]);
        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .expect("the separator should be present");
        assert!(
            separator < arguments.len() - 1,
            "names must follow the separator: {arguments:?}"
        );
    }

    #[test]
    fn deleting_merged_branches_only_warns_that_it_cannot_be_undone() {
        let header = confirm_header(Language::Japanese.messages(), &[]);

        assert!(header.contains("元に戻せません"), "unexpected: {header}");
        assert!(
            !header.contains(UNMERGED_LABEL),
            "there is nothing unmerged to warn about: {header}"
        );
    }

    #[test]
    fn the_confirmation_names_every_unmerged_branch_it_is_about_to_lose() {
        let candidates = [candidate("wip", false), candidate("old", false)];
        let unmerged: Vec<&DeleteCandidate> = candidates.iter().collect();

        let header = confirm_header(Language::Japanese.messages(), &unmerged);

        assert!(header.contains("警告"), "unexpected: {header}");
        assert!(header.contains("wip, old"), "unexpected: {header}");
        assert!(header.contains("失われます"), "unexpected: {header}");
    }

    #[test]
    fn an_unmerged_selection_is_rejected_with_the_option_that_would_allow_it() {
        let candidates = [candidate("wip", false)];
        let unmerged: Vec<&DeleteCandidate> = candidates.iter().collect();

        let message = unmerged_rejection(Language::Japanese.messages(), &unmerged);

        assert!(message.contains("wip"), "unexpected: {message}");
        assert!(
            message.contains("gz branch delete --force"),
            "the way forward should be named: {message}"
        );
    }

    // --- 文言 ----------------------------------------------------------------

    /// 引数を取らない文言。
    fn plain_texts(language: Language) -> Vec<&'static str> {
        let branch_manage = language.messages().branch_manage();

        vec![
            branch_manage.no_tracking(),
            branch_manage.unknown_date(),
            branch_manage.merged_state_read_failed(),
            branch_manage.activity_read_failed(),
            branch_manage.deletion_failed(),
            branch_manage.delete_header(),
            branch_manage.no_delete_candidates(),
            branch_manage.delete_confirmation(),
            branch_manage.cleanup_header(),
            branch_manage.no_cleanup_candidates(),
        ]
    }

    /// 引数を取る文言と、そこへ展開されるべき引数。
    fn texts_with_arguments(language: Language) -> Vec<(String, &'static str)> {
        let branch_manage = language.messages().branch_manage();

        vec![
            (
                branch_manage.base_selection_not_found("tag v1.0"),
                "tag v1.0",
            ),
            (branch_manage.creation_failed("feature"), "feature"),
            (branch_manage.created("feature", "branch main"), "feature"),
            (
                branch_manage.created("feature", "branch main"),
                "branch main",
            ),
            (branch_manage.switch_hint("feature"), "git switch feature"),
            (branch_manage.tracking("origin/done"), "origin/done"),
            (branch_manage.unknown_merge_base("nope"), "nope"),
            (branch_manage.unknown_merge_base("nope"), "--into"),
            (branch_manage.selection_not_found("wip, old"), "wip, old"),
            (branch_manage.unmerged_rejection("wip"), "wip"),
            (
                branch_manage.unmerged_rejection("wip"),
                "gz branch delete --force",
            ),
            (branch_manage.unmerged_confirmation("wip, old"), "wip, old"),
        ]
    }

    #[test]
    fn every_branch_manage_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            for text in plain_texts(language) {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
            for (text, _) in texts_with_arguments(language) {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
        }
    }

    #[test]
    fn every_branch_manage_message_expands_its_arguments() {
        for language in [Language::Japanese, Language::English] {
            for (text, argument) in texts_with_arguments(language) {
                assert!(
                    text.contains(argument),
                    "{language:?} must mention `{argument}`: {text}"
                );
            }
        }
    }

    #[test]
    fn the_branch_manage_wording_is_translated() {
        for (japanese, english) in plain_texts(Language::Japanese)
            .into_iter()
            .zip(plain_texts(Language::English))
        {
            assert_ne!(japanese, english, "the wording must be translated");
        }

        for ((japanese, _), (english, _)) in texts_with_arguments(Language::Japanese)
            .into_iter()
            .zip(texts_with_arguments(Language::English))
        {
            assert_ne!(japanese, english, "the wording must be translated");
        }
    }

    #[test]
    fn a_line_keeps_the_git_merge_state_in_every_language() {
        // `merged` / `unmerged` は git の語彙であるため訳さない（design.md「翻訳しないもの」）
        let candidates = build_candidates(
            &[local("done"), local("wip")],
            &set(&["done"]),
            &map(&[("done", "3 days ago")]),
            &HashSet::new(),
            &map(&[("done", "origin/done")]),
        );

        for language in [Language::Japanese, Language::English] {
            let merged = display_line(language.messages(), &candidates[0]);
            let unmerged = display_line(language.messages(), &candidates[1]);

            assert!(
                merged.starts_with(&format!("done  {MERGED_LABEL}  3 days ago  ")),
                "{language:?}: {merged}"
            );
            assert!(
                unmerged.starts_with(&format!("wip  {UNMERGED_LABEL}  ")),
                "{language:?}: {unmerged}"
            );
        }
    }

    #[test]
    fn the_tracking_part_of_a_line_is_translated() {
        // 追跡の有無は fuzgit 自身の文言であり、git の出力ではないため訳す
        let candidates = build_candidates(
            &[local("done"), local("wip")],
            &set(&["done"]),
            &map(&[("done", "3 days ago")]),
            &HashSet::new(),
            &map(&[("done", "origin/done")]),
        );

        for candidate in &candidates {
            assert_ne!(
                display_line(Language::Japanese.messages(), candidate),
                display_line(Language::English.messages(), candidate),
                "the line must be translated: {candidate:?}"
            );
        }
    }

    #[test]
    fn the_english_creation_report_explains_how_to_switch_as_well() {
        let base = &base_candidates(&[local("main")], &[])[0];
        let mut output = Vec::new();

        report_created(
            &mut output,
            Language::English.messages(),
            "feature",
            base,
            SwitchAfterCreate::Stay,
        )
        .expect("writing to a buffer should succeed");

        let text = String::from_utf8(output).expect("the message should be utf-8");
        assert_eq!(text.lines().count(), 2, "unexpected: {text}");
        assert!(text.contains("branch main"), "unexpected: {text}");
        assert!(
            text.contains("git switch feature"),
            "the way to switch should be suggested: {text}"
        );
    }

    #[test]
    fn the_english_confirmation_keeps_the_warning_on_its_own_lines() {
        let candidates = [candidate("wip", false), candidate("old", false)];
        let unmerged: Vec<&DeleteCandidate> = candidates.iter().collect();

        let header = confirm_header(Language::English.messages(), &unmerged);

        assert_eq!(header.lines().count(), 3, "unexpected: {header}");
        assert!(header.contains("wip, old"), "unexpected: {header}");
        assert!(header.contains(UNMERGED_LABEL), "unexpected: {header}");
        assert!(
            header.starts_with(
                Language::English
                    .messages()
                    .branch_manage()
                    .delete_confirmation()
            ),
            "the warning is appended to the common explanation: {header}"
        );
    }
}
