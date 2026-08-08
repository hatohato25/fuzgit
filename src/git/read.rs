//! リポジトリ情報の読み取り。
//!
//! ブランチ・コミット・タグ・reflog・変更ファイルの一覧を、`gix` の型を
//! `commands` 層へ漏らさないプレーンな構造体として返す。
//!
//! 基本は `gix` で読むが、作業ツリーの状態（`git status`）とツリーのファイル一覧
//! （`git ls-tree`）は `git` コマンドのキャプチャで取得する。gix の status 実装は
//! 設定（`status.renames` 等）の解釈まで含めると挙動互換の担保が難しく、
//! ここでは git 本体の判定をそのまま利用するほうが確実なため。

use std::path::Path;

use gix::bstr::{BStr, ByteSlice as _};

use crate::error::{Error, Result};
use crate::git::exec::{capture_git_in, capture_git_with_status_in};
use crate::git::repo::workdir;

/// 候補に含めるブランチの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchScope {
    /// ローカルブランチ（`refs/heads/`）のみを対象にする。
    Local,
    /// ローカルブランチに加えてリモート追跡ブランチ（`refs/remotes/`）も対象にする。
    All,
}

/// ブランチ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// 短縮名。ローカルは `main`、リモート追跡は `origin/main` の形式。
    pub name: String,
    /// HEAD が指しているブランチかどうか。
    pub is_current: bool,
    /// リモート追跡ブランチかどうか。
    pub is_remote: bool,
}

/// 候補に含めるコミットの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitScope<'a> {
    /// HEAD から辿れるコミットのみを対象にする（`gz log`）。
    Head,
    /// すべてのブランチ（ローカル・リモート追跡）の先端から辿れるコミットを対象にする。
    AllBranches,
    /// 指定したブランチ（リビジョン）から辿れるコミットを対象にする。
    Branch(&'a str),
}

/// コミット 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// フルハッシュ。
    pub id: String,
    /// `core.abbrev` に従って短縮したハッシュ。
    pub short_id: String,
    /// コミットメッセージの 1 行目。
    pub summary: String,
    /// 作者名。
    pub author: String,
    /// 作者日時。コミットに記録されたタイムゾーンでの `YYYY-MM-DD` 表記。
    pub time: String,
}

/// `gix` 側のエラーを [`Error::RepositoryReadFailed`] へ変換する。
fn read_error(
    operation: &str,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> Error {
    Error::RepositoryReadFailed {
        operation: operation.to_owned(),
        source: source.into(),
    }
}

/// git の参照名・署名は任意のバイト列を取り得るため、UTF-8 でない場合は明示的にエラーとする。
fn to_utf8(bytes: &BStr, operation: &str) -> Result<String> {
    bytes
        .to_str()
        .map(str::to_owned)
        .map_err(|source| read_error(operation, source))
}

/// HEAD がまだ 1 件もコミットを持たないブランチ（unborn HEAD）を指している場合、その短縮名を返す。
fn unborn_branch(repository: &gix::Repository) -> Result<Option<String>> {
    let head = repository
        .head()
        .map_err(|source| read_error("HEAD の読み取り", source))?;

    match &head.kind {
        gix::head::Kind::Unborn(name) => {
            to_utf8(name.shorten(), "HEAD のブランチ名の解釈").map(Some)
        }
        gix::head::Kind::Symbolic(_) | gix::head::Kind::Detached { .. } => Ok(None),
    }
}

/// 候補が 0 件になった原因が unborn HEAD であれば [`Error::UnbornHead`] を返す。
///
/// `git init` 直後のリポジトリでは「候補がありません」だけでは原因が分からないため、
/// 単なる候補ゼロと区別して原因と次に取れる操作を伝える。
fn reject_unborn_head(repository: &gix::Repository) -> Result<()> {
    match unborn_branch(repository)? {
        Some(branch) => Err(Error::UnbornHead { branch }),
        None => Ok(()),
    }
}

/// HEAD が指しているローカルブランチの短縮名を返す。detached HEAD の場合は `None`。
///
/// まだコミットの無いブランチ（unborn HEAD）でも、HEAD が指す名前を返す。
///
/// # Errors
///
/// HEAD の読み取りに失敗した場合、またはブランチ名が UTF-8 でない場合は
/// [`Error::RepositoryReadFailed`] を返す。
pub fn current_branch(repository: &gix::Repository) -> Result<Option<String>> {
    let Some(name) = repository
        .head_name()
        .map_err(|source| read_error("HEAD の読み取り", source))?
    else {
        return Ok(None);
    };

    to_utf8(name.shorten(), "HEAD のブランチ名の解釈").map(Some)
}

/// ブランチ一覧を名前順で取得する。
///
/// [`BranchScope::All`] の場合はローカルブランチの後ろにリモート追跡ブランチを並べる。
///
/// # Errors
///
/// - 参照の列挙・読み取りに失敗した場合、または参照名が UTF-8 でない場合は
///   [`Error::RepositoryReadFailed`]
/// - ブランチが 1 件も無く、その原因が unborn HEAD である場合は [`Error::UnbornHead`]
pub fn branches(repository: &gix::Repository, scope: BranchScope) -> Result<Vec<BranchInfo>> {
    let current = current_branch(repository)?;
    let platform = repository
        .references()
        .map_err(|source| read_error("参照の列挙", source))?;

    let mut locals = Vec::new();
    for reference in platform
        .local_branches()
        .map_err(|source| read_error("ローカルブランチの列挙", source))?
    {
        let reference =
            reference.map_err(|source| read_error("ローカルブランチの読み取り", source))?;
        let name = to_utf8(reference.name().shorten(), "ローカルブランチ名の解釈")?;
        locals.push(BranchInfo {
            is_current: current.as_deref() == Some(name.as_str()),
            name,
            is_remote: false,
        });
    }
    locals.sort_by(|left, right| left.name.cmp(&right.name));

    if scope == BranchScope::Local {
        if locals.is_empty() {
            reject_unborn_head(repository)?;
        }
        return Ok(locals);
    }

    let mut remotes = Vec::new();
    for reference in platform
        .remote_branches()
        .map_err(|source| read_error("リモート追跡ブランチの列挙", source))?
    {
        let reference =
            reference.map_err(|source| read_error("リモート追跡ブランチの読み取り", source))?;

        // `origin/HEAD` はリモートの既定ブランチへのシンボリック参照であり、
        // 切り替え先としては実体のブランチと重複するため候補から外す
        if matches!(reference.target(), gix::refs::TargetRef::Symbolic(_)) {
            continue;
        }

        let name = to_utf8(reference.name().shorten(), "リモート追跡ブランチ名の解釈")?;
        remotes.push(BranchInfo {
            name,
            is_current: false,
            is_remote: true,
        });
    }
    remotes.sort_by(|left, right| left.name.cmp(&right.name));

    locals.extend(remotes);
    if locals.is_empty() {
        reject_unborn_head(repository)?;
    }
    Ok(locals)
}

/// 現在のブランチを除いたブランチ一覧（ローカル・リモート追跡）を取得する。
///
/// `gz merge` の merge 対象、`gz rebase` の base の候補に用いる。自分自身への merge /
/// rebase は「取り込むコミットが 1 件も無い」操作であり、選んでも意味を持たないため候補から外す。
/// detached HEAD では現在のブランチが無いため、すべてのブランチが候補になる。
///
/// # Errors
///
/// [`branches`] と同じ（unborn HEAD の場合は [`Error::UnbornHead`]）。
pub fn other_branches(repository: &gix::Repository) -> Result<Vec<BranchInfo>> {
    Ok(branches(repository, BranchScope::All)?
        .into_iter()
        .filter(|branch| !branch.is_current)
        .collect())
}

/// リモートの名前一覧を名前順で取得する。
///
/// `gz push` の候補（リモート × 現在ブランチ）を組み立てるために用いる。
///
/// # Errors
///
/// リモート名が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn remotes(repository: &gix::Repository) -> Result<Vec<String>> {
    // `remote_names()` は BTreeSet であり既に名前順に並んでいる
    repository
        .remote_names()
        .iter()
        .map(|name| to_utf8(name.as_bstr(), "リモート名の解釈"))
        .collect()
}

/// ブランチに設定された upstream（`branch.<name>.remote` / `.merge`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// `branch.<name>.remote` の値。
    ///
    /// 通常は `origin` のようなリモート名だが、git の設定では URL を直接書くこともできるため
    /// その場合は URL がそのまま入る（[`remotes`] の一覧とは一致しない）。
    pub remote: String,
    /// `branch.<name>.merge` の値（リモート側の完全な参照名。例: `refs/heads/main`）。
    pub merge_ref: String,
}

/// ブランチに設定された upstream を取得する。設定が無い場合は `None`。
///
/// # Errors
///
/// ブランチ名が参照名として不正な場合、upstream の参照名を解決できない場合、
/// または値が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn upstream(repository: &gix::Repository, branch: &str) -> Result<Option<Upstream>> {
    let full_name = gix::refs::Category::LocalBranch
        .to_full_name(branch)
        .map_err(|source| read_error(&format!("ブランチ名 `{branch}` の解釈"), source))?;

    // fetch 方向を見る。push 方向は pushRemote / push.default の解決を伴い、
    // 「現在の追跡先」を知りたいここでの用途とは意味が異なる
    let direction = gix::remote::Direction::Fetch;

    let Some(remote) = repository.branch_remote_name(branch, direction) else {
        return Ok(None);
    };
    let remote = match remote {
        gix::remote::Name::Symbol(name) => name.into_owned(),
        gix::remote::Name::Url(url) => to_utf8(url.as_ref(), "リモート URL の解釈")?,
    };

    let Some(merge_ref) = repository.branch_remote_ref_name(full_name.as_ref(), direction) else {
        return Ok(None);
    };
    let merge_ref = merge_ref
        .map_err(|source| read_error(&format!("`{branch}` の upstream の解決"), source))?;
    let merge_ref = to_utf8(merge_ref.as_bstr(), "upstream の参照名の解釈")?;

    Ok(Some(Upstream { remote, merge_ref }))
}

/// `git rev-list --left-right --count` の出力をパースする。
///
/// 出力は `<左だけに含まれる件数>\t<右だけに含まれる件数>\n` の 1 行
/// （末尾の改行は無い場合もある）。`<local>...<remote>` の順で渡すため、
/// 左が ahead、右が behind に対応する。
///
/// # Errors
///
/// フィールド数が 2 でない場合、数値として解釈できない場合、
/// UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_ahead_behind(output: &[u8]) -> Result<(usize, usize)> {
    let malformed = || {
        read_error(
            "git rev-list 出力の解釈",
            format!(
                "ahead/behind の形式が想定と異なります: {:?}",
                output.as_bstr().to_str_lossy()
            ),
        )
    };

    let text = output
        .to_str()
        .map_err(|_| malformed())?
        .trim_end_matches(['\n', '\r']);

    let mut fields = text.split('\t');
    let (Some(ahead), Some(behind), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(malformed());
    };

    let ahead = ahead.parse::<usize>().map_err(|_| malformed())?;
    let behind = behind.parse::<usize>().map_err(|_| malformed())?;
    Ok((ahead, behind))
}

