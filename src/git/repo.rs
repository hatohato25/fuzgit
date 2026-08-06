//! `gix` によるリポジトリのオープン。

use std::path::Path;

use crate::error::{Error, Result};

/// 指定ディレクトリから親方向へ探索して git リポジトリを開く。
///
/// 通常は [`discover_from_current_dir`] 経由で呼び出す。ディレクトリを引数に取るのは
/// テストから任意のディレクトリを対象にできるようにするため。
///
/// # Errors
///
/// リポジトリが見つからない場合は [`Error::NotARepository`] を返す。
pub fn discover(directory: &Path) -> Result<gix::Repository> {
    gix::discover(directory).map_err(|source| Error::NotARepository {
        source: Box::new(source),
    })
}

/// カレントディレクトリから git リポジトリを開く。
///
/// # Errors
///
/// リポジトリが見つからない場合は [`Error::NotARepository`] を返す。
pub fn discover_from_current_dir() -> Result<gix::Repository> {
    discover(Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn discover_fails_outside_of_a_repository() {
        let dir = TempDir::new("repo-discover");

        let err = discover(dir.path()).expect_err("temp dir must not be a git repository");

        assert!(matches!(err, Error::NotARepository { .. }));
    }

    #[test]
    fn discover_succeeds_inside_a_repository() {
        let dir = TempDir::new("repo-discover-ok");
        crate::test_support::init_repository(dir.path());

        let repo = discover(dir.path()).expect("initialized repository must be discoverable");

        assert!(repo.git_dir().ends_with(".git"));
    }
}
