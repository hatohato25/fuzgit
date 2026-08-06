//! `gix` によるリポジトリ情報の読み取り。
//!
//! ブランチ・コミット・タグ・reflog・変更ファイルの一覧を、`gix` の型を
//! `commands` 層へ漏らさないプレーンな構造体として返す。

use gix::bstr::{BStr, ByteSlice as _};

use crate::error::{Error, Result};

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
/// 参照の列挙・読み取りに失敗した場合、または参照名が UTF-8 でない場合に
/// [`Error::RepositoryReadFailed`] を返す。
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
    Ok(locals)
}

/// HEAD から辿れるコミットを新しい順に最大 `limit` 件取得する。
///
/// コミットが 1 件も無いリポジトリ（unborn HEAD）では空の `Vec` を返す。
///
/// # Errors
///
/// HEAD の解決・履歴の走査・コミットの復号に失敗した場合に
/// [`Error::RepositoryReadFailed`] を返す。
pub fn commits(repository: &gix::Repository, limit: usize) -> Result<Vec<CommitInfo>> {
    let mut head = repository
        .head()
        .map_err(|source| read_error("HEAD の読み取り", source))?;

    let Some(head_id) = head
        .try_peel_to_id()
        .map_err(|source| read_error("HEAD の解決", source))?
    else {
        return Ok(Vec::new());
    };

    let walk = head_id
        .ancestors()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{
        COMMIT_DATE_SHORT, TempDir, commit, create_branch, create_remote_branch,
        create_remote_symbolic_ref, git_in, init_repository,
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
    fn a_repository_without_commits_has_no_branches() {
        let dir = TempDir::new("read-unborn-branches");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let branches = branches(&repository, BranchScope::All).expect("branches should be read");

        assert!(branches.is_empty(), "unexpected branches: {branches:?}");
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

        let commits = commits(&repository, 10).expect("commits should be read");

        let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["third", "second", "first"]);
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

        let commits = commits(&repository, 2).expect("commits should be read");

        let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["third", "second"]);
    }

    #[test]
    fn a_limit_of_zero_yields_no_commits() {
        let (_dir, repository, _head) = repository_with_one_commit("read-commits-zero");

        let commits = commits(&repository, 0).expect("commits should be read");

        assert!(commits.is_empty(), "unexpected commits: {commits:?}");
    }

    #[test]
    fn a_repository_without_commits_yields_no_commits() {
        let dir = TempDir::new("read-commits-unborn");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits = commits(&repository, 10).expect("unborn HEAD should not be an error");

        assert!(commits.is_empty(), "unexpected commits: {commits:?}");
    }

    #[test]
    fn commit_metadata_is_exposed_without_gix_types() {
        let (_dir, repository, head) = repository_with_one_commit("read-commit-metadata");

        let commits = commits(&repository, 1).expect("commits should be read");
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
