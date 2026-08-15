//! 単体テスト用のヘルパー。
//!
//! 一時ディレクトリ生成のためだけに依存クレートを追加しないよう、最小限の実装を持つ。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// テストリポジトリのコミット作者名。
const AUTHOR_NAME: &str = "fuzgit test";

/// テストリポジトリのコミット作者メールアドレス。
const AUTHOR_EMAIL: &str = "test@example.invalid";

/// テストリポジトリのコミット日時。
///
/// 日時の整形結果を実行マシンのタイムゾーンに依存させないため、オフセット付きで固定する。
const COMMIT_DATE: &str = "2024-01-02T03:04:05+00:00";

/// [`COMMIT_DATE`] を `%Y-%m-%d` 形式で表したもの。
pub const COMMIT_DATE_SHORT: &str = "2024-01-02";

/// コミットを積む対象のファイル名。
const HISTORY_FILE: &str = "history.txt";

/// テストごとに一意な一時ディレクトリ。Drop で再帰削除する。
pub struct TempDir {
    path: PathBuf,
}

/// 同一プロセス内でディレクトリ名が衝突しないようにするための連番。
static COUNTER: AtomicU32 = AtomicU32::new(0);

impl TempDir {
    /// `label` を含む一意な一時ディレクトリを作成する。
    pub fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fuzgit-{label}-{pid}-{unique}",
            pid = std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    /// 一時ディレクトリのパスを返す。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// テストリポジトリ上で `git` を実行し、標準出力（前後の空白を除去したもの）を返す。
///
/// 実行環境のユーザー設定・タイムゾーンに結果が左右されないよう、
/// グローバル/システム設定を無効化し、署名と日時を固定する。
pub fn git_in(directory: &Path, args: &[&str]) -> String {
    git_in_at(directory, args, COMMIT_DATE)
}

/// [`git_in`] と同じだが、コミット日時を明示的に指定する。
///
/// コミット日時順の並びを検証するテストで用いる。
fn git_in_at(directory: &Path, args: &[&str], date: &str) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));

    assert!(
        output.status.success(),
        "git {args:?} failed in {directory:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("git output should be utf-8")
        .trim()
        .to_string()
}

/// [`git_in`] と同じ設定で `git` を実行し、成功したかどうかだけを返す。
///
/// コンフリクトする merge のように、非ゼロ終了が想定内の操作に用いる。
pub fn try_git_in(directory: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
        .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
        .env("GIT_COMMITTER_NAME", AUTHOR_NAME)
        .env("GIT_COMMITTER_EMAIL", AUTHOR_EMAIL)
        .env("GIT_AUTHOR_DATE", COMMIT_DATE)
        .env("GIT_COMMITTER_DATE", COMMIT_DATE)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"))
        .status
        .success()
}

/// 指定ディレクトリに空の git リポジトリを作成する。
///
/// 既定ブランチ名は実行環境の `init.defaultBranch` に依存しないよう `main` に固定する。
pub fn init_repository(path: &Path) {
    git_in(path, &["init", "--quiet", "--initial-branch=main"]);
}

/// テストリポジトリに 1 件コミットし、そのフルハッシュを返す。
///
/// 空コミットを避けるため、メッセージを追記したファイルを一緒にコミットする。
pub fn commit(path: &Path, message: &str) -> String {
    commit_at(path, message, COMMIT_DATE)
}

/// [`commit`] と同じだが、コミット日時を明示的に指定する。
///
/// `date` は `2024-01-02T03:04:05+00:00` 形式で与える。
pub fn commit_at(path: &Path, message: &str, date: &str) -> String {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join(HISTORY_FILE))
        .expect("failed to open history file");
    writeln!(file, "{message}").expect("failed to write history file");
    drop(file);

    git_in_at(path, &["add", "--", HISTORY_FILE], date);
    git_in_at(path, &["commit", "--quiet", "-m", message], date);
    git_in_at(path, &["rev-parse", "HEAD"], date)
}

/// 指定ディレクトリに bare リポジトリ（作業ツリーを持たないリポジトリ）を作成する。
pub fn init_bare_repository(path: &Path) {
    git_in(
        path,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
}

/// テストリポジトリ内にファイルを作成・上書きする。
///
/// `relative` にはディレクトリ区切りを含めてよく、親ディレクトリは自動的に作成する。
pub fn write_file(path: &Path, relative: &str, contents: &str) {
    let file = path.join(relative);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(file, contents).expect("failed to write file");
}

/// 現在の HEAD からローカルブランチを作成する（切り替えは行わない）。
pub fn create_branch(path: &Path, name: &str) {
    git_in(path, &["branch", name]);
}

/// リモート追跡ブランチ（`refs/remotes/<name>`）を直接作成する。
///
/// 実際のリモートを用意せずに `--all` の候補生成を検証するために用いる。
pub fn create_remote_branch(path: &Path, name: &str, commit: &str) {
    git_in(
        path,
        &["update-ref", &format!("refs/remotes/{name}"), commit],
    );
}

/// lightweight tag（参照が対象を直接指すタグ）を現在の HEAD に作成する。
pub fn create_lightweight_tag(path: &Path, name: &str) {
    git_in(path, &["tag", name]);
}

/// annotated tag（タグオブジェクトを伴うタグ）を現在の HEAD に作成する。
pub fn create_annotated_tag(path: &Path, name: &str, message: &str) {
    git_in(path, &["tag", "--annotate", "--message", message, name]);
}

/// 作業ツリーの変更を stash へ退避する。
///
/// `message` を与えた場合は `On <branch>: <message>`、省略した場合は
/// `WIP on <branch>: <hash> <summary>` 形式のメッセージになる。
pub fn stash_changes(path: &Path, message: Option<&str>) {
    match message {
        Some(message) => git_in(path, &["stash", "push", "--quiet", "--message", message]),
        None => git_in(path, &["stash", "push", "--quiet"]),
    };
}

/// リモート追跡ブランチへのシンボリック参照（`origin/HEAD` 相当）を作成する。
pub fn create_remote_symbolic_ref(path: &Path, name: &str, target: &str) {
    git_in(
        path,
        &[
            "symbolic-ref",
            &format!("refs/remotes/{name}"),
            &format!("refs/remotes/{target}"),
        ],
    );
}

/// 文字列に日本語の文字が含まれるかどうかを判定する。
///
/// 表示言語ごとの文言（FR-27）を検査するために用いる。`en` を選んだ文言に日本語が
/// 混ざっていないことは判定できるが、逆（日本語の文言に英語が混ざらないこと）は
/// 判定しない。`git commit` のような**翻訳しない語**が日本語の文言にも意図的に
/// 含まれるためである。
pub fn contains_japanese(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(character,
            '\u{3000}'..='\u{303F}'   // 全角の記号・句読点
            | '\u{3040}'..='\u{309F}' // ひらがな
            | '\u{30A0}'..='\u{30FF}' // カタカナ
            | '\u{4E00}'..='\u{9FFF}' // 漢字
            | '\u{FF00}'..='\u{FFEF}' // 全角形
        )
    })
}