/// `revision` が解決できるかどうかを判定する。
///
/// `git rev-parse --verify --quiet` は解決できない場合にメッセージを出さず非ゼロ終了するため、
/// 終了コードだけで「存在するか」を判定できる。
fn revision_exists(workdir: &Path, revision: &str) -> Result<bool> {
    // `^{commit}` を付けて、同名のファイルやタグではなくコミットとして解決できることを確かめる
    let specification = format!("{revision}^{{commit}}");
    let (code, _) = capture_git_with_status_in(
        workdir,
        &["rev-parse", "--verify", "--quiet", &specification],
    )?;

    Ok(code == 0)
}

/// `local` が `remote_ref` に対して何コミット進んでいる / 遅れているかを返す。
///
/// `remote_ref` が存在しない場合（まだ push していないブランチなど）は `None` を返す。
///
/// # Errors
///
/// `git` の実行に失敗した場合、または出力をパースできない場合にエラーを返す。
pub fn ahead_behind(
    workdir: &Path,
    local: &str,
    remote_ref: &str,
) -> Result<Option<(usize, usize)>> {
    if !revision_exists(workdir, remote_ref)? {
        return Ok(None);
    }

    // `<左>...<右>` の対称差を左右別に数える。左＝ローカルにしかないコミット数＝ahead
    let range = format!("{local}...{remote_ref}");
    // 末尾の `--` はリビジョンとパスの境界。同名のファイルがあってもパスとして解釈させない
    let output = capture_git_in(
        workdir,
        &["rev-list", "--left-right", "--count", &range, "--"],
    )?;

    parse_ahead_behind(&output).map(Some)
}

/// `git rev-list --count` の出力をパースする。
///
/// 出力は件数だけの 1 行（末尾の改行は無い場合もある）。
///
/// # Errors
///
/// 数値として解釈できない場合、UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_commit_count(output: &[u8]) -> Result<usize> {
    let malformed = || {
        read_error(
            "git rev-list 出力の解釈",
            format!(
                "コミット数の形式が想定と異なります: {:?}",
                output.as_bstr().to_str_lossy()
            ),
        )
    };

    output
        .to_str()
        .map_err(|_| malformed())?
        .trim_end_matches(['\n', '\r'])
        .parse::<usize>()
        .map_err(|_| malformed())
}

/// `range`（`<from>..<to>` 形式）に含まれるコミット数を数える。
///
/// `gz merge` で取り込まれるコミット数、`gz rebase` で replay されるコミット数を
/// 確認プロンプトに提示するために用いる。
///
/// # Errors
///
/// `git` の実行に失敗した場合、または出力をパースできない場合にエラーを返す。
pub fn commit_count(workdir: &Path, range: &str) -> Result<usize> {
    // 末尾の `--` はリビジョンとパスの境界。同名のファイルがあってもパスとして解釈させない
    let output = capture_git_in(workdir, &["rev-list", "--count", range, "--"])?;

    parse_commit_count(&output)
}

/// push 先の候補 1 件分の情報（リモート × 現在のブランチ）。
///
/// `remote` / `branch` はいずれも gix が列挙した値であり、ユーザーの自由入力ではない。
/// `git push` はパス以外の位置引数を取り `--` で保護できないため、この由来を保つことが
/// そのままインジェクション対策になる（design.md セキュリティ設計）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    /// リモート名（`origin` 等）。
    pub remote: String,
    /// push するブランチ名（現在のブランチ）。
    pub branch: String,
    /// 現在のブランチの upstream に対応する候補かどうか。
    pub is_upstream: bool,
    /// リモート追跡参照に対する (ahead, behind)。追跡参照が無い場合は `None`。
    pub ahead_behind: Option<(usize, usize)>,
}

impl PushTarget {
    /// リモート追跡参照の短縮名（`origin/main`）。表示と選択結果の照合に用いる。
    #[must_use]
    pub fn tracking_name(&self) -> String {
        format!(
            "{remote}/{branch}",
            remote = self.remote,
            branch = self.branch
        )
    }

    /// リモート追跡参照の完全な参照名（`refs/remotes/origin/main`）。
    ///
    /// git へ渡す際は短縮名ではなくこちらを使う。同名のローカルブランチ・タグがあっても
    /// リモート追跡参照として解決されることを保証するため。
    #[must_use]
    pub fn tracking_ref(&self) -> String {
        format!("refs/remotes/{name}", name = self.tracking_name())
    }
}

/// `upstream` が「リモート `remote` の `refs/heads/<branch>`」を指しているかどうか。
///
/// `gz push` の候補は `git push <remote> <branch>`（= リモート側の `refs/heads/<branch>` を更新）
/// であるため、リモート名だけでなく更新先の参照名まで一致して初めて upstream と同じ宛先といえる。
fn targets_upstream(upstream: Option<&Upstream>, remote: &str, branch: &str) -> bool {
    upstream.is_some_and(|upstream| {
        upstream.remote == remote && upstream.merge_ref == format!("refs/heads/{branch}")
    })
}

/// push 先の候補（リモート × 現在のブランチ）をリモート名順で取得する。
///
/// # Errors
///
/// - HEAD がブランチを指していない場合は [`Error::DetachedHead`]
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - リモート・upstream の読み取り、ahead/behind の算出に失敗した場合はそれぞれのエラー
pub fn push_targets(repository: &gix::Repository) -> Result<Vec<PushTarget>> {
    let Some(branch) = current_branch(repository)? else {
        return Err(Error::DetachedHead);
    };

    let upstream = upstream(repository, &branch)?;
    let workdir = workdir(repository)?;
    // まだコミットが 1 件も無いブランチはローカル側を解決できず、比較そのものが成立しない
    let branch_exists = revision_exists(workdir, &branch)?;

    let mut targets = Vec::new();
    for remote in remotes(repository)? {
        let mut target = PushTarget {
            remote,
            branch: branch.clone(),
            is_upstream: false,
            ahead_behind: None,
        };
        target.is_upstream = targets_upstream(upstream.as_ref(), &target.remote, &branch);
        if branch_exists {
            target.ahead_behind = ahead_behind(workdir, &branch, &target.tracking_ref())?;
        }
        targets.push(target);
    }

    Ok(targets)
}

/// 進行中の git 操作のうち、fuzgit が復帰メニューを提供するもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// merge が進行中（`.git/MERGE_HEAD` がある）。
    Merge,
    /// rebase が進行中（`.git/rebase-merge` または `.git/rebase-apply` がある）。
    Rebase,
}

/// merge / rebase が進行中かどうかを返す。
///
/// cherry-pick / revert / bisect / `git am` も「進行中の操作」ではあるが、
/// fuzgit が continue / abort の復帰メニューを提供するのは merge と rebase だけであるため
/// `None`（進行中ではない扱い）を返す。
#[must_use]
pub fn operation_in_progress(repository: &gix::Repository) -> Option<Operation> {
    use gix::state::InProgress;

    match repository.state()? {
        InProgress::Merge => Some(Operation::Merge),
        // 非対話の `git rebase` でも merge バックエンドは `rebase-merge/interactive` を作るため、
        // RebaseInteractive も通常の rebase として扱う。ApplyMailboxRebase は
        // `rebase-apply` があり `applying` が無い状態で、git 自身も rebase 進行中と判定する
        InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase => {
            Some(Operation::Rebase)
        }
        InProgress::ApplyMailbox
        | InProgress::Bisect
        | InProgress::CherryPick
        | InProgress::CherryPickSequence
        | InProgress::Revert
        | InProgress::RevertSequence => None,
    }
}

/// 走査の起点となるコミット（tip）を [`CommitScope`] に応じて集める。
///
/// 起点が存在しない場合（unborn HEAD、ブランチが 1 件も無いリポジトリ）は空の `Vec` を返す。
fn commit_tips(repository: &gix::Repository, scope: CommitScope<'_>) -> Result<Vec<gix::ObjectId>> {
    match scope {
        CommitScope::Head => {
            let mut head = repository
                .head()
                .map_err(|source| read_error("HEAD の読み取り", source))?;
            let head_id = head
                .try_peel_to_id()
                .map_err(|source| read_error("HEAD の解決", source))?;
            Ok(head_id.map(|id| id.detach()).into_iter().collect())
        }
        CommitScope::AllBranches => {
            let platform = repository
                .references()
                .map_err(|source| read_error("参照の列挙", source))?;
            let locals = platform
                .local_branches()
                .map_err(|source| read_error("ローカルブランチの列挙", source))?;
            let remotes = platform
                .remote_branches()
                .map_err(|source| read_error("リモート追跡ブランチの列挙", source))?;

            let mut tips = Vec::new();
            for reference in locals.chain(remotes) {
                let reference =
                    reference.map_err(|source| read_error("ブランチの読み取り", source))?;
                let id = reference
                    .into_fully_peeled_id()
                    .map_err(|source| read_error("ブランチ先端の解決", source))?;
                tips.push(id.detach());
            }
            // 複数ブランチが同じコミットを指していても rev_walk 側で重複は除かれる
            Ok(tips)
        }
        CommitScope::Branch(name) => {
            let id = repository
                .rev_parse_single(name)
                .map_err(|source| read_error(&format!("ブランチ `{name}` の解決"), source))?;
            Ok(vec![id.detach()])
        }
    }
}

/// 走査の並び順を [`CommitScope`] に応じて決める。
fn sorting(scope: CommitScope<'_>) -> gix::revision::walk::Sorting {
    match scope {
        // 起点が 1 つの場合、グラフ順はそのまま HEAD からの祖先順になる（P1 の `log` と同じ順序）
        CommitScope::Head | CommitScope::Branch(_) => gix::revision::walk::Sorting::BreadthFirst,
        // 複数の起点をグラフ順で辿るとブランチごとにコミットが偏り、limit で打ち切った際に
        // 一部のブランチの新しいコミットが候補から漏れるため、コミット日時の新しい順に揃える
        CommitScope::AllBranches => gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ),
    }
}

/// `scope` の範囲のコミットを新しい順に最大 `limit` 件取得する。
///
/// # Errors
///
/// - 起点の解決・履歴の走査・コミットの復号に失敗した場合は [`Error::RepositoryReadFailed`]
/// - コミットが 1 件も無く、その原因が unborn HEAD である場合は [`Error::UnbornHead`]
pub fn commits(
    repository: &gix::Repository,
    scope: CommitScope<'_>,
    limit: usize,
) -> Result<Vec<CommitInfo>> {
    let tips = commit_tips(repository, scope)?;

    let walk = repository
        .rev_walk(tips)
        .sorting(sorting(scope))
        .all()
        .map_err(|source| read_error("コミット履歴の走査", source))?;

    // 大規模リポジトリでも初期表示を保つため、rev_walk は limit 件で打ち切る
    let mut commits = Vec::new();
    for info in walk.take(limit) {
        let info = info.map_err(|source| read_error("コミット履歴の走査", source))?;
        let commit = info
            .object()
            .map_err(|source| read_error("コミットオブジェクトの取得", source))?;
        commits.push(commit_info(&commit)?);
    }

    if commits.is_empty() {
        reject_unborn_head(repository)?;
    }

    Ok(commits)
}

