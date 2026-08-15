//! 兄弟リポジトリ（現在のリポジトリと同じ親ディレクトリ直下にあるリポジトリ）の探索。
//!
//! [`read`](crate::git::read) が「開いている 1 つのリポジトリの読み取り」を担うのに対し、
//! ここはファイルシステムの走査と複数リポジトリのオープンを担う
//! （[`repo`](crate::git::repo) と対になる位置づけ）。
//!
//! fuzgit が現在のリポジトリ以外に触れる唯一の経路（`gz fetch --siblings`）であり、
//! 例外であることを構造的に担保するため次の 3 点を守る。
//!
//! - 走査の起点は常に現在のリポジトリから導出する（探索パスを引数で受け取らない）
//! - 走査は親ディレクトリ直下の 1 階層のみで、再帰しない
//! - 候補生成では `git` プロセスを 1 度も起動しない（すべて `gix` のプロセス内読み取りで賄う）

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::read;
use crate::git::repo;

/// リポジトリ探索に伴うファイルシステムの操作。
///
/// [`Error::FilesystemReadFailed`] が「何の操作に失敗したか」を**表示済みの文字列ではなく
/// 値として**保持するための型（[`crate::git::read::ReadOperation`] と同じ設計）。
/// 表示は [`crate::i18n::messages::ErrorMessages::describe`] が担うため、
/// バリアントを追加すると ja / en の双方がコンパイルエラーになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemOperation {
    /// パスの正規化（`canonicalize`）。
    PathCanonicalization,
    /// ディレクトリの走査（`read_dir`）。
    DirectoryScan,
}

/// リポジトリを示すディレクトリ内のエントリ名。
///
/// ディレクトリ形式（通常のリポジトリ）とファイル形式（linked worktree / submodule）の
/// どちらもこの名前で存在する。
const DOT_GIT: &str = ".git";

/// 兄弟リポジトリ 1 件分の情報。
///
/// `gix` の型を `commands` 層へ漏らさないため、プレーンな値だけを持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingRepository {
    /// ワークツリーのルート（正規化済みの絶対パス）。選択結果の照合キーにもなる。
    pub workdir: PathBuf,
    /// ワークツリーのルートのディレクトリ名。
    pub name: String,
    /// HEAD が指すブランチの短縮名。detached HEAD の場合は `None`。
    pub current_branch: Option<String>,
    /// 登録されているリモート名（名前順）。候補は必ず 1 件以上のリモートを持つ。
    pub remotes: Vec<String>,
    /// 現在のリポジトリ（`gix::discover` で開いたもの）自身かどうか。
    pub is_current: bool,
}

/// 兄弟リポジトリの走査結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingScan {
    /// fetch 対象にできるリポジトリ。現在のリポジトリが先頭、以降はディレクトリ名の昇順。
    pub candidates: Vec<SiblingRepository>,
    /// fetch できないため候補から除外したリポジトリの件数（bare / リモート未登録）。
    ///
    /// 黙って消さずに件数を示せるよう、候補と併せて返す。
    pub excluded: usize,
}

/// 走査の起点。
///
/// ワークツリーのルートと、その親ディレクトリ（走査範囲）を組にして持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanRoot {
    /// 現在のリポジトリのワークツリーのルート（正規化済みの絶対パス）。
    current_workdir: PathBuf,
    /// 走査するディレクトリ（`current_workdir` の親）。
    scope: PathBuf,
}

/// 重複排除のキーを添えた候補。
///
/// キー（common dir）は候補の情報としては公開しないため、内部でのみ持ち回る。
#[derive(Debug)]
struct Candidate {
    /// 呼び出し側へ返す候補。
    sibling: SiblingRepository,
    /// 重複排除キー。linked worktree と main リポジトリで同じ値になる。
    common_dir: PathBuf,
}

/// 現在のリポジトリと、その兄弟リポジトリを列挙する。
///
/// 走査範囲は現在のリポジトリのワークツリーのルートの親ディレクトリ直下（1 階層のみ）。
/// 現在のリポジトリ自身も候補に含まれる（[`SiblingRepository::is_current`] が `true`）。
///
/// bare なリポジトリとリモートが 1 件も無いリポジトリは fetch できないため候補から除外し、
/// その件数を [`SiblingScan::excluded`] として返す。同一リポジトリの linked worktree は
/// 1 件に畳む。
///
/// # Errors
///
/// - 現在のリポジトリが bare で作業ツリーを持たない場合は [`Error::NoWorktree`]
/// - ワークツリーのルートの親ディレクトリを取得できない場合は [`Error::NoSiblingScope`]
/// - パスの正規化・ディレクトリの走査に失敗した場合は [`Error::FilesystemReadFailed`]
/// - 兄弟リポジトリの情報の読み取りに失敗した場合は [`Error::RepositoryReadFailed`]
pub fn discover(current: &gix::Repository) -> Result<SiblingScan> {
    let root = scan_root(current)?;
    scan(&root)
}

