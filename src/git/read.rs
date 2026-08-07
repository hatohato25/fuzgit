//! リポジトリ情報の読み取り。
//!
//! ブランチ・コミット・タグ・reflog・変更ファイルの一覧を、`gix` の型を
//! `commands` 層へ漏らさないプレーンな構造体として返す。
//!
//! 基本は `gix` で読むが、作業ツリーの状態（`git status`）とツリーのファイル一覧
//! （`git ls-tree`）は `git` コマンドのキャプチャで取得する。gix の status 実装は
//! 設定（`status.renames` 等）の解釈まで含めると挙動互換の担保が難しく、
//! ここでは git 本体の判定をそのまま利用するほうが確実なため。

use gix::bstr::{BStr, ByteSlice as _};

use crate::error::{Error, Result};
use crate::git::exec::capture_git_in;
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
fn current_branch(repository: &gix::Repository) -> Result<Option<String>> {
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

/// 候補に含める変更ファイルの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeScope {
    /// ステージ済みの変更（`gz restore --staged` の対象）。
    Staged,
    /// 作業ツリーの変更（`gz restore` の対象）。
    Worktree,
    /// 未ステージの変更と未追跡ファイル（`gz add` の対象）。
    Stageable,
}

impl ChangeScope {
    /// 変更がこの範囲に含まれるかどうかを判定する。
    fn includes(self, change: &FileChange) -> bool {
        match self {
            ChangeScope::Staged => change.has_staged_change(),
            ChangeScope::Worktree => change.has_worktree_change(),
            ChangeScope::Stageable => change.is_untracked() || change.has_worktree_change(),
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

    /// `git status --porcelain` と同じ 2 文字の状態コード（`MM` / ` M` / `??` など）。
    #[must_use]
    pub fn status_code(&self) -> String {
        [self.index_status, self.worktree_status].iter().collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{
        COMMIT_DATE_SHORT, TempDir, commit, commit_at, create_branch, create_remote_branch,
        create_remote_symbolic_ref, git_in, init_repository, write_file,
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
}