/// `gix` のコミットオブジェクトを [`CommitInfo`] へ変換する。
fn commit_info(commit: &gix::Commit<'_>) -> Result<CommitInfo> {
    let short_id = commit
        .short_id()
        .map_err(|source| read_error("短縮ハッシュの算出", source))?;
    let message = commit
        .message()
        .map_err(|source| read_error("コミットメッセージの解釈", source))?;
    let author = commit
        .author()
        .map_err(|source| read_error("コミット作者の解釈", source))?
        .trim();
    let time = author
        .time()
        .map_err(|source| read_error("コミット日時の解釈", source))?
        .format(gix::date::time::format::SHORT)
        .map_err(|source| read_error("コミット日時の整形", source))?;

    Ok(CommitInfo {
        id: commit.id().to_string(),
        short_id: short_id.to_string(),
        summary: to_utf8(&message.summary(), "コミットサマリの解釈")?,
        author: to_utf8(author.name, "コミット作者名の解釈")?,
        time,
    })
}

/// `git status` の実行引数。
///
/// - `-z`: パスを NUL 区切り・エスケープなしで出力させる。既定の出力は空白や非 ASCII を含むパスを
///   ダブルクォートとバックスラッシュでエスケープする（`core.quotepath`）ため、解析側でその復元が
///   必要になるうえ、パス中の空白と区切りの空白を区別できない。NUL 区切りならどちらの問題も起きない
/// - `--untracked-files=all`: 未追跡ディレクトリを 1 エントリにまとめず、ファイル単位で列挙させる
///   （`gz add` でファイル単位に選択・プレビューできるようにするため）
const STATUS_ARGS: [&str; 4] = ["status", "--porcelain", "-z", "--untracked-files=all"];

/// `git status --porcelain` の 1 エントリのうち、状態コードと区切り空白が占める幅（`XY `）。
const STATUS_FIELD_WIDTH: usize = 3;

/// 未追跡を表す状態コード（`??`）。
const UNTRACKED_CODE: char = '?';

/// 変更なしを表す状態コード。
const UNMODIFIED_CODE: char = ' ';

/// マージ未解決を表す状態コード（`UU` / `AU` / `UD` など）。
const UNMERGED_CODE: char = 'U';

/// 追加を表す状態コード。両側が `A` の場合（`AA`）はマージ未解決を意味する。
const ADDED_CODE: char = 'A';

/// 削除を表す状態コード。両側が `D` の場合（`DD`）はマージ未解決を意味する。
const DELETED_CODE: char = 'D';

/// 候補に含める変更ファイルの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeScope {
    /// ステージ済みの変更（`gz restore --staged` の対象）。
    Staged,
    /// 作業ツリーの変更（`gz restore` の対象）。
    Worktree,
    /// 未ステージの変更と未追跡ファイル（`gz add` の対象）。
    Stageable,
    /// 追跡済みファイルの変更（`gz stash push` の対象）。
    ///
    /// `git stash push -- <paths>` はステージ済みの変更も含めて退避するため、
    /// [`ChangeScope::Worktree`] と違い index 側だけに変更があるファイルも対象に含める。
    Tracked,
    /// 追跡済みファイルの変更と未追跡ファイル（`gz stash push --include-untracked` の対象）。
    ///
    /// 未追跡ファイルは `git stash push` に `--include-untracked` を付けたときだけ退避できるため、
    /// [`ChangeScope::Tracked`] と分けて指定する。
    TrackedOrUntracked,
    /// マージ未解決（コンフリクト中）のファイル。
    ///
    /// merge / rebase の復帰メニューで、解決したファイルを stage する対象に用いる。
    Unmerged,
}

impl ChangeScope {
    /// 変更がこの範囲に含まれるかどうかを判定する。
    fn includes(self, change: &FileChange) -> bool {
        match self {
            ChangeScope::Staged => change.has_staged_change(),
            ChangeScope::Worktree => change.has_worktree_change(),
            ChangeScope::Stageable => change.is_untracked() || change.has_worktree_change(),
            ChangeScope::Tracked => change.has_staged_change() || change.has_worktree_change(),
            ChangeScope::TrackedOrUntracked => {
                change.is_untracked() || change.has_staged_change() || change.has_worktree_change()
            }
            ChangeScope::Unmerged => change.is_unmerged(),
        }
    }
}

/// 変更ファイル 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// 作業ツリールートからの相対パス。リネーム・コピーの場合は変更後のパス。
    pub path: String,
    /// リネーム・コピー元のパス（`R` / `C` のエントリのみ）。
    pub original_path: Option<String>,
    /// index 側の状態コード（`git status --porcelain` の 1 文字目。未追跡は `?`）。
    pub index_status: char,
    /// 作業ツリー側の状態コード（2 文字目。未追跡は `?`）。
    pub worktree_status: char,
}

impl FileChange {
    /// 未追跡ファイル（`??`）かどうか。
    #[must_use]
    pub fn is_untracked(&self) -> bool {
        self.index_status == UNTRACKED_CODE
    }

    /// HEAD と index の間に差分がある（ステージ済みの変更を持つ）かどうか。
    #[must_use]
    pub fn has_staged_change(&self) -> bool {
        !self.is_untracked() && self.index_status != UNMODIFIED_CODE
    }

    /// index と作業ツリーの間に差分がある（未ステージの変更を持つ）かどうか。
    #[must_use]
    pub fn has_worktree_change(&self) -> bool {
        !self.is_untracked() && self.worktree_status != UNMODIFIED_CODE
    }

    /// マージが未解決（コンフリクト中）かどうか。
    ///
    /// `git status --porcelain` がマージ未解決として扱う状態コードは
    /// `DD` / `AU` / `UD` / `UA` / `DU` / `AA` / `UU` の 7 種類。
    /// どちらかが `U` の 5 種類に加えて、両側が `A`（双方が追加）と両側が `D`（双方が削除）が含まれる。
    #[must_use]
    pub fn is_unmerged(&self) -> bool {
        let (index, worktree) = (self.index_status, self.worktree_status);

        index == UNMERGED_CODE
            || worktree == UNMERGED_CODE
            || (index == ADDED_CODE && worktree == ADDED_CODE)
            || (index == DELETED_CODE && worktree == DELETED_CODE)
    }

    /// `git status --porcelain` と同じ 2 文字の状態コード（`MM` / ` M` / `??` など）。
    #[must_use]
    pub fn status_code(&self) -> String {
        [self.index_status, self.worktree_status].iter().collect()
    }
}

/// タグ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    /// 短縮名（`refs/tags/` を除いた `v1.0` の形式）。
    pub name: String,
    /// タグ参照が直接指すオブジェクトの ID。annotated tag ではタグオブジェクト自身。
    ///
    /// `git show` へ渡すとタグのメッセージと対象コミットの両方が表示され、
    /// `git switch --detach` / `git diff` へ渡した場合は git 側でコミットまで peel される。
    /// タグ名ではなくこの ID を git へ渡すことで、名前がオプションとして解釈される余地を排除する。
    pub id: String,
    /// annotated tag のメッセージ 1 行目。lightweight tag では `None`。
    pub message: Option<String>,
}

/// reflog エントリ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// 新しいものから数えた位置。`HEAD@{n}` の `n` に対応する。
    pub index: usize,
    /// エントリの適用後に HEAD が指していたオブジェクトのフルハッシュ。
    pub id: String,
    /// reflog のメッセージ（`checkout: moving from main to feature` 等）。
    pub message: String,
}

impl ReflogEntry {
    /// エントリを一意に指す表記（`HEAD@{n}`）。
    ///
    /// 同じコミット・同じメッセージのエントリが並び得るため、選択結果の照合に用いる。
    #[must_use]
    pub fn selector(&self) -> String {
        format!("HEAD@{{{index}}}", index = self.index)
    }
}

/// stash エントリ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// `stash@{n}` の `n`。
    pub index: usize,
    /// stash のメッセージ（`WIP on main: 5d21a8c first` / `On main: 作業中` 等）。
    pub message: String,
}

impl StashEntry {
    /// git へ渡す stash の参照（`stash@{n}`）。
    ///
    /// git の出力をそのまま持ち回らず、パース済みの添字から組み立て直すことで、
    /// 想定外の文字列が git の引数として渡らないことを保証する。
    #[must_use]
    pub fn selector(&self) -> String {
        format!("stash@{{{index}}}", index = self.index)
    }
}

/// リビジョンが指すツリーのファイル一覧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionFiles {
    /// 解決済みのフルハッシュ。
    ///
    /// `git` へ渡す際は指定された文字列ではなくこちらを使う。ユーザー入力が
    /// オプションとして解釈される余地を排除し、存在しないリビジョンを早期に弾くため。
    pub id: String,
    /// 作業ツリールートからの相対パス（git が返す辞書順）。
    pub paths: Vec<String>,
}

/// NUL 区切りの出力をレコード列へ分割する。
///
/// 末尾の区切り文字が生む空レコードは取り除く。git が返すパスは空文字になり得ないため、
/// 空レコードを捨てても情報は失われない。
fn nul_records(output: &[u8]) -> impl Iterator<Item = &[u8]> {
    output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
}

/// `git status --porcelain -z` の出力をパースする。
///
/// 1 エントリは `XY <path>` で、リネーム・コピー（`R` / `C`）の場合のみ変更元のパスが
/// 次のレコードとして続く（`-z` では `<変更後>` `<変更元>` の順になる）。
///
/// # Errors
///
/// エントリの形式が想定と異なる場合、変更元のレコードが欠けている場合、
/// パスが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_status(output: &[u8]) -> Result<Vec<FileChange>> {
    let mut records = nul_records(output);
    let mut changes = Vec::new();

    while let Some(record) = records.next() {
        if record.len() <= STATUS_FIELD_WIDTH || record[2] != b' ' {
            return Err(read_error(
                "git status 出力の解釈",
                format!(
                    "エントリの形式が想定と異なります: {:?}",
                    record.as_bstr().to_str_lossy()
                ),
            ));
        }

        let index_status = char::from(record[0]);
        let worktree_status = char::from(record[1]);
        let path = to_utf8(record[STATUS_FIELD_WIDTH..].as_bstr(), "パスの解釈")?;

        let original_path = if is_rename_or_copy(index_status) || is_rename_or_copy(worktree_status)
        {
            let original = records.next().ok_or_else(|| {
                read_error(
                    "git status 出力の解釈",
                    format!("`{path}` のリネーム元のパスが見つかりません"),
                )
            })?;
            Some(to_utf8(original.as_bstr(), "リネーム元のパスの解釈")?)
        } else {
            None
        };

        changes.push(FileChange {
            path,
            original_path,
            index_status,
            worktree_status,
        });
    }

    Ok(changes)
}