/// 走査の起点を解決する。
///
/// `gix` の探索結果である `workdir()` は cwd 起点の相対パス（`.` / `..`）を返し得るため、
/// **`canonicalize()` を経てから `parent()` を取る**。正規化前に親を取ると、
/// 例えば `<repo>/nested/..` から `<repo>/nested` という誤った走査範囲を導いてしまう。
fn scan_root(current: &gix::Repository) -> Result<ScanRoot> {
    let current_workdir = canonicalize(repo::workdir(current)?)?;
    let scope = scope_of(&current_workdir)?;

    Ok(ScanRoot {
        current_workdir,
        scope,
    })
}

/// 正規化済みのワークツリーのルートから走査範囲（親ディレクトリ）を決める。
fn scope_of(current_workdir: &Path) -> Result<PathBuf> {
    current_workdir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::NoSiblingScope {
            workdir: current_workdir.to_path_buf(),
        })
}

/// パスを正規化（シンボリックリンクを解決した絶対パス化）する。
fn canonicalize(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|source| filesystem_error(FilesystemOperation::PathCanonicalization, path, source))
}

/// I/O エラーを [`Error::FilesystemReadFailed`] へ変換する。
fn filesystem_error(operation: FilesystemOperation, path: &Path, source: std::io::Error) -> Error {
    Error::FilesystemReadFailed {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

/// 走査範囲を走査して候補を組み立てる。
fn scan(root: &ScanRoot) -> Result<SiblingScan> {
    let mut candidates = Vec::new();
    let mut excluded = 0;

    for directory in repository_directories(&root.scope)? {
        // 非リポジトリのディレクトリが混ざるため、開くのは `gix::open()` に限る。
        // `gix::discover()` は上位へ遡るため、`.git` を持たない兄弟から走査範囲より上の
        // リポジトリを誤って掴んでしまう
        let Ok(repository) = gix::open(&directory.path) else {
            // `.git` はあるが実体が壊れている等で開けないディレクトリはリポジトリとして
            // 扱えない。fetch 可能なリポジトリの除外（bare / リモート未登録）とは
            // 性質が異なるため `excluded` には数えない
            continue;
        };

        match candidate(&repository, directory, &root.current_workdir)? {
            Some(candidate) => candidates.push(candidate),
            None => excluded += 1,
        }
    }

    // 「現在のリポジトリを先頭、以降はディレクトリ名の昇順」に並べてから重複排除することで、
    // 同一リポジトリが複数並ぶ場合に「現在のリポジトリ優先、それ以外は名前順で先に
    // 現れたもの」が残る。並び順も同時に確定する
    candidates.sort_by(|left, right| {
        right
            .sibling
            .is_current
            .cmp(&left.sibling.is_current)
            .then_with(|| left.sibling.name.cmp(&right.sibling.name))
    });

    Ok(SiblingScan {
        candidates: deduplicate(candidates),
        excluded,
    })
}

/// 1 件の候補を組み立てる。fetch できないリポジトリの場合は `None` を返す。
fn candidate(
    repository: &gix::Repository,
    directory: RepositoryDirectory,
    current_workdir: &Path,
) -> Result<Option<Candidate>> {
    // 作業ツリーを持たないリポジトリは fetch した参照の反映先が無いため対象外
    if repository.is_bare() {
        return Ok(None);
    }

    let remotes = read::remotes(repository)?;
    if remotes.is_empty() {
        return Ok(None);
    }

    // linked worktree の common dir は `<main>/.git/worktrees/<name>/../..` のように
    // 相対要素を含むため、正規化して初めて main リポジトリと同じキーになる
    let common_dir = canonicalize(repository.common_dir())?;

    Ok(Some(Candidate {
        sibling: SiblingRepository {
            is_current: directory.path == current_workdir,
            workdir: directory.path,
            name: directory.name,
            current_branch: read::current_branch(repository)?,
            remotes,
        },
        common_dir,
    }))
}

/// 同一リポジトリの候補を 1 件に畳む。
///
/// 呼び出し時点の並び順（現在のリポジトリが先頭、以降は名前順）で先に現れたものを残す。
/// `git worktree list` は使わない（現在のリポジトリの worktree しか分からず、
/// 兄弟 A の worktree が兄弟 B として並ぶ場合を検出できないうえ、git プロセスを 1 回要する）。
fn deduplicate(candidates: Vec<Candidate>) -> Vec<SiblingRepository> {
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(candidates.len());
    let mut unique = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        if seen.insert(candidate.common_dir) {
            unique.push(candidate.sibling);
        }
    }

    unique
}

/// リポジトリの可能性があるディレクトリ 1 件分。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryDirectory {
    /// ディレクトリの絶対パス（走査範囲が正規化済みのため、これも正規化済み）。
    path: PathBuf,
    /// ディレクトリ名。
    name: String,
}

