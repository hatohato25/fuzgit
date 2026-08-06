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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{
        COMMIT_DATE_SHORT, TempDir, commit, commit_at, create_branch, create_remote_branch,
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