/// 状態コードがリネーム（`R`）・コピー（`C`）かどうか。
fn is_rename_or_copy(status: char) -> bool {
    status == 'R' || status == 'C'
}

/// 作業ツリーの変更ファイルを `scope` で絞り込んで取得する。
///
/// # Errors
///
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - `git status` の実行に失敗した場合は [`Error::GitCommandFailed`] 等
/// - 出力のパースに失敗した場合は [`Error::RepositoryReadFailed`]
pub fn changes(repository: &gix::Repository, scope: ChangeScope) -> Result<Vec<FileChange>> {
    let output = capture_git_in(workdir(repository)?, &STATUS_ARGS)?;

    Ok(parse_status(&output)?
        .into_iter()
        .filter(|change| scope.includes(change))
        .collect())
}

/// `revision` が指すツリーに含まれるファイルの一覧を取得する。
///
/// # Errors
///
/// - リビジョンを解決できない場合は [`Error::RepositoryReadFailed`]
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - `git ls-tree` の実行・出力のパースに失敗した場合はそれぞれのエラー
pub fn revision_files(repository: &gix::Repository, revision: &str) -> Result<RevisionFiles> {
    let id = repository
        .rev_parse_single(revision)
        .map_err(|source| read_error(&format!("リビジョン `{revision}` の解決"), source))?
        .to_string();

    // `--full-tree` を付けないと ls-tree はカレントディレクトリ基準のパスを返すため、
    // `git status` と同じく作業ツリールート基準へ揃える
    let output = capture_git_in(
        workdir(repository)?,
        &["ls-tree", "-r", "--name-only", "-z", "--full-tree", &id],
    )?;

    let mut paths = Vec::new();
    for record in nul_records(&output) {
        paths.push(to_utf8(record.as_bstr(), "パスの解釈")?);
    }

    Ok(RevisionFiles { id, paths })
}

/// 複数行のメッセージから 1 行目を取り出す。
fn first_line(message: &BStr) -> &BStr {
    let end = message
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(message.len());
    message[..end].as_bstr()
}

/// タグ一覧を名前順で取得する。
///
/// annotated tag（タグオブジェクトを伴うタグ）はそのメッセージの 1 行目も併せて返す。
///
/// # Errors
///
/// 参照の列挙・解決、タグオブジェクトの復号に失敗した場合、
/// またはタグ名・メッセージが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn tags(repository: &gix::Repository) -> Result<Vec<TagInfo>> {
    let platform = repository
        .references()
        .map_err(|source| read_error("参照の列挙", source))?;

    let mut tags = Vec::new();
    for reference in platform
        .tags()
        .map_err(|source| read_error("タグの列挙", source))?
    {
        let mut reference = reference.map_err(|source| read_error("タグの読み取り", source))?;
        let name = to_utf8(reference.name().shorten(), "タグ名の解釈")?;

        // シンボリック参照は辿るが、annotated tag のタグオブジェクトは peel せずに保持する。
        // タグのメッセージを読むにはタグオブジェクト自体が必要なため
        let id = reference
            .follow_to_object()
            .map_err(|source| read_error(&format!("タグ `{name}` の解決"), source))?;
        let object = id
            .object()
            .map_err(|source| read_error(&format!("タグ `{name}` の対象の取得"), source))?;

        let message = match object.kind {
            gix::object::Kind::Tag => {
                let tag = object
                    .try_into_tag()
                    .map_err(|source| read_error(&format!("タグ `{name}` の解釈"), source))?;
                let decoded = tag
                    .decode()
                    .map_err(|source| read_error(&format!("タグ `{name}` の復号"), source))?;
                Some(to_utf8(
                    first_line(decoded.message),
                    "タグメッセージの解釈",
                )?)
            }
            // lightweight tag は参照が直接コミット等を指すためメッセージを持たない
            _ => None,
        };

        tags.push(TagInfo {
            name,
            id: id.to_string(),
            message,
        });
    }

    tags.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tags)
}

/// HEAD の reflog を新しい順に取得する。
///
/// 削除されたブランチの調査が用途であり、gc 済みで実体を失ったオブジェクトを指すエントリも
/// そのまま返す（オブジェクトデータベースは参照しない）。
///
/// # Errors
///
/// HEAD の参照を開けない場合、reflog の読み取り・解釈に失敗した場合、
/// またはメッセージが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn head_reflog(repository: &gix::Repository) -> Result<Vec<ReflogEntry>> {
    let reference = repository
        .find_reference("HEAD")
        .map_err(|source| read_error("HEAD の読み取り", source))?;

    let mut platform = reference.log_iter();
    // reflog が 1 度も記録されていないリポジトリでは reflog ファイル自体が存在しない
    let Some(lines) = platform
        .rev()
        .map_err(|source| read_error("HEAD の reflog の読み取り", source))?
    else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    for (index, line) in lines.enumerate() {
        let line = line.map_err(|source| read_error("HEAD の reflog の解釈", source))?;
        entries.push(ReflogEntry {
            index,
            id: line.new_oid.to_string(),
            message: to_utf8(line.message.as_bstr(), "reflog メッセージの解釈")?,
        });
    }

    Ok(entries)
}

/// `git stash list` の実行引数。
///
/// - `--format=%gd%x00%gs`: 参照（`stash@{n}`）とメッセージを NUL で区切って出力させる。
///   既定の出力形式（`stash@{0}: WIP on main: 5d21a8c first`）はメッセージ自体がコロンを
///   含み得るため、コロンでの分割では参照とメッセージを確実に切り分けられない
/// - `-z`: エントリ同士も NUL で区切らせる。メッセージ（reflog subject）は改行を含まないが、
///   区切りを NUL に統一することでフィールド区切りと同じ規則で解析できる
///   （`git status` を `-z` で読むのと同じ方針）
const STASH_LIST_ARGS: [&str; 4] = ["stash", "list", "-z", "--format=%gd%x00%gs"];

/// stash の参照（`stash@{n}`）の前置き。
const STASH_SELECTOR_PREFIX: &str = "stash@{";

/// stash の参照（`stash@{n}`）の後置き。
const STASH_SELECTOR_SUFFIX: &str = "}";

/// `stash@{n}` 形式の参照から添字を取り出す。
fn parse_stash_selector(selector: &str) -> Option<usize> {
    selector
        .strip_prefix(STASH_SELECTOR_PREFIX)?
        .strip_suffix(STASH_SELECTOR_SUFFIX)?
        .parse()
        .ok()
}

/// [`STASH_LIST_ARGS`] の出力をパースする。
///
/// 出力は `<参照>\0<メッセージ>\0` の繰り返しで、末尾にも区切りの NUL が付く。
/// メッセージは空になり得るため空レコードは読み飛ばさず、2 件ずつの組として厳密に扱う。
///
/// # Errors
///
/// レコード数が奇数の場合、参照が `stash@{n}` 形式でない場合、
/// メッセージが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_stash_list(output: &[u8]) -> Result<Vec<StashEntry>> {
    if output.is_empty() {
        return Ok(Vec::new());
    }

    let mut records: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    // 最後のエントリの後ろにも区切りが置かれるため、その分の空レコードだけを取り除く
    if records.last().is_some_and(|record| record.is_empty()) {
        records.pop();
    }

    if !records.len().is_multiple_of(2) {
        return Err(read_error(
            "git stash list 出力の解釈",
            format!(
                "参照とメッセージの組になっていません（{} レコード）",
                records.len()
            ),
        ));
    }

    let mut entries = Vec::with_capacity(records.len() / 2);
    for pair in records.chunks_exact(2) {
        let selector = to_utf8(pair[0].as_bstr(), "stash の参照の解釈")?;
        let index = parse_stash_selector(&selector).ok_or_else(|| {
            read_error(
                "git stash list 出力の解釈",
                format!("`{selector}` は `stash@{{n}}` 形式ではありません"),
            )
        })?;

        entries.push(StashEntry {
            index,
            message: to_utf8(pair[1].as_bstr(), "stash メッセージの解釈")?,
        });
    }

    Ok(entries)
}