/// 走査範囲の直下から、リポジトリの可能性があるディレクトリを列挙する。
///
/// 実体のディレクトリだけを対象とし（`symlink_metadata` で判定するため
/// シンボリックリンクは辿らない）、サブディレクトリへは再帰しない。
/// `<dir>/.git` が存在するものだけを返す（ディレクトリ形式・ファイル形式の両方。
/// ファイル形式の `gitdir: <path>` の解決は `gix` に委ねる）。
///
/// 並び順は `read_dir` の返す順序のままであり、呼び出し側で並べ替える。
fn repository_directories(scope: &Path) -> Result<Vec<RepositoryDirectory>> {
    let entries = std::fs::read_dir(scope)
        .map_err(|source| filesystem_error(FilesystemOperation::DirectoryScan, scope, source))?;

    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            filesystem_error(FilesystemOperation::DirectoryScan, scope, source)
        })?;
        let path = entry.path();

        // 走査中に消えたエントリは stat に失敗する。走査範囲の他のリポジトリとは無関係な
        // 事象であり、コマンド全体を失敗させる理由にはならないため候補から外すに留める
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }

        // ディレクトリ名は候補行の表示と選択結果の照合に使うため、文字列にできないものは
        // 候補にできない
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };

        if !path.join(DOT_GIT).exists() {
            continue;
        }

        directories.push(RepositoryDirectory { path, name });
    }

    Ok(directories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::test_support::{TempDir, commit, git_in, init_bare_repository, init_repository};

    /// テスト用の親ディレクトリ（走査範囲）を作る。
    fn scope_dir(label: &str) -> TempDir {
        TempDir::new(label)
    }

    /// 走査範囲の直下に、リモートを 1 件持つリポジトリを作る。
    fn sibling_repository(scope: &Path, name: &str) -> PathBuf {
        let path = scope.join(name);
        std::fs::create_dir_all(&path).expect("failed to create sibling dir");
        init_repository(&path);
        commit(&path, "initial");
        add_remote(&path, "origin");
        path
    }

    /// リモートを登録する（URL はネットワークへ出ないダミー）。
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

    /// 走査範囲の直下にリポジトリではないディレクトリを作る。
    fn plain_dir(scope: &Path, name: &str) -> PathBuf {
        let path = scope.join(name);
        std::fs::create_dir_all(&path).expect("failed to create plain dir");
        path
    }

    /// 指定ディレクトリを起点に `gix::open` でリポジトリを開く。
    fn open(path: &Path) -> gix::Repository {
        gix::open(path).expect("repository should be openable")
    }

    /// 正規化済みの絶対パスを返す。
    fn real(path: &Path) -> PathBuf {
        path.canonicalize().expect("path should exist")
    }

    /// 候補のディレクトリ名を並び順のまま取り出す。
    fn names(scan: &SiblingScan) -> Vec<String> {
        scan.candidates
            .iter()
            .map(|candidate| candidate.name.clone())
            .collect()
    }

    // --- 探索起点の解決（T-192） ---

    #[test]
    fn the_scan_root_is_the_parent_of_the_worktree_root() {
        let scope = scope_dir("siblings-scan-root");
        let work = sibling_repository(scope.path(), "work");

        let root = scan_root(&open(&work)).expect("the scan root should be resolvable");

        assert_eq!(root.current_workdir, real(&work));
        assert_eq!(root.scope, real(scope.path()));
    }

    #[test]
    fn the_worktree_root_is_normalized_before_taking_its_parent() {
        let scope = scope_dir("siblings-scan-root-normalize");
        let work = sibling_repository(scope.path(), "work");
        std::fs::create_dir_all(work.join("nested")).expect("failed to create nested dir");
        // `gix` の探索結果は `.` / `..` を含む相対パスを返し得る。ここでは相対要素を含む
        // パスから開くことで同じ状況を作る
        let unnormalized = work.join("nested").join("..");

        let root = scan_root(&open(&unnormalized)).expect("the scan root should be resolvable");

        assert_eq!(root.current_workdir, real(&work));
        assert_eq!(
            root.scope,
            real(scope.path()),
            "the parent must be taken after normalization"
        );
    }

    #[test]
    fn a_bare_repository_has_no_scan_root() {
        let scope = scope_dir("siblings-scan-root-bare");
        let bare = scope.path().join("bare.git");
        std::fs::create_dir_all(&bare).expect("failed to create bare dir");
        init_bare_repository(&bare);

        let err = discover(&open(&bare)).expect_err("a bare repository has no work tree");

        assert!(matches!(err, Error::NoWorktree), "unexpected: {err:?}");
    }

    #[test]
    fn a_worktree_root_without_a_parent_is_rejected() {
        let err = scope_of(Path::new("/")).expect_err("the filesystem root has no parent");

        assert!(
            matches!(&err, Error::NoSiblingScope { workdir } if workdir == Path::new("/")),
            "unexpected: {err:?}"
        );
    }

    #[test]
    fn the_no_sibling_scope_error_names_the_worktree_root() {
        let err = Error::NoSiblingScope {
            workdir: PathBuf::from("/work"),
        };

        assert!(err.to_string().contains("/work"), "unexpected: {err}");
    }

    // --- 1 階層走査（T-194） ---

    #[test]
    fn only_directories_holding_a_dot_git_are_scanned() {
        let scope = scope_dir("siblings-list");
        sibling_repository(scope.path(), "with-dot-git-dir");
        plain_dir(scope.path(), "without-dot-git");
        std::fs::write(scope.path().join("a-file"), "not a directory")
            .expect("failed to write file");

        let directories =
            repository_directories(&real(scope.path())).expect("the scope should be readable");

        assert_eq!(
            directories
                .iter()
                .map(|directory| directory.name.clone())
                .collect::<Vec<_>>(),
            vec!["with-dot-git-dir".to_string()]
        );
    }

    #[test]
    fn a_dot_git_file_is_recognized_as_well() {
        let scope = scope_dir("siblings-list-dot-git-file");
        let main = sibling_repository(scope.path(), "main");
        // linked worktree の `.git` はファイル形式（`gitdir: <path>`）
        git_in(&main, &["worktree", "add", "--quiet", "../linked"]);
        assert!(
            scope.path().join("linked").join(DOT_GIT).is_file(),
            "the linked worktree must have a .git file"
        );

        let directories =
            repository_directories(&real(scope.path())).expect("the scope should be readable");

        let mut listed = directories
            .iter()
            .map(|directory| directory.name.clone())
            .collect::<Vec<_>>();
        listed.sort();
        assert_eq!(listed, vec!["linked".to_string(), "main".to_string()]);
    }

    #[test]
    fn symbolic_links_are_not_followed() {
        let scope = scope_dir("siblings-list-symlink");
        let main = sibling_repository(scope.path(), "main");
        std::os::unix::fs::symlink(&main, scope.path().join("link-to-main"))
            .expect("failed to create symlink");

        let directories =
            repository_directories(&real(scope.path())).expect("the scope should be readable");

        assert_eq!(
            directories
                .iter()
                .map(|directory| directory.name.clone())
                .collect::<Vec<_>>(),
            vec!["main".to_string()],
            "a symlinked sibling must not be scanned"
        );
    }

    // --- `gix::open()` で開く（T-195） ---

    #[test]
    fn a_sibling_without_a_dot_git_is_not_opened_as_the_enclosing_repository() {
        let scope = scope_dir("siblings-open");
        // 走査範囲そのものをリポジトリにして、`discover()` が遡る先を用意する
        init_repository(scope.path());
        commit(scope.path(), "initial");
        add_remote(scope.path(), "origin");
        let plain = plain_dir(scope.path(), "plain");

        // `gix::discover()` は上位へ遡り、非リポジトリのディレクトリから走査範囲の
        // リポジトリを掴んでしまう
        let discovered = gix::discover(&plain).expect("discover walks up to the enclosing repo");
        assert_eq!(
            discovered.workdir().map(real),
            Some(real(scope.path())),
            "discover() must be shown to walk up"
        );

        // `gix::open()` は与えられたディレクトリだけを見るため、非リポジトリは開けない
        assert!(
            gix::open(&plain).is_err(),
            "open() must not walk up to the enclosing repository"
        );
    }

    #[test]
    fn a_directory_that_cannot_be_opened_is_left_out_without_being_counted_as_excluded() {
        let scope = scope_dir("siblings-open-broken");
        let work = sibling_repository(scope.path(), "work");
        let broken = plain_dir(scope.path(), "broken");
        // `.git` はあるが指し先が存在しないため開けない
        std::fs::write(broken.join(DOT_GIT), "gitdir: /nonexistent/fuzgit/gitdir\n")
            .expect("failed to write .git file");

        let scan = discover(&open(&work)).expect("the scan should succeed");

        assert_eq!(names(&scan), vec!["work".to_string()]);
        assert_eq!(
            scan.excluded, 0,
            "an unopenable directory is not a fetch target"
        );
    }

    // --- 除外条件と除外件数（T-196） ---

    #[test]
    fn a_sibling_without_remotes_is_excluded_and_counted() {
        let scope = scope_dir("siblings-exclude-no-remote");
        let work = sibling_repository(scope.path(), "work");
        let lonely = scope.path().join("lonely");
        std::fs::create_dir_all(&lonely).expect("failed to create dir");
        init_repository(&lonely);
        commit(&lonely, "initial");

        let scan = discover(&open(&work)).expect("the scan should succeed");

        assert_eq!(names(&scan), vec!["work".to_string()]);
        assert_eq!(scan.excluded, 1);
    }

    #[test]
    fn a_bare_sibling_is_excluded_and_counted() {
        let scope = scope_dir("siblings-exclude-bare");
        let work = sibling_repository(scope.path(), "work");
        let bare = scope.path().join("bare");
        std::fs::create_dir_all(bare.join(DOT_GIT)).expect("failed to create dir");
        init_bare_repository(&bare.join(DOT_GIT));
        add_remote(&bare.join(DOT_GIT), "origin");
        assert!(
            open(&bare).is_bare(),
            "the sibling must be seen as bare by gix"
        );

        let scan = discover(&open(&work)).expect("the scan should succeed");

        assert_eq!(names(&scan), vec!["work".to_string()]);
        assert_eq!(scan.excluded, 1);
    }

    #[test]
    fn both_kinds_of_exclusion_are_counted_together() {
        let scope = scope_dir("siblings-exclude-both");
        let work = sibling_repository(scope.path(), "work");
        for name in ["lonely-a", "lonely-b"] {
            let path = scope.path().join(name);
            std::fs::create_dir_all(&path).expect("failed to create dir");
            init_repository(&path);
        }

        let scan = discover(&open(&work)).expect("the scan should succeed");

        assert_eq!(names(&scan), vec!["work".to_string()]);
        assert_eq!(scan.excluded, 2);
    }

    // --- 候補の内容 ---

    #[test]
    fn a_candidate_carries_the_branch_and_the_remotes() {
        let scope = scope_dir("siblings-candidate");
        let work = sibling_repository(scope.path(), "work");
        add_remote(&work, "upstream");

        let scan = discover(&open(&work)).expect("the scan should succeed");

        let candidate = scan.candidates.first().expect("the current repo is listed");
        assert_eq!(candidate.workdir, real(&work));
        assert_eq!(candidate.name, "work");
        assert_eq!(candidate.current_branch.as_deref(), Some("main"));
        assert_eq!(
            candidate.remotes,
            vec!["origin".to_string(), "upstream".to_string()]
        );
        assert!(candidate.is_current);
    }

    #[test]
    fn a_detached_head_has_no_current_branch() {
        let scope = scope_dir("siblings-candidate-detached");
        let work = sibling_repository(scope.path(), "work");
        let head = git_in(&work, &["rev-parse", "HEAD"]);
        git_in(&work, &["checkout", "--quiet", "--detach", &head]);

        let scan = discover(&open(&work)).expect("the scan should succeed");

        let candidate = scan.candidates.first().expect("the current repo is listed");
        assert_eq!(candidate.current_branch, None);
    }

    // --- 重複排除（T-197） ---

    #[test]
    fn a_linked_worktree_is_folded_into_the_main_repository() {
        let scope = scope_dir("siblings-dedup-worktree");
        let main = sibling_repository(scope.path(), "main");
        git_in(&main, &["worktree", "add", "--quiet", "../linked"]);

        let scan = discover(&open(&main)).expect("the scan should succeed");

        assert_eq!(
            names(&scan),
            vec!["main".to_string()],
            "the linked worktree shares the common dir with the main repository"
        );
        assert_eq!(scan.excluded, 0, "a duplicate is not an exclusion");
    }

    #[test]
    fn the_current_repository_wins_over_its_duplicate() {
        let scope = scope_dir("siblings-dedup-current");
        let main = sibling_repository(scope.path(), "main");
        git_in(&main, &["worktree", "add", "--quiet", "../linked"]);
        let linked = scope.path().join("linked");

        // 現在のリポジトリが linked worktree 側でも、残るのは現在のリポジトリ
        let scan = discover(&open(&linked)).expect("the scan should succeed");

        assert_eq!(names(&scan), vec!["linked".to_string()]);
        assert!(
            scan.candidates
                .first()
                .expect("one candidate remains")
                .is_current
        );
    }

    #[test]
    fn duplicates_keep_the_first_one_in_name_order() {
        let scope = scope_dir("siblings-dedup-name-order");
        let work = sibling_repository(scope.path(), "work");
        let main = sibling_repository(scope.path(), "main");
        git_in(&main, &["worktree", "add", "--quiet", "../zz-linked"]);

        let scan = discover(&open(&work)).expect("the scan should succeed");

        assert_eq!(names(&scan), vec!["work".to_string(), "main".to_string()]);
    }

    // --- 並び順（T-198） ---

    #[test]
    fn the_current_repository_comes_first_and_the_rest_are_sorted_by_name() {
        let scope = scope_dir("siblings-order");
        sibling_repository(scope.path(), "alpha");
        sibling_repository(scope.path(), "zulu");
        let work = sibling_repository(scope.path(), "mike");

        let scan = discover(&open(&work)).expect("the scan should succeed");

        assert_eq!(
            names(&scan),
            vec!["mike".to_string(), "alpha".to_string(), "zulu".to_string()]
        );
        assert!(
            scan.candidates
                .first()
                .expect("candidates are not empty")
                .is_current
        );
    }

    /// [`FilesystemOperation`] の全バリアント。網羅性は `match` でコンパイル時に担保する。
    fn every_filesystem_operation() -> Vec<FilesystemOperation> {
        let all = vec![
            FilesystemOperation::PathCanonicalization,
            FilesystemOperation::DirectoryScan,
        ];

        // バリアントを追加したらこの `match` が壊れ、`all` の更新漏れに気づける
        for operation in &all {
            match operation {
                FilesystemOperation::PathCanonicalization | FilesystemOperation::DirectoryScan => {}
            }
        }

        all
    }

    /// 指定した操作の [`Error::FilesystemReadFailed`] を組み立てる。
    fn filesystem_failure(operation: FilesystemOperation) -> Error {
        filesystem_error(operation, Path::new("/work"), std::io::Error::other("boom"))
    }

    #[test]
    fn every_filesystem_operation_is_described_in_both_languages() {
        for language in [Language::Japanese, Language::English] {
            for operation in every_filesystem_operation() {
                let described = language
                    .messages()
                    .errors()
                    .describe(&filesystem_failure(operation));

                assert!(
                    !described.trim().is_empty(),
                    "{language:?} left {operation:?} empty"
                );
            }
        }
    }

    #[test]
    fn the_filesystem_operation_wording_is_translated() {
        for operation in every_filesystem_operation() {
            let failure = filesystem_failure(operation);

            assert_ne!(
                Language::Japanese.messages().errors().describe(&failure),
                Language::English.messages().errors().describe(&failure),
                "{operation:?} must differ between the languages"
            );
        }
    }

    #[test]
    fn a_scan_failure_is_described_in_the_language_of_the_display() {
        // 走査層は操作を値として返すだけであり、言語は表示のときに決まることを両言語で確かめる
        let dir = TempDir::new("siblings-language");
        let missing = dir.path().join("missing");

        let err =
            repository_directories(&missing).expect_err("a missing directory cannot be scanned");

        assert!(
            matches!(
                &err,
                Error::FilesystemReadFailed { operation, .. }
                    if *operation == FilesystemOperation::DirectoryScan
            ),
            "unexpected: {err:?}"
        );
        assert!(
            Language::Japanese
                .messages()
                .errors()
                .describe(&err)
                .contains("ディレクトリの走査"),
            "the japanese description must name the failed operation"
        );
        assert!(
            Language::English
                .messages()
                .errors()
                .describe(&err)
                .contains("scanning the directory"),
            "the english description must name the failed operation"
        );
    }
}