/// stash 一覧を新しい順（`stash@{0}` から）に取得する。
///
/// # Errors
///
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - `git stash list` の実行に失敗した場合は [`Error::GitCommandFailed`] 等
/// - 出力のパースに失敗した場合は [`Error::RepositoryReadFailed`]
pub fn stashes(repository: &gix::Repository) -> Result<Vec<StashEntry>> {
    let output = capture_git_in(workdir(repository)?, &STASH_LIST_ARGS)?;
    parse_stash_list(&output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{
        COMMIT_DATE_SHORT, TempDir, commit, commit_at, create_annotated_tag, create_branch,
        create_lightweight_tag, create_remote_branch, create_remote_symbolic_ref, git_in,
        init_repository, stash_changes, try_git_in, write_file,
    };

    /// `main` に 1 コミットだけ持つテストリポジトリを用意する。
    fn repository_with_one_commit(label: &str) -> (TempDir, gix::Repository, String) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository, head)
    }

    fn names(branches: &[BranchInfo]) -> Vec<&str> {
        branches.iter().map(|branch| branch.name.as_str()).collect()
    }

    fn summaries(commits: &[CommitInfo]) -> Vec<&str> {
        commits
            .iter()
            .map(|commit| commit.summary.as_str())
            .collect()
    }

    #[test]
    fn local_branches_are_listed_by_name_with_the_current_one_marked() {
        let (dir, repository, _head) = repository_with_one_commit("read-local-branches");
        create_branch(dir.path(), "feature");
        create_branch(dir.path(), "another");

        let branches = branches(&repository, BranchScope::Local).expect("branches should be read");

        assert_eq!(names(&branches), ["another", "feature", "main"]);
        assert!(branches.iter().all(|branch| !branch.is_remote));
        let current: Vec<&str> = branches
            .iter()
            .filter(|branch| branch.is_current)
            .map(|branch| branch.name.as_str())
            .collect();
        assert_eq!(current, ["main"]);
    }

    #[test]
    fn a_repository_without_commits_reports_the_unborn_branch_instead_of_an_empty_list() {
        let dir = TempDir::new("read-unborn-branches");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        for scope in [BranchScope::Local, BranchScope::All] {
            let err = branches(&repository, scope)
                .expect_err("an unborn HEAD should be reported as its own error");

            match err {
                Error::UnbornHead { branch } => assert_eq!(branch, "main"),
                other => panic!("unexpected error for {scope:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn an_orphan_branch_does_not_hide_the_existing_branches() {
        let (dir, _repository, _head) = repository_with_one_commit("read-orphan-branch");
        git_in(dir.path(), &["checkout", "--quiet", "--orphan", "orphan"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let branches = branches(&repository, BranchScope::Local).expect("branches should be read");

        assert_eq!(names(&branches), ["main"]);
    }

    #[test]
    fn a_detached_head_marks_no_branch_as_current() {
        let (dir, _repository, head) = repository_with_one_commit("read-detached-head");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let branches = branches(&repository, BranchScope::Local).expect("branches should be read");

        assert_eq!(names(&branches), ["main"]);
        assert!(branches.iter().all(|branch| !branch.is_current));
    }

    #[test]
    fn remote_branches_are_excluded_unless_the_scope_is_all() {
        let (dir, repository, head) = repository_with_one_commit("read-remote-scope");
        create_remote_branch(dir.path(), "origin/main", &head);

        let local = branches(&repository, BranchScope::Local).expect("branches should be read");
        assert_eq!(names(&local), ["main"]);

        let all = branches(&repository, BranchScope::All).expect("branches should be read");
        assert_eq!(names(&all), ["main", "origin/main"]);
        assert_eq!(
            all.iter()
                .map(|branch| branch.is_remote)
                .collect::<Vec<_>>(),
            [false, true]
        );
    }

    #[test]
    fn symbolic_remote_references_are_not_offered_as_candidates() {
        let (dir, repository, head) = repository_with_one_commit("read-remote-symbolic");
        create_remote_branch(dir.path(), "origin/main", &head);
        create_remote_symbolic_ref(dir.path(), "origin/HEAD", "origin/main");

        let all = branches(&repository, BranchScope::All).expect("branches should be read");

        assert_eq!(names(&all), ["main", "origin/main"]);
    }

    #[test]
    fn commits_are_returned_from_head_backwards() {
        let dir = TempDir::new("read-commits-order");
        init_repository(dir.path());
        commit(dir.path(), "first");
        commit(dir.path(), "second");
        let third = commit(dir.path(), "third");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits = commits(&repository, CommitScope::Head, 10).expect("commits should be read");

        assert_eq!(summaries(&commits), ["third", "second", "first"]);
        assert_eq!(commits[0].id, third);
    }

    #[test]
    fn commits_are_truncated_to_the_limit() {
        let dir = TempDir::new("read-commits-limit");
        init_repository(dir.path());
        commit(dir.path(), "first");
        commit(dir.path(), "second");
        commit(dir.path(), "third");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits = commits(&repository, CommitScope::Head, 2).expect("commits should be read");

        assert_eq!(summaries(&commits), ["third", "second"]);
    }

    #[test]
    fn a_limit_of_zero_yields_no_commits() {
        let (_dir, repository, _head) = repository_with_one_commit("read-commits-zero");

        let commits = commits(&repository, CommitScope::Head, 0).expect("commits should be read");

        assert!(commits.is_empty(), "unexpected commits: {commits:?}");
    }

    #[test]
    fn a_repository_without_commits_reports_the_unborn_branch() {
        let dir = TempDir::new("read-commits-unborn");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = commits(&repository, CommitScope::Head, 10)
            .expect_err("an unborn HEAD should be reported as its own error");

        match err {
            Error::UnbornHead { branch } => assert_eq!(branch, "main"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn an_orphan_branch_is_reported_as_unborn_even_when_other_commits_exist() {
        let (dir, _repository, _head) = repository_with_one_commit("read-commits-orphan");
        git_in(dir.path(), &["checkout", "--quiet", "--orphan", "orphan"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = commits(&repository, CommitScope::Head, 10)
            .expect_err("an unborn HEAD should be reported as its own error");

        match err {
            Error::UnbornHead { branch } => assert_eq!(branch, "orphan"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 2 本のブランチに分岐した履歴を持つテストリポジトリを用意する。
    ///
    /// ```text
    /// first (01-01) ─┬─ main only    (01-02)  <- main（HEAD）
    ///                └─ feature only (01-03)  <- feature
    /// ```
    ///
    /// コミット日時をずらしてあるため、日時順とブランチのまとまりが一致しない。
    fn repository_with_two_branches(label: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        commit_at(dir.path(), "first", "2024-01-01T00:00:00+00:00");
        git_in(dir.path(), &["switch", "--quiet", "-c", "feature"]);
        commit_at(dir.path(), "feature only", "2024-01-03T00:00:00+00:00");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        commit_at(dir.path(), "main only", "2024-01-02T00:00:00+00:00");

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn the_head_scope_ignores_commits_of_other_branches() {
        let (_dir, repository) = repository_with_two_branches("read-commits-head-scope");

        let commits = commits(&repository, CommitScope::Head, 10).expect("commits should be read");

        assert_eq!(summaries(&commits), ["main only", "first"]);
    }

    #[test]
    fn the_all_branches_scope_merges_every_branch_by_commit_time() {
        let (_dir, repository) = repository_with_two_branches("read-commits-all-scope");

        let commits =
            commits(&repository, CommitScope::AllBranches, 10).expect("commits should be read");

        assert_eq!(
            summaries(&commits),
            ["feature only", "main only", "first"],
            "commits of all branches should be interleaved by commit time"
        );
    }

    #[test]
    fn a_commit_shared_by_several_branches_is_listed_once() {
        let (_dir, repository) = repository_with_two_branches("read-commits-shared");

        let commits =
            commits(&repository, CommitScope::AllBranches, 10).expect("commits should be read");

        let shared = commits
            .iter()
            .filter(|commit| commit.summary == "first")
            .count();
        assert_eq!(shared, 1, "the shared ancestor should not be duplicated");
    }

    #[test]
    fn the_all_branches_scope_includes_remote_tracking_branches() {
        let (dir, _repository) = repository_with_two_branches("read-commits-remote-scope");
        git_in(dir.path(), &["switch", "--quiet", "-c", "temporary"]);
        let head = commit_at(dir.path(), "remote only", "2024-01-04T00:00:00+00:00");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        // ローカルブランチを消し、リモート追跡ブランチからのみ辿れるコミットにする
        git_in(dir.path(), &["branch", "--delete", "--force", "temporary"]);
        create_remote_branch(dir.path(), "origin/temporary", &head);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits =
            commits(&repository, CommitScope::AllBranches, 10).expect("commits should be read");

        assert_eq!(
            summaries(&commits),
            ["remote only", "feature only", "main only", "first"]
        );
    }

    #[test]
    fn the_all_branches_scope_is_truncated_to_the_limit_across_branches() {
        let (_dir, repository) = repository_with_two_branches("read-commits-all-limit");

        let commits =
            commits(&repository, CommitScope::AllBranches, 2).expect("commits should be read");

        assert_eq!(summaries(&commits), ["feature only", "main only"]);
    }

    #[test]
    fn a_branch_scope_is_limited_to_that_branch() {
        let (_dir, repository) = repository_with_two_branches("read-commits-branch-scope");

        let commits = commits(&repository, CommitScope::Branch("feature"), 10)
            .expect("commits should be read");

        assert_eq!(summaries(&commits), ["feature only", "first"]);
    }

    #[test]
    fn an_unknown_branch_is_reported_with_its_name() {
        let (_dir, repository) = repository_with_two_branches("read-commits-unknown-branch");

        let err = commits(&repository, CommitScope::Branch("no-such-branch"), 10)
            .expect_err("an unknown branch must not be silently ignored");

        match err {
            Error::RepositoryReadFailed { operation, .. } => {
                assert!(
                    operation.contains("no-such-branch"),
                    "the branch should be named: {operation}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// `git status --porcelain -z` の出力を組み立てる（各レコードの末尾に NUL を置く）。
    fn status_output(records: &[&str]) -> Vec<u8> {
        let mut output = Vec::new();
        for record in records {
            output.extend_from_slice(record.as_bytes());
            output.push(0);
        }
        output
    }

    fn change(path: &str, code: &str) -> FileChange {
        let mut codes = code.chars();
        FileChange {
            path: path.to_owned(),
            original_path: None,
            index_status: codes.next().expect("a status code has two characters"),
            worktree_status: codes.next().expect("a status code has two characters"),
        }
    }

    fn paths(changes: &[FileChange]) -> Vec<&str> {
        changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>()
    }

    #[test]
    fn an_empty_status_yields_no_changes() {
        let changes = parse_status(&[]).expect("empty output should parse");

        assert!(changes.is_empty(), "unexpected changes: {changes:?}");
    }

    #[test]
    fn staged_unstaged_and_untracked_entries_are_distinguished() {
        let output = status_output(&["M  staged.txt", " M unstaged.txt", "?? new.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(paths(&changes), ["staged.txt", "unstaged.txt", "new.txt"]);
        assert_eq!(
            changes
                .iter()
                .map(FileChange::status_code)
                .collect::<Vec<_>>(),
            ["M ", " M", "??"]
        );
        assert_eq!(
            changes
                .iter()
                .map(FileChange::has_staged_change)
                .collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(
            changes
                .iter()
                .map(FileChange::has_worktree_change)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(
            changes
                .iter()
                .map(FileChange::is_untracked)
                .collect::<Vec<_>>(),
            [false, false, true]
        );
    }

    #[test]
    fn an_entry_can_be_staged_and_unstaged_at_the_same_time() {
        let output = status_output(&["MM both.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let change = changes.first().expect("one entry should be parsed");
        assert!(change.has_staged_change());
        assert!(change.has_worktree_change());
        assert!(!change.is_untracked());
    }

    #[test]
    fn a_path_containing_spaces_is_kept_verbatim() {
        let output = status_output(&[" M dir/with space.txt", "?? another one.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(
            paths(&changes),
            ["dir/with space.txt", "another one.txt"],
            "NUL separated records must not be split on spaces"
        );
    }

    #[test]
    fn a_path_starting_with_a_dash_is_kept_verbatim() {
        let output = status_output(&["?? --looks-like-an-option"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(paths(&changes), ["--looks-like-an-option"]);
    }

    #[test]
    fn a_rename_keeps_both_the_new_and_the_original_path() {
        // `-z` では変更後のパスが先、変更元が次のレコードになる
        let output = status_output(&["R  new.txt", "old.txt", "?? other.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(paths(&changes), ["new.txt", "other.txt"]);
        let rename = changes.first().expect("the rename should be parsed");
        assert_eq!(rename.original_path.as_deref(), Some("old.txt"));
        assert_eq!(rename.status_code(), "R ");
    }

    #[test]
    fn a_rename_with_a_worktree_change_is_reported_once() {
        let output = status_output(&["RM new name.txt", "old name.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(changes.len(), 1);
        let rename = changes.first().expect("the rename should be parsed");
        assert_eq!(rename.path, "new name.txt");
        assert_eq!(rename.original_path.as_deref(), Some("old name.txt"));
        assert!(rename.has_staged_change());
        assert!(rename.has_worktree_change());
    }

    #[test]
    fn a_copy_also_carries_the_original_path() {
        let output = status_output(&["C  copy.txt", "source.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let copy = changes.first().expect("the copy should be parsed");
        assert_eq!(copy.original_path.as_deref(), Some("source.txt"));
    }

    #[test]
    fn a_rename_detected_in_the_work_tree_also_carries_the_original_path() {
        let output = status_output(&[" R new.txt", "old.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let rename = changes.first().expect("the rename should be parsed");
        assert_eq!(rename.original_path.as_deref(), Some("old.txt"));
        assert!(rename.has_worktree_change());
        assert!(!rename.has_staged_change());
    }

    #[test]
    fn an_unmerged_entry_is_reported_for_both_sides() {
        let output = status_output(&["UU conflict.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let conflict = changes.first().expect("the conflict should be parsed");
        assert!(conflict.has_staged_change());
        assert!(conflict.has_worktree_change());
        assert!(!conflict.is_untracked());
    }

    #[test]
    fn a_deletion_is_reported_on_the_side_it_happened() {
        let output = status_output(&["D  staged-delete.txt", " D worktree-delete.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert!(changes[0].has_staged_change());
        assert!(!changes[0].has_worktree_change());
        assert!(changes[1].has_worktree_change());
        assert!(!changes[1].has_staged_change());
    }

    #[test]
    fn a_malformed_entry_is_rejected_instead_of_being_skipped() {
        for record in ["M", "M  ", "M-path.txt"] {
            let output = status_output(&[record]);

            let err =
                parse_status(&output).expect_err("a malformed entry must not be silently ignored");

            assert!(
                matches!(err, Error::RepositoryReadFailed { .. }),
                "unexpected error for {record:?}: {err:?}"
            );
        }
    }

    #[test]
    fn a_rename_without_its_original_path_is_rejected() {
        let output = status_output(&["R  new.txt"]);

        let err = parse_status(&output).expect_err("a truncated rename entry must not be accepted");

        match err {
            Error::RepositoryReadFailed { operation, source } => {
                assert_eq!(operation, "git status 出力の解釈");
                assert!(
                    source.to_string().contains("new.txt"),
                    "the affected path should be named: {source}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_non_utf8_path_is_rejected_instead_of_being_converted_lossily() {
        let mut output = b"?? invalid-".to_vec();
        output.push(0xff);
        output.push(0);

        let err = parse_status(&output).expect_err("a non utf-8 path must not be accepted");

        assert!(
            matches!(err, Error::RepositoryReadFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn a_scope_selects_the_matching_side_of_a_change() {
        let staged = change("a.txt", "M ");
        let unstaged = change("b.txt", " M");
        let both = change("c.txt", "MM");
        let untracked = change("d.txt", "??");

        for (scope, expected) in [
            (ChangeScope::Staged, [true, false, true, false]),
            (ChangeScope::Worktree, [false, true, true, false]),
            (ChangeScope::Stageable, [false, true, true, true]),
            (ChangeScope::Tracked, [true, true, true, false]),
            (ChangeScope::TrackedOrUntracked, [true, true, true, true]),
        ] {
            let actual =
                [&staged, &unstaged, &both, &untracked].map(|change| scope.includes(change));
            assert_eq!(actual, expected, "unexpected selection for {scope:?}");
        }
    }

    /// 各種の変更を持つテストリポジトリを用意する。
    ///
    /// ```text
    /// staged.txt        M    ステージ済みの変更のみ
    /// unstaged.txt       M   未ステージの変更のみ
    /// both.txt          MM   両方
    /// dir/new file.txt  ??   未追跡（空白入りのパス）
    /// renamed.txt       R    ステージ済みのリネーム（元: history.txt）
    /// ```
    fn repository_with_changes(label: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        for name in ["staged.txt", "unstaged.txt", "both.txt"] {
            write_file(dir.path(), name, "original\n");
        }
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");

        write_file(dir.path(), "staged.txt", "staged\n");
        write_file(dir.path(), "both.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "staged.txt", "both.txt"]);
        write_file(dir.path(), "unstaged.txt", "unstaged\n");
        write_file(dir.path(), "both.txt", "unstaged\n");
        write_file(dir.path(), "dir/new file.txt", "untracked\n");
        git_in(dir.path(), &["mv", "history.txt", "renamed.txt"]);

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn the_staged_scope_lists_the_index_side_of_the_changes() {
        let (_dir, repository) = repository_with_changes("read-status-staged");

        let changes = changes(&repository, ChangeScope::Staged).expect("status should be read");

        assert_eq!(paths(&changes), ["both.txt", "renamed.txt", "staged.txt"]);
    }

    #[test]
    fn the_worktree_scope_lists_the_unstaged_changes_only() {
        let (_dir, repository) = repository_with_changes("read-status-worktree");

        let changes = changes(&repository, ChangeScope::Worktree).expect("status should be read");

        assert_eq!(paths(&changes), ["both.txt", "unstaged.txt"]);
    }

    #[test]
    fn the_stageable_scope_lists_unstaged_changes_together_with_untracked_files() {
        let (_dir, repository) = repository_with_changes("read-status-stageable");

        let changes = changes(&repository, ChangeScope::Stageable).expect("status should be read");

        assert_eq!(
            paths(&changes),
            ["both.txt", "unstaged.txt", "dir/new file.txt"],
            "untracked files with spaces should be listed individually"
        );
        let untracked = changes.last().expect("the untracked file should be listed");
        assert!(untracked.is_untracked());
    }

    #[test]
    fn the_tracked_scope_lists_staged_and_unstaged_changes_without_untracked_files() {
        let (_dir, repository) = repository_with_changes("read-status-tracked");

        let changes = changes(&repository, ChangeScope::Tracked).expect("status should be read");

        assert_eq!(
            paths(&changes),
            ["both.txt", "renamed.txt", "staged.txt", "unstaged.txt"],
            "`git stash push` also stashes changes that are only staged"
        );
    }

    #[test]
    fn the_tracked_or_untracked_scope_adds_the_untracked_files() {
        let (_dir, repository) = repository_with_changes("read-status-tracked-untracked");

        let changes =
            changes(&repository, ChangeScope::TrackedOrUntracked).expect("status should be read");

        assert_eq!(
            paths(&changes),
            [
                "both.txt",
                "renamed.txt",
                "staged.txt",
                "unstaged.txt",
                "dir/new file.txt"
            ]
        );
    }

    #[test]
    fn a_staged_rename_reports_both_of_its_paths() {
        let (_dir, repository) = repository_with_changes("read-status-rename");

        let changes = changes(&repository, ChangeScope::Staged).expect("status should be read");

        let rename = changes
            .iter()
            .find(|change| change.path == "renamed.txt")
            .expect("the rename should be listed");
        assert_eq!(rename.original_path.as_deref(), Some("history.txt"));
        assert_eq!(rename.index_status, 'R');
    }

    #[test]
    fn an_unmodified_repository_has_no_changes() {
        let (_dir, repository, _head) = repository_with_one_commit("read-status-clean");

        for scope in [
            ChangeScope::Staged,
            ChangeScope::Worktree,
            ChangeScope::Stageable,
            ChangeScope::Tracked,
            ChangeScope::TrackedOrUntracked,
        ] {
            let changes = changes(&repository, scope).expect("status should be read");
            assert!(changes.is_empty(), "unexpected changes for {scope:?}");
        }
    }

    #[test]
    fn a_bare_repository_has_no_work_tree_to_inspect() {
        let dir = TempDir::new("read-status-bare");
        crate::test_support::init_bare_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = changes(&repository, ChangeScope::Worktree)
            .expect_err("a bare repository has no work tree");

        assert!(
            matches!(err, Error::NoWorktree),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn revision_files_lists_the_files_of_that_revision_with_its_resolved_id() {
        let (dir, repository) = repository_with_changes("read-revision-files");
        let head = git_in(dir.path(), &["rev-parse", "HEAD"]);

        let files = revision_files(&repository, "HEAD").expect("HEAD should be listed");

        assert_eq!(files.id, head, "the revision should be resolved to its id");
        assert_eq!(
            files.paths,
            ["both.txt", "history.txt", "staged.txt", "unstaged.txt"],
            "the listing must reflect the commit, not the work tree"
        );
    }

    #[test]
    fn revision_files_lists_nested_paths_from_the_repository_root() {
        let dir = TempDir::new("read-revision-files-nested");
        init_repository(dir.path());
        write_file(dir.path(), "dir/with space.txt", "content\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let files = revision_files(&repository, "HEAD").expect("HEAD should be listed");

        assert!(
            files.paths.contains(&"dir/with space.txt".to_owned()),
            "unexpected listing: {:?}",
            files.paths
        );
    }

    #[test]
    fn an_unknown_revision_is_reported_with_its_name() {
        let (_dir, repository, _head) = repository_with_one_commit("read-revision-unknown");

        let err = revision_files(&repository, "no-such-revision")
            .expect_err("an unknown revision must not be silently ignored");

        match err {
            Error::RepositoryReadFailed { operation, .. } => assert!(
                operation.contains("no-such-revision"),
                "the revision should be named: {operation}"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_repository_without_tags_yields_no_candidates() {
        let (_dir, repository, _head) = repository_with_one_commit("read-tags-empty");

        let tags = tags(&repository).expect("tags should be read");

        assert!(tags.is_empty(), "unexpected tags: {tags:?}");
    }

    #[test]
    fn lightweight_and_annotated_tags_are_listed_by_name() {
        let (dir, repository, head) = repository_with_one_commit("read-tags-kinds");
        create_lightweight_tag(dir.path(), "v2.0-light");
        create_annotated_tag(dir.path(), "v1.0", "リリース v1.0\n\n詳細な説明");

        let tags = tags(&repository).expect("tags should be read");

        assert_eq!(
            tags.iter().map(|tag| tag.name.as_str()).collect::<Vec<_>>(),
            ["v1.0", "v2.0-light"]
        );

        let annotated = tags.first().expect("the annotated tag should be listed");
        assert_eq!(
            annotated.message.as_deref(),
            Some("リリース v1.0"),
            "only the first line of the message is kept"
        );
        assert_ne!(
            annotated.id, head,
            "an annotated tag reference points at the tag object itself"
        );

        let lightweight = tags.last().expect("the lightweight tag should be listed");
        assert_eq!(
            lightweight.message, None,
            "a lightweight tag carries no message"
        );
        assert_eq!(
            lightweight.id, head,
            "a lightweight tag reference points at the commit"
        );
    }

    #[test]
    fn an_annotated_tag_id_resolves_to_its_commit_for_git() {
        let (dir, repository, head) = repository_with_one_commit("read-tags-peel");
        create_annotated_tag(dir.path(), "v1.0", "リリース");
        let tags = tags(&repository).expect("tags should be read");
        let tag = tags.first().expect("the tag should be listed");

        let peeled = git_in(
            dir.path(),
            &["rev-parse", &format!("{id}^{{commit}}", id = tag.id)],
        );

        assert_eq!(peeled, head);
    }

    #[test]
    fn the_head_reflog_is_returned_newest_first() {
        let dir = TempDir::new("read-reflog-order");
        init_repository(dir.path());
        commit(dir.path(), "first");
        commit(dir.path(), "second");
        git_in(dir.path(), &["switch", "--quiet", "-c", "feature"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = head_reflog(&repository).expect("the reflog should be read");

        let messages: Vec<&str> = entries.iter().map(|entry| entry.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "checkout: moving from main to feature",
                "commit: second",
                "commit (initial): first"
            ]
        );
        assert_eq!(
            entries.iter().map(|entry| entry.index).collect::<Vec<_>>(),
            [0, 1, 2],
            "the newest entry is HEAD@{{0}}"
        );
    }

    #[test]
    fn a_reflog_entry_carries_the_commit_it_moved_to() {
        let dir = TempDir::new("read-reflog-id");
        init_repository(dir.path());
        let first = commit(dir.path(), "first");
        let second = commit(dir.path(), "second");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = head_reflog(&repository).expect("the reflog should be read");

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [second, first]
        );
    }

    #[test]
    fn a_repository_without_a_reflog_yields_no_entries() {
        let dir = TempDir::new("read-reflog-empty");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = head_reflog(&repository).expect("a missing reflog is not an error");

        assert!(entries.is_empty(), "unexpected entries: {entries:?}");
    }

    /// `git stash list -z --format=%gd%x00%gs` の出力を組み立てる。
    ///
    /// 実際の git は各レコードの後ろに NUL を置く（末尾のエントリも同様）。
    fn stash_output(records: &[&str]) -> Vec<u8> {
        let mut output = Vec::new();
        for record in records {
            output.extend_from_slice(record.as_bytes());
            output.push(0);
        }
        output
    }

    #[test]
    fn an_empty_stash_list_yields_no_entries() {
        let entries = parse_stash_list(&[]).expect("empty output should parse");

        assert!(entries.is_empty(), "unexpected entries: {entries:?}");
    }

    #[test]
    fn a_stash_entry_keeps_its_index_and_message() {
        let output = stash_output(&[
            "stash@{0}",
            "On main: 作業中",
            "stash@{1}",
            "WIP on main: 5d21a8c first",
        ]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(
            entries,
            [
                StashEntry {
                    index: 0,
                    message: "On main: 作業中".to_owned()
                },
                StashEntry {
                    index: 1,
                    message: "WIP on main: 5d21a8c first".to_owned()
                }
            ]
        );
    }

    #[test]
    fn a_message_containing_colons_and_spaces_is_kept_verbatim() {
        let output = stash_output(&["stash@{0}", "On main: fix: 認証 の バグ: 2 件"]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(
            entries[0].message, "On main: fix: 認証 の バグ: 2 件",
            "a colon inside the message must not split the record"
        );
    }

    #[test]
    fn an_output_without_a_trailing_separator_parses_as_well() {
        let mut output = stash_output(&["stash@{0}"]);
        output.extend_from_slice("On main: 作業中".as_bytes());

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "On main: 作業中");
    }

    #[test]
    fn an_empty_message_does_not_shift_the_following_entries() {
        let output = stash_output(&["stash@{0}", "", "stash@{1}", "On main: 次"]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(
            entries,
            [
                StashEntry {
                    index: 0,
                    message: String::new()
                },
                StashEntry {
                    index: 1,
                    message: "On main: 次".to_owned()
                }
            ]
        );
    }

    #[test]
    fn a_double_digit_index_is_parsed() {
        let output = stash_output(&["stash@{12}", "On main: 作業中"]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(entries[0].index, 12);
        assert_eq!(entries[0].selector(), "stash@{12}");
    }

    #[test]
    fn a_dangling_record_is_rejected_instead_of_being_dropped() {
        let mut output = stash_output(&["stash@{0}", "On main: 作業中"]);
        output.extend_from_slice(b"stash@{1}\0");

        let err = parse_stash_list(&output).expect_err("an odd record count must be rejected");

        assert!(
            matches!(err, Error::RepositoryReadFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn a_selector_of_an_unexpected_shape_is_rejected() {
        for selector in ["stash@{x}", "stash@{}", "refs/stash", "stash@{0", "0"] {
            let output = stash_output(&[selector, "On main: 作業中"]);

            let err = parse_stash_list(&output)
                .expect_err("a selector that is not stash@{n} must be rejected");

            match err {
                Error::RepositoryReadFailed { source, .. } => assert!(
                    source.to_string().contains(selector),
                    "the offending selector should be named: {source}"
                ),
                other => panic!("unexpected error for {selector:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_non_utf8_message_is_rejected_instead_of_being_converted_lossily() {
        let mut output = b"stash@{0}\0On main: ".to_vec();
        output.push(0xff);
        output.push(0);

        let err = parse_stash_list(&output).expect_err("a non utf-8 message must not be accepted");

        assert!(
            matches!(err, Error::RepositoryReadFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn the_stash_list_of_a_repository_is_read_newest_first() {
        let dir = TempDir::new("read-stash-list");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        write_file(dir.path(), "history.txt", "旧: 変更 1\n");
        stash_changes(dir.path(), Some("最初の: 退避"));
        write_file(dir.path(), "history.txt", "旧: 変更 2\n");
        stash_changes(dir.path(), None);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = stashes(&repository).expect("the stash list should be read");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 0);
        assert!(
            entries[0].message.starts_with("WIP on main:"),
            "the newest stash comes first: {entries:?}"
        );
        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].message, "On main: 最初の: 退避");
    }

    #[test]
    fn a_repository_without_stashes_yields_no_entries() {
        let (_dir, repository, _head) = repository_with_one_commit("read-stash-empty");

        let entries = stashes(&repository).expect("the stash list should be read");

        assert!(entries.is_empty(), "unexpected entries: {entries:?}");
    }

    #[test]
    fn the_first_line_of_a_message_stops_at_the_newline() {
        assert_eq!(first_line("summary\n\nbody".into()), "summary");
        assert_eq!(first_line("summary".into()), "summary");
        assert_eq!(first_line("".into()), "");
    }

    #[test]
    fn commit_metadata_is_exposed_without_gix_types() {
        let (_dir, repository, head) = repository_with_one_commit("read-commit-metadata");

        let commits = commits(&repository, CommitScope::Head, 1).expect("commits should be read");
        let commit = commits.first().expect("one commit should be present");

        assert_eq!(commit.id, head);
        assert!(
            head.starts_with(&commit.short_id),
            "short id {:?} should be a prefix of {head:?}",
            commit.short_id
        );
        assert!(
            !commit.short_id.is_empty(),
            "short id should not be empty: {commit:?}"
        );
        assert_eq!(commit.summary, "first commit");
        assert_eq!(commit.author, "fuzgit test");
        assert_eq!(commit.time, COMMIT_DATE_SHORT);
    }

    #[test]
    fn the_current_branch_is_the_one_head_points_at() {
        let (dir, repository, _head) = repository_with_one_commit("read-current-branch");

        assert_eq!(
            current_branch(&repository).expect("HEAD should be readable"),
            Some("main".to_string())
        );

        git_in(dir.path(), &["switch", "--quiet", "--create", "feature"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        assert_eq!(
            current_branch(&repository).expect("HEAD should be readable"),
            Some("feature".to_string())
        );
    }

    #[test]
    fn a_detached_head_has_no_current_branch() {
        let (dir, _repository, head) = repository_with_one_commit("read-current-branch-detached");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        assert_eq!(
            current_branch(&repository).expect("HEAD should be readable"),
            None
        );
    }

    #[test]
    fn remotes_are_listed_by_name() {
        let (dir, _repository, _head) = repository_with_one_commit("read-remotes");
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "upstream",
                "https://example.invalid/up.git",
            ],
        );
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/origin.git",
            ],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let remotes = remotes(&repository).expect("remotes should be read");

        assert_eq!(remotes, ["origin", "upstream"]);
    }

    #[test]
    fn a_repository_without_remotes_lists_none() {
        let (_dir, repository, _head) = repository_with_one_commit("read-remotes-empty");

        let remotes = remotes(&repository).expect("remotes should be read");

        assert!(remotes.is_empty(), "unexpected remotes: {remotes:?}");
    }

    #[test]
    fn an_upstream_is_read_from_the_branch_configuration() {
        let (dir, _repository, head) = repository_with_one_commit("read-upstream");
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/origin.git",
            ],
        );
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let upstream = upstream(&repository, "main")
            .expect("the upstream should be readable")
            .expect("main tracks origin/main");

        assert_eq!(upstream.remote, "origin");
        assert_eq!(upstream.merge_ref, "refs/heads/main");
    }

    #[test]
    fn a_branch_without_tracking_configuration_has_no_upstream() {
        let (dir, repository, _head) = repository_with_one_commit("read-upstream-none");
        create_branch(dir.path(), "feature");

        assert_eq!(
            upstream(&repository, "main").expect("the upstream should be readable"),
            None
        );
        assert_eq!(
            upstream(&repository, "feature").expect("the upstream should be readable"),
            None
        );
    }

    #[test]
    fn ahead_and_behind_are_counted_against_the_tracking_reference() {
        let (dir, _repository, _head) = repository_with_one_commit("read-ahead-behind");
        create_branch(dir.path(), "remote-side");
        commit(dir.path(), "local 1");
        commit(dir.path(), "local 2");

        git_in(dir.path(), &["switch", "--quiet", "remote-side"]);
        let remote_head = commit(dir.path(), "remote 1");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        create_remote_branch(dir.path(), "origin/main", &remote_head);

        let counts = ahead_behind(dir.path(), "main", "origin/main")
            .expect("the counts should be computed")
            .expect("origin/main exists");

        assert_eq!(counts, (2, 1), "the left side of the range is ahead");
    }

    #[test]
    fn a_branch_in_sync_with_its_tracking_reference_is_neither_ahead_nor_behind() {
        let (dir, _repository, head) = repository_with_one_commit("read-ahead-behind-sync");
        create_remote_branch(dir.path(), "origin/main", &head);

        let counts = ahead_behind(dir.path(), "main", "origin/main")
            .expect("the counts should be computed")
            .expect("origin/main exists");

        assert_eq!(counts, (0, 0));
    }

    #[test]
    fn a_missing_tracking_reference_yields_no_counts() {
        let (dir, _repository, _head) = repository_with_one_commit("read-ahead-behind-missing");

        let counts =
            ahead_behind(dir.path(), "main", "origin/main").expect("a missing ref is not an error");

        assert_eq!(counts, None);
    }

    #[test]
    fn ahead_behind_output_is_tab_separated() {
        // `git rev-list --left-right --count <local>...<remote>` は `<ahead>\t<behind>\n` を出す
        assert_eq!(
            parse_ahead_behind(b"3\t2\n").expect("the output should parse"),
            (3, 2)
        );
    }

    #[test]
    fn ahead_behind_output_parses_without_a_trailing_newline() {
        assert_eq!(
            parse_ahead_behind(b"0\t0").expect("the output should parse"),
            (0, 0)
        );
    }

    #[test]
    fn a_malformed_ahead_behind_output_is_rejected() {
        for output in [
            &b""[..],
            &b"3"[..],
            &b"3 2\n"[..],
            &b"3\t2\t1\n"[..],
            &b"3\t-2\n"[..],
            &b"a\tb\n"[..],
            &[0xff, b'\t', b'1'],
        ] {
            let err = parse_ahead_behind(output)
                .expect_err("a malformed output must not be silently accepted");

            assert!(
                matches!(err, Error::RepositoryReadFailed { .. }),
                "unexpected error for {output:?}: {err:?}"
            );
        }
    }

    #[test]
    fn the_current_branch_is_not_offered_as_a_merge_or_rebase_target() {
        let (dir, repository, head) = repository_with_one_commit("read-other-branches");
        create_branch(dir.path(), "feature");
        create_remote_branch(dir.path(), "origin/main", &head);

        let candidates = other_branches(&repository).expect("branches should be listed");

        assert_eq!(
            names(&candidates),
            ["feature", "origin/main"],
            "only the current branch is removed from the `--all` listing"
        );
    }

    #[test]
    fn a_detached_head_keeps_every_branch_as_a_target() {
        let (dir, _repository, head) = repository_with_one_commit("read-other-branches-detached");
        create_branch(dir.path(), "feature");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("repository should open");

        let candidates = other_branches(&repository).expect("branches should be listed");

        // detached HEAD ではどのブランチも「現在のブランチ」ではないため除外されない
        assert_eq!(names(&candidates), ["feature", "main"]);
    }

    #[test]
    fn the_commits_of_a_range_are_counted() {
        let (dir, _repository, _head) = repository_with_one_commit("read-commit-count");
        create_branch(dir.path(), "feature");
        git_in(dir.path(), &["switch", "--quiet", "feature"]);
        commit(dir.path(), "feature 1");
        commit(dir.path(), "feature 2");
        git_in(dir.path(), &["switch", "--quiet", "main"]);

        assert_eq!(
            commit_count(dir.path(), "HEAD..feature").expect("the range should be counted"),
            2,
            "the commits that would be merged are counted"
        );
        assert_eq!(
            commit_count(dir.path(), "feature..HEAD").expect("the range should be counted"),
            0,
            "the current branch has nothing the other branch lacks"
        );
    }

    #[test]
    fn a_commit_count_is_a_single_number() {
        assert_eq!(
            parse_commit_count(b"3\n").expect("the output should parse"),
            3
        );
        assert_eq!(
            parse_commit_count(b"0").expect("the output should parse"),
            0
        );
    }

    #[test]
    fn a_malformed_commit_count_is_rejected() {
        for output in [
            &b""[..],
            &b"-1\n"[..],
            &b"3 4\n"[..],
            &b"many\n"[..],
            &[0xff],
        ] {
            let err = parse_commit_count(output)
                .expect_err("a malformed output must not be silently accepted");

            assert!(
                matches!(err, Error::RepositoryReadFailed { .. }),
                "unexpected error for {output:?}: {err:?}"
            );
        }
    }

    /// テストリポジトリにリモートを登録する（fetch は行わない）。
    fn add_remote(path: &Path, name: &str) {
        git_in(
            path,
            &[
                "remote",
                "add",
                name,
                &format!("https://example.invalid/{name}.git"),
            ],
        );
    }

    #[test]
    fn a_push_target_is_generated_for_every_remote() {
        let (dir, _repository, _head) = repository_with_one_commit("read-push-targets");
        add_remote(dir.path(), "upstream");
        add_remote(dir.path(), "origin");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(PushTarget::tracking_name)
                .collect::<Vec<_>>(),
            ["origin/main", "upstream/main"],
            "every remote is combined with the current branch, in name order"
        );
        assert!(
            targets.iter().all(|target| target.branch == "main"),
            "the current branch is fixed as the push target: {targets:?}"
        );
    }

    #[test]
    fn a_repository_without_remotes_has_no_push_target() {
        let (_dir, repository, _head) = repository_with_one_commit("read-push-targets-empty");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert!(targets.is_empty(), "unexpected targets: {targets:?}");
    }

    #[test]
    fn only_the_configured_upstream_is_marked_among_the_targets() {
        let (dir, _repository, head) = repository_with_one_commit("read-push-targets-upstream");
        add_remote(dir.path(), "origin");
        add_remote(dir.path(), "backup");
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.tracking_name(), target.is_upstream))
                .collect::<Vec<_>>(),
            [
                ("backup/main".to_owned(), false),
                ("origin/main".to_owned(), true)
            ]
        );
    }

    #[test]
    fn a_push_target_carries_the_counts_against_its_tracking_reference() {
        let (dir, _repository, _head) = repository_with_one_commit("read-push-targets-counts");
        add_remote(dir.path(), "origin");
        add_remote(dir.path(), "backup");
        create_branch(dir.path(), "remote-side");
        commit(dir.path(), "local 1");
        commit(dir.path(), "local 2");
        git_in(dir.path(), &["switch", "--quiet", "remote-side"]);
        let remote_head = commit(dir.path(), "remote 1");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        create_remote_branch(dir.path(), "origin/main", &remote_head);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.tracking_name(), target.ahead_behind))
                .collect::<Vec<_>>(),
            [
                ("backup/main".to_owned(), None),
                ("origin/main".to_owned(), Some((2, 1)))
            ],
            "a remote without a tracking reference cannot be compared"
        );
    }

    #[test]
    fn a_push_target_of_a_branch_without_commits_has_no_counts() {
        let dir = TempDir::new("read-push-targets-unborn");
        init_repository(dir.path());
        add_remote(dir.path(), "origin");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.tracking_name(), target.ahead_behind))
                .collect::<Vec<_>>(),
            [("origin/main".to_owned(), None)]
        );
    }

    #[test]
    fn a_detached_head_has_no_push_target() {
        let (dir, _repository, head) = repository_with_one_commit("read-push-targets-detached");
        add_remote(dir.path(), "origin");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = push_targets(&repository)
            .expect_err("a detached HEAD gives no branch to push and must be reported");

        assert!(matches!(err, Error::DetachedHead), "unexpected: {err:?}");
    }

    #[test]
    fn a_tracking_reference_is_addressed_by_its_full_name() {
        let target = PushTarget {
            remote: "origin".to_owned(),
            branch: "feature/login".to_owned(),
            is_upstream: false,
            ahead_behind: None,
        };

        assert_eq!(target.tracking_name(), "origin/feature/login");
        assert_eq!(
            target.tracking_ref(),
            "refs/remotes/origin/feature/login",
            "the full name keeps a same-named local branch or tag from winning"
        );
    }

    #[test]
    fn a_target_matches_the_upstream_only_when_both_the_remote_and_the_reference_agree() {
        let upstream = Upstream {
            remote: "origin".to_owned(),
            merge_ref: "refs/heads/main".to_owned(),
        };

        assert!(targets_upstream(Some(&upstream), "origin", "main"));
        assert!(
            !targets_upstream(Some(&upstream), "backup", "main"),
            "another remote is a different destination"
        );
        assert!(
            !targets_upstream(Some(&upstream), "origin", "trunk"),
            "a branch tracking a differently named reference is a different destination"
        );
        assert!(!targets_upstream(None, "origin", "main"));
    }

    #[test]
    fn no_operation_is_in_progress_in_a_clean_repository() {
        let (_dir, repository, _head) = repository_with_one_commit("read-state-clean");

        assert_eq!(operation_in_progress(&repository), None);
    }

    #[test]
    fn a_merge_in_progress_is_detected() {
        let (dir, repository, head) = repository_with_one_commit("read-state-merge");
        write_file(dir.path(), ".git/MERGE_HEAD", &format!("{head}\n"));

        assert_eq!(operation_in_progress(&repository), Some(Operation::Merge));
    }

    #[test]
    fn a_rebase_in_progress_is_detected() {
        let (dir, repository, _head) = repository_with_one_commit("read-state-rebase");
        // 非対話の `git rebase` でも merge バックエンドは `rebase-merge/interactive` を作る
        write_file(dir.path(), ".git/rebase-merge/interactive", "");

        assert_eq!(operation_in_progress(&repository), Some(Operation::Rebase));
    }

    #[test]
    fn a_rebase_started_by_the_apply_backend_is_detected() {
        let (dir, repository, _head) = repository_with_one_commit("read-state-rebase-apply");
        write_file(dir.path(), ".git/rebase-apply/rebasing", "");

        assert_eq!(operation_in_progress(&repository), Some(Operation::Rebase));
    }

    #[test]
    fn an_operation_without_a_recovery_menu_is_not_reported() {
        // cherry-pick は fuzgit の復帰メニューの対象外（git の hint をそのまま見せる）
        let (dir, repository, head) = repository_with_one_commit("read-state-cherry-pick");
        write_file(dir.path(), ".git/CHERRY_PICK_HEAD", &format!("{head}\n"));

        assert_eq!(operation_in_progress(&repository), None);
    }

    #[test]
    fn unmerged_status_codes_are_recognised() {
        // git がマージ未解決として扱う 7 種類の状態コード
        for code in ["DD", "AU", "UD", "UA", "DU", "AA", "UU"] {
            assert!(
                change("conflicted.txt", code).is_unmerged(),
                "{code} must be treated as unmerged"
            );
        }
    }

    #[test]
    fn ordinary_status_codes_are_not_unmerged() {
        for code in [
            "M ", " M", "MM", "A ", " D", "D ", "??", "R ", "C ", "AM", "AD",
        ] {
            assert!(
                !change("plain.txt", code).is_unmerged(),
                "{code} must not be treated as unmerged"
            );
        }
    }

    #[test]
    fn unmerged_entries_are_extracted_from_a_conflicted_status() {
        let output = status_output(&[
            "UU both modified.txt",
            "AA both added.txt",
            "DD both deleted.txt",
            "M  staged.txt",
            "?? new.txt",
        ]);

        let changes = parse_status(&output).expect("status output should parse");
        let unmerged: Vec<&str> = changes
            .iter()
            .filter(|change| ChangeScope::Unmerged.includes(change))
            .map(|change| change.path.as_str())
            .collect();

        assert_eq!(
            unmerged,
            ["both modified.txt", "both added.txt", "both deleted.txt"]
        );
    }

    #[test]
    fn a_real_merge_conflict_is_reported_as_an_unmerged_change() {
        let dir = TempDir::new("read-unmerged-real");
        init_repository(dir.path());
        write_file(dir.path(), "shared.txt", "base\n");
        git_in(dir.path(), &["add", "--", "shared.txt"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "base"]);

        create_branch(dir.path(), "other");
        write_file(dir.path(), "shared.txt", "main side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "main side"]);

        git_in(dir.path(), &["switch", "--quiet", "other"]);
        write_file(dir.path(), "shared.txt", "other side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "other side"]);

        assert!(
            !try_git_in(dir.path(), &["merge", "--no-edit", "main"]),
            "the merge is expected to conflict"
        );

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        assert_eq!(operation_in_progress(&repository), Some(Operation::Merge));

        let unmerged =
            changes(&repository, ChangeScope::Unmerged).expect("the status should be read");

        assert_eq!(
            unmerged
                .iter()
                .map(|change| (change.path.as_str(), change.status_code()))
                .collect::<Vec<_>>(),
            [("shared.txt", "UU".to_string())]
        );
    }
}
