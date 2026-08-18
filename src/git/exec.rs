//! システムの `git` コマンド実行ラッパー。
//!
//! すべての呼び出しは `Command::new("git").args(...)` の引数配列渡しで行い、
//! シェル（`sh -c` 等）を一切経由しない。これによりブランチ名・パス・検索クエリ等の
//! ユーザー由来データによるシェルインジェクションを構造的に排除する。
//!
//! # 実行の 2 系統（(A) / (B)。FR-26）
//!
//! git の実行は「出力を誰が読むか」で 2 つに分かれ、子プロセスへ渡すロケール環境変数が
//! 異なる（[`LocaleIntent`]）。
//!
//! | 系統 | 出力を読むのは | ロケール | 該当する関数 |
//! |---|---|---|---|
//! | **(A)** | fuzgit（パースする） | `LC_MESSAGES=C` に固定 | [`capture_git`] / [`capture_git_in`] / [`capture_git_with_status_in`] |
//! | **(B)** | ユーザー（端末・プレビューで読む） | 解決された表示言語を伝播 | [`run_git`] / [`run_git_in`] / [`capture_git_stderr_in`] / [`capture_git_display`] / [`capture_git_display_in`] |
//!
//! **不変条件: `language: Language` を引数に取る関数が (B)、取らない関数が (A)。**
//! 分類を命名規約ではなく型で表現しているため、新しい呼び出しを書くときは
//! どちらの系統かを選ばざるを得ない（`Language` を渡す手段が無ければ (A) しか使えない）。
//!
//! # デバッグログ
//!
//! 環境変数 `FUZGIT_DEBUG=1` を指定すると、このモジュールが実行する git コマンドを
//! 標準エラーへ出力する。行には (A)/(B) の分類と、設定したロケール関連の環境変数を併記する。
//! ロギングクレートは導入せず、ここだけの軽量な実装で済ませる。

use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};
use crate::i18n::Language;

/// デバッグログを有効にする環境変数名。
const DEBUG_ENV: &str = "FUZGIT_DEBUG";

/// デバッグログを有効にする値。この値と完全に一致する場合のみ出力する。
const DEBUG_ENABLED_VALUE: &str = "1";

/// デバッグログの各行に付ける接頭辞。git 自身の出力と区別するために付ける。
///
/// `crate::i18n::resolve` の言語解決ログもこの接頭辞を共有する（同じ `FUZGIT_DEBUG=1` で
/// 出る行の見た目を揃えるため、定義を 2 か所に持たない）。
pub(crate) const DEBUG_PREFIX: &str = "[fuzgit]";

/// すべてのロケールカテゴリを一括で上書きする環境変数名。
const LC_ALL_ENV: &str = "LC_ALL";

/// メッセージ言語を決めるロケールカテゴリの環境変数名。
const LC_MESSAGES_ENV: &str = "LC_MESSAGES";

/// GNU gettext が参照する言語優先リストの環境変数名。
const LANGUAGE_ENV: &str = "LANGUAGE";

/// [`LC_ALL_ENV`] を等価な個別カテゴリへ展開するときの対象キー。
///
/// POSIX（`man 7 locale`）が定めるカテゴリのうち、環境変数として設定できるのはこの 6 つ
/// （`LC_ALL` 自身を除く）。1 つでも漏らすと `LC_ALL` を外した瞬間にそのカテゴリだけ
/// 親環境と異なる挙動になるため、網羅していることが重要（調査 T-263）。
const LOCALE_CATEGORY_ENVS: [&str; 6] = [
    "LC_CTYPE",
    "LC_COLLATE",
    LC_MESSAGES_ENV,
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];

/// (A) 系でメッセージ言語を固定するロケール名。
///
/// POSIX ロケールであり、どの環境にも必ず存在する（存在しないロケール名を推測して
/// 設定する形にはしない）。
const FIXED_MESSAGES_LOCALE: &str = "C";

/// デバッグログで「削除した」ことを示す値の表記。
const REMOVED_DISPLAY: &str = "(unset)";

/// git 自身の端末プロンプト（`Username for …`）の可否を決める環境変数。
const GIT_TERMINAL_PROMPT_ENV: &str = "GIT_TERMINAL_PROMPT";

/// 資格情報を訊くヘルパを指定する環境変数。
const GIT_ASKPASS_ENV: &str = "GIT_ASKPASS";

/// git が ssh を起動する際のコマンドを指定する環境変数。
const GIT_SSH_COMMAND_ENV: &str = "GIT_SSH_COMMAND";

/// [`GIT_TERMINAL_PROMPT_ENV`] に与えて端末プロンプトを禁止する値。
const TERMINAL_PROMPT_DISABLED: &str = "0";

/// [`GIT_SSH_COMMAND_ENV`] が未設定のときに使う ssh コマンド。
const DEFAULT_SSH_COMMAND: &str = "ssh";

/// ssh へ対話を禁じるオプション。passphrase や未知のホストの確認を待たずに失敗させる。
const SSH_BATCH_MODE_OPTION: &str = "-o BatchMode=yes";

/// 子プロセス `git` のメッセージ言語をどう扱うか（FR-26 の (A)/(B) 分類）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocaleIntent {
    /// (A) fuzgit が出力をパースする実行。メッセージ言語をロケール非依存に固定する。
    Fixed,
    /// (B) ユーザーが出力を読む実行。解決された表示言語を子プロセスへ伝える。
    Display(Language),
}

impl LocaleIntent {
    /// デバッグログに出す分類の表記。
    fn label(self) -> &'static str {
        match self {
            Self::Fixed => "(A)",
            Self::Display(_) => "(B)",
        }
    }
}

/// 子プロセスへ適用するロケール関連の環境変数の変更一式。
///
/// 変更を「組み立てる」ことと「適用する」ことを分けているのは、組み立てを純関数
/// （[`locale_environment`]）にして単体テストできるようにするためと、適用した内容を
/// そのままデバッグログへ出せるようにするため。
///
/// **`Command::env_clear()` は使わない。**`PATH` / `SSH_AUTH_SOCK` / `GIT_*` を失うと
/// 認証と git 本体の動作が壊れるため、操作するキーはここに並ぶものだけに限定する。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LocaleEnvironment {
    /// 子プロセスの環境から取り除くキー。
    removed: Vec<&'static str>,
    /// 子プロセスの環境へ設定するキーと値（キーの重複は持たない）。
    assigned: Vec<(&'static str, OsString)>,
}

impl LocaleEnvironment {
    /// キーに値を設定する。同じキーが既にあれば上書きする（後勝ち）。
    ///
    /// 上書きにするのは、`LC_ALL` の等価変換が設定した `LC_MESSAGES` を
    /// [`LocaleIntent::Fixed`] が `C` へ差し替える場合に、最終的な値だけが
    /// 残るようにするため（ログにも最終値だけが出る）。
    fn assign(&mut self, key: &'static str, value: impl Into<OsString>) {
        let value = value.into();
        match self.assigned.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.assigned.push((key, value)),
        }
    }

    /// 組み立てた変更を子プロセスの環境へ反映する。
    fn apply(&self, command: &mut Command) {
        for key in &self.removed {
            command.env_remove(key);
        }
        for (key, value) in &self.assigned {
            command.env(key, value);
        }
    }

    /// デバッグログ用に変更内容を 1 行へ整形する。
    ///
    /// 削除したキーは `LC_ALL=(unset)` の形で示す。値は表示専用であり、
    /// この文字列が環境へ戻ることはない。
    fn describe(&self) -> String {
        let removed = self
            .removed
            .iter()
            .map(|key| format!("{key}={REMOVED_DISPLAY}"));
        let assigned = self
            .assigned
            .iter()
            .map(|(key, value)| format!("{key}={value}", value = value.to_string_lossy()));

        removed.chain(assigned).collect::<Vec<String>>().join(" ")
    }
}

/// 子プロセスへ適用するロケール環境を組み立てる（純関数）。
///
/// `lc_all` には**親プロセスの** `LC_ALL` の値を渡す。値を引数で受け取るのは、
/// `std::env` に依存せず単体テストできるようにするため（[`is_debug_enabled`] と同じ理由）。
///
/// 1. **等価変換**: `LC_ALL=V` があれば `LC_ALL` を削除し、`LC_CTYPE` /
///    `LC_COLLATE` / `LC_MESSAGES` / `LC_MONETARY` / `LC_NUMERIC` / `LC_TIME` を
///    すべて `V` で上書きする。`LC_ALL` は個別カテゴリより強いため、これを外さないと
///    後続の `LC_MESSAGES` 指定が一切効かない（実測で確認済み）。カテゴリを転記するのは
///    `LC_ALL` を外したことで文字種・照合順序まで変えてしまわないようにするため
/// 2. [`LocaleIntent::Fixed`]: **`LC_MESSAGES=C` だけを設定する**。
///    `LANGUAGE` には触れない。`LC_MESSAGES=C` のとき GNU gettext は `LANGUAGE` を
///    無視し、かつ `LANGUAGE=""` は「未設定」として扱われ英語固定の効果を持たないため
///    （いずれも調査 T-261 の実測。空文字を設定しても意味が無い）
/// 3. [`LocaleIntent::Display`]: `LANGUAGE=<ja|en>` を設定する。**`LC_MESSAGES` は
///    書き換えない**。存在しないロケール名（`ja_JP.UTF-8` 等）を推測して設定すると、
///    その環境にデータが無ければ黙って `C` へ落ちる＝暗黙のフォールバックになるため。
///    **既知の制約**: 親環境の実効ロケールが `C` / `POSIX` のときは `LANGUAGE` が
///    無視される。ただし対象言語は `ja`（git に `ja` のカタログが無く英語になる）と
///    `en`（英語）であり、どちらも結果は英語で一致するため実害は無い
fn locale_environment(intent: LocaleIntent, lc_all: Option<&OsStr>) -> LocaleEnvironment {
    let mut environment = LocaleEnvironment::default();

    if let Some(value) = lc_all {
        environment.removed.push(LC_ALL_ENV);
        for key in LOCALE_CATEGORY_ENVS {
            environment.assign(key, value);
        }
    }

    match intent {
        LocaleIntent::Fixed => environment.assign(LC_MESSAGES_ENV, FIXED_MESSAGES_LOCALE),
        LocaleIntent::Display(language) => environment.assign(LANGUAGE_ENV, language.code()),
    }

    environment
}

/// これから実行する `git` へロケール環境を適用し、適用した内容を返す。
///
/// 戻り値はデバッグログ（[`debug_line`]）で「何を設定したか」を示すために使う。
fn apply_locale(command: &mut Command, intent: LocaleIntent) -> LocaleEnvironment {
    let environment = locale_environment(intent, std::env::var_os(LC_ALL_ENV).as_deref());
    environment.apply(command);
    environment
}

/// 並列フェーズで子プロセスへ追加設定する環境変数を組み立てる（純関数）。
///
/// `ssh_command` には**親プロセスの** `GIT_SSH_COMMAND` の値を渡す。値を引数で受け取るのは
/// `std::env` に依存せず単体テストできるようにするため（[`locale_environment`] と同じ理由）。
///
/// 並列実行では複数の `git` の出力が同じ端末に混ざるため、認証プロンプトが出ても
/// 「どのリポジトリが訊いているのか」を示せない。そこで並列フェーズでは対話を
/// **構造的に禁止**し、対話が必要な対象は失敗させて直列フェーズで実行し直す。
/// 以下はいずれも実測で確認した挙動（`.claude/tasks.md` T-366 / T-367）。
///
/// - `GIT_TERMINAL_PROMPT=0`: git 自身の端末プロンプトを止める
/// - `GIT_ASKPASS=""`: askpass ヘルパの起動を止める。空文字は `core.askPass` と
///   `SSH_ASKPASS` の**両方へのフォールバックも塞ぐ**
/// - `GIT_SSH_COMMAND`: ssh は passphrase を**標準入力ではなく `/dev/tty` から読む**ため、
///   `stdin` を閉じるだけでは防げない。`-o BatchMode=yes` を付けると実 pty 配下でも
///   端末へ何も書かずに即座に失敗する。親環境に値があればそれへ追記し、利用者の ssh 設定を
///   捨てない
///
/// **上の 2 つはどちらか一方では足りない。**`GIT_TERMINAL_PROMPT=0` だけでは askpass が
/// 起動し、`GIT_ASKPASS=""` だけでは端末へプロンプトが書き出される。
///
/// 親環境の `GIT_SSH_COMMAND` が**空文字の場合は「未設定」として扱う**。そのまま追記すると
/// 先頭にコマンド名の無い `" -o BatchMode=yes"` になってしまうため
/// （空文字を未設定とみなすのは `FUZGIT_LANG` / `fuzgit.lang` と同じ流儀）。
///
/// **`Command::env_clear()` は使わない**（[`LocaleEnvironment`] と同じ理由）。設定するのは
/// ここに並ぶ 3 つだけであり、直列フェーズや他の実行経路の環境は一切変えない。
fn noninteractive_environment(ssh_command: Option<&OsStr>) -> Vec<(&'static str, OsString)> {
    let ssh = match ssh_command.filter(|value| !value.is_empty()) {
        Some(inherited) => {
            let mut combined = inherited.to_owned();
            combined.push(" ");
            combined.push(SSH_BATCH_MODE_OPTION);
            combined
        }
        None => OsString::from(format!("{DEFAULT_SSH_COMMAND} {SSH_BATCH_MODE_OPTION}")),
    };

    vec![
        (
            GIT_TERMINAL_PROMPT_ENV,
            OsString::from(TERMINAL_PROMPT_DISABLED),
        ),
        (GIT_ASKPASS_ENV, OsString::new()),
        (GIT_SSH_COMMAND_ENV, ssh),
    ]
}

/// エラーメッセージ表示用に引数列を連結する。
///
/// あくまで表示専用であり、この文字列をコマンドとして実行することはない。
fn display_args(args: &[&str]) -> String {
    args.join(" ")
}

/// エラーメッセージ表示用のコマンド名（`git commit` 等）を組み立てる。
///
/// 継承 stdio 実行では git 自身のメッセージが既に端末へ出ているため、引数を全て並べると
/// pathspec の列で本当の原因が埋もれる。そこでサブコマンド名までに切り詰める。
/// オプションや対象（リビジョン・パス）は、呼び出し側が `anyhow` の文脈で補う。
fn command_display(args: &[&str]) -> String {
    match args.first() {
        Some(subcommand) => format!("git {subcommand}"),
        None => "git".to_owned(),
    }
}

/// 環境変数の値からデバッグログの有効・無効を判定する。
///
/// 有効とみなすのは値が厳密に `1` の場合だけで、未設定（`None`）・空文字・`0`・`true` などは
/// すべて無効とする。「設定されていれば何でも有効」にすると `FUZGIT_DEBUG=0` を無効化のつもりで
/// 指定したときに挙動が食い違うため、判定基準を 1 つに固定する。
///
/// 値を引数で受け取るのは、`std::env::set_var` に依存せず単体テストできるようにするため
/// （テストは並列実行されるため、プロセス全体の環境変数を書き換えると他のテストと干渉する）。
fn is_debug_enabled(value: Option<&str>) -> bool {
    value == Some(DEBUG_ENABLED_VALUE)
}

/// 現在のプロセスの環境変数からデバッグログの有効・無効を判定する。
///
/// 値が UTF-8 でない場合は [`DEBUG_ENABLED_VALUE`] と一致し得ないため無効とする。
///
/// `crate::i18n::resolve` の言語解決ログも判定をここへ委ねる（`FUZGIT_DEBUG` の
/// 解釈を [`is_debug_enabled`] の 1 か所に保つため）。
pub(crate) fn debug_enabled() -> bool {
    is_debug_enabled(std::env::var(DEBUG_ENV).ok().as_deref())
}

/// デバッグログに出力する 1 行を組み立てる。
///
/// 実際に実行する引数配列をそのまま並べる（表示専用であり、この文字列を実行することはない）。
/// 出力されるのは git のサブコマンド・リビジョン・パスだけであり、認証情報の類は含まれない。
///
/// 行頭の `(A)` / `(B)` はその実行の分類（FR-26）、末尾の角括弧は
/// [`apply_locale`] が設定・削除したロケール関連の環境変数。どちらも
/// 「なぜこの言語で出たのか」を追えるようにするために併記する。
fn debug_line(
    directory: Option<&Path>,
    args: &[&str],
    intent: LocaleIntent,
    locale: &LocaleEnvironment,
) -> String {
    let location = match directory {
        Some(directory) => format!(" (cwd: {directory})", directory = directory.display()),
        None => String::new(),
    };

    format!(
        "{DEBUG_PREFIX} {label}{location} git {args} [{locale}]",
        label = intent.label(),
        args = display_args(args),
        locale = locale.describe()
    )
}

/// これから実行する git コマンドをデバッグログとして標準エラーへ出力する。
///
/// 標準出力はハッシュ・タグ名のパイプ用途に使うため、ログは必ず標準エラーへ出す。
fn log_command(
    directory: Option<&Path>,
    args: &[&str],
    intent: LocaleIntent,
    locale: &LocaleEnvironment,
) {
    if !debug_enabled() {
        return;
    }

    // ログの書き込み失敗（標準エラーの閉鎖など）で git の実行そのものを止めたくないため、
    // ここだけは結果を破棄する。デバッグ出力は本来の処理に影響を与えない
    let _ = writeln!(
        std::io::stderr(),
        "{}",
        debug_line(directory, args, intent, locale)
    );
}

/// `git` を起動する [`Command`] を組み立て、ロケール環境の適用とデバッグログ出力を行う。
///
/// 実行方法（継承 stdio / 出力キャプチャ）に依らず共通する前処理をここへ集約することで、
/// **ロケールを適用しない実行経路が生まれないようにする**。
fn build_command(directory: Option<&Path>, args: &[&str], intent: LocaleIntent) -> Command {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }

    let locale = apply_locale(&mut command, intent);
    log_command(directory, args, intent, &locale);

    command
}

/// `git` の起動失敗を、原因に応じたドメインエラーへ変換する。
fn map_spawn_error(source: std::io::Error, args: &[&str]) -> Error {
    if source.kind() == ErrorKind::NotFound {
        Error::GitNotFound
    } else {
        Error::GitSpawnFailed {
            args: display_args(args),
            source,
        }
    }
}

/// `git` を標準入出力を継承したまま実行する（**(B) 系**）。
///
/// `switch` / `cherry-pick` 等の書き込み系操作に用いる。git 自身の出力（進捗・
/// コンフリクト内容など）をそのままユーザーへ見せたいため、出力はキャプチャせず、
/// `language` で解決された表示言語を子プロセスへ伝播する。
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - 非ゼロ終了した場合は [`Error::GitRunFailed`]（失敗の詳細は git が端末へ直接出力済み）
pub fn run_git(language: Language, args: &[&str]) -> Result<()> {
    run(None, args, LocaleIntent::Display(language))
}

/// [`run_git`] と同じだが、`directory` をカレントディレクトリとして実行する（**(B) 系**）。
///
/// 継承 stdio のまま実行ディレクトリだけを差し替えるためのもので、[`capture_git_in`] と対になる。
/// 現在のリポジトリ以外に対して書き込み系操作を行う経路（`gz fetch --siblings`）で用いる。
/// 認証プロンプト・進捗・更新された参照の一覧は [`run_git`] と同じく git に委ねる。
///
/// # Errors
///
/// [`run_git`] と同じ。
pub fn run_git_in(language: Language, directory: &Path, args: &[&str]) -> Result<()> {
    run(Some(directory), args, LocaleIntent::Display(language))
}

/// `git` を標準入出力を継承したまま実行する。`directory` 指定時はそこを作業ディレクトリとする。
fn run(directory: Option<&Path>, args: &[&str], intent: LocaleIntent) -> Result<()> {
    let status = build_command(directory, args, intent)
        .status()
        .map_err(|source| map_spawn_error(source, args))?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::GitRunFailed {
            command: command_display(args),
            code: status.code(),
        })
    }
}

/// `git` を実行し、標準出力をバイト列としてキャプチャする（**(A) 系**）。
///
/// `git status --porcelain` のように **fuzgit がパースする**出力の取得に用いる。
/// メッセージ言語は `LC_MESSAGES=C` に固定され、ユーザー環境の `LANG` に左右されない。
/// 標準入力は閉じ、対話プロンプトで固まらないようにする。
///
/// ユーザーが読む出力（プレビュー本文）を取得する場合は
/// [`capture_git_display`] を使うこと。
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - 非ゼロ終了した場合は stderr を含む [`Error::GitCommandFailed`]
pub fn capture_git(args: &[&str]) -> Result<Vec<u8>> {
    capture(None, args, LocaleIntent::Fixed)
}

/// [`capture_git`] と同じだが、`directory` をカレントディレクトリとして実行する（**(A) 系**）。
///
/// 候補一覧の読み取り（`git status` / `git ls-tree`）を、プロセスのカレントディレクトリではなく
/// 開いたリポジトリに対して確実に行うために用いる。
///
/// # Errors
///
/// [`capture_git`] と同じ。
pub fn capture_git_in(directory: &Path, args: &[&str]) -> Result<Vec<u8>> {
    capture(Some(directory), args, LocaleIntent::Fixed)
}

/// `git` を実行し、**ユーザーが読む**標準出力をバイト列としてキャプチャする（**(B) 系**）。
///
/// finder のプレビュー本文（`git show --color=always` 等）の取得に用いる。
/// 出力は fuzgit が解釈せずそのまま画面へ描画されるため、解決された表示言語を伝播する。
///
/// # Errors
///
/// [`capture_git`] と同じ。
pub fn capture_git_display(language: Language, args: &[&str]) -> Result<Vec<u8>> {
    capture(None, args, LocaleIntent::Display(language))
}

/// [`capture_git_display`] と同じだが、`directory` をカレントディレクトリとして実行する
/// （**(B) 系**）。
///
/// 現在のリポジトリ以外（`gz fetch --siblings` の兄弟リポジトリ）のプレビューに用いる。
///
/// # Errors
///
/// [`capture_git`] と同じ。
pub fn capture_git_display_in(
    language: Language,
    directory: &Path,
    args: &[&str],
) -> Result<Vec<u8>> {
    capture(Some(directory), args, LocaleIntent::Display(language))
}

/// `git` を `directory` で実行し、**標準エラー**をバイト列としてキャプチャする（**(B) 系**）。
///
/// `git worktree prune --dry-run --verbose` は整理対象の報告を標準出力ではなく標準エラーへ
/// 出す（git 2.55 で実測）。標準出力だけを見ると常に空になり「対象なし」と取り違えるため、
/// 報告を標準エラーへ出す少数のコマンド専用のヘルパを設ける。
///
/// # (B) 系である根拠と、その境界条件
///
/// 唯一の呼び出し元（`commands::worktree::prune`）が報告に対して行うのは「行に分割し、
/// 空行を落とす」だけで、**行の中身は一切解釈しない**。得られた行はそのまま確認プロンプトの
/// 対象一覧として画面に出る。分岐に使うのは「報告が 0 行かどうか」の 1 点だけであり、
/// これは整理対象が無いとき git が何も出力しないという挙動に依存していて、
/// メッセージの言語には依存しない。
///
/// **将来この報告から理由を機械的に読み取るようになった場合は、(A) 系へ移すか、
/// 理由の判定だけを別の (A) 系の呼び出しへ分ける必要がある。**
///
/// # Errors
///
/// [`capture_git`] と同じ。
pub fn capture_git_stderr_in(
    language: Language,
    directory: &Path,
    args: &[&str],
) -> Result<Vec<u8>> {
    Ok(capture_output(Some(directory), args, LocaleIntent::Display(language))?.stderr)
}

/// 出力をキャプチャした実行の結果。
///
/// fuzgit は中身を解釈せず、そのままユーザーへ書き出すためだけに保持する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRun {
    /// 標準出力。
    pub stdout: Vec<u8>,
    /// 標準エラー。`git fetch` の更新表と進捗はこちらへ出る。
    pub stderr: Vec<u8>,
}

/// `git` を `directory` で実行し、**対話を禁じたうえで**出力をキャプチャする（**(B) 系**）。
///
/// `gz fetch --siblings` の並列フェーズ専用。並列実行では複数の `git` の出力が同じ端末へ
/// 混ざり、認証プロンプトが出ても対象を示せないため、出力を対象ごとに束ねて受け取り、
/// 対話が必要になった場合は待たずに失敗させる（抑止の内容は
/// [`noninteractive_environment`]）。失敗した対象は呼び出し側が直列フェーズで、
/// 従来どおり継承 stdio で実行し直す。
///
/// カレントディレクトリで動く版は用意していない。唯一の呼び出し元が必ず対象リポジトリの
/// ディレクトリを指定するためで、必要になった時点で対にする。
///
/// # (B) 系である根拠と、その境界条件
///
/// この出力を読むのは**ユーザー**である。fuzgit は行数も文言も見ず、受け取ったバイト列を
/// そのまま書き出すだけで、分岐に使うのは `Result` の成否（＝終了コード）だけである。
/// 判断基準「その出力を読むのは誰か」に照らせば (B) であり、[`capture_git_stderr_in`] を
/// (B) とした判断とまったく同型である。
///
/// **将来この出力から失敗の理由を機械的に読み取るようになった場合は、(A) 系へ移すか、
/// 理由の判定だけを別の (A) 系の呼び出しへ分ける必要がある。**
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - 非ゼロ終了した場合は [`Error::GitRunFailed`]
///
/// 非ゼロ終了時に**キャプチャ済みの出力はエラーへ載せない**。載せると呼び出し側がそれを見て
/// 分岐したくなる誘因を作り、上記の境界条件が破れるため（失敗の理由は、直列フェーズで
/// 実行し直したときに git 自身が端末へ出す）。
pub fn capture_git_noninteractive_in(
    language: Language,
    directory: &Path,
    args: &[&str],
) -> Result<CapturedRun> {
    let mut command = build_command(Some(directory), args, LocaleIntent::Display(language));
    command.stdin(Stdio::null());
    for (key, value) in noninteractive_environment(std::env::var_os(GIT_SSH_COMMAND_ENV).as_deref())
    {
        command.env(key, value);
    }

    let output = command
        .output()
        .map_err(|source| map_spawn_error(source, args))?;

    if output.status.success() {
        Ok(CapturedRun {
            stdout: output.stdout,
            stderr: output.stderr,
        })
    } else {
        log_discarded_output(args, &output);
        Err(Error::GitRunFailed {
            command: command_display(args),
            code: output.status.code(),
        })
    }
}

/// 失敗した並列フェーズの実行について、呼び出し側へ渡さずに捨てる出力をログへ出す。
///
/// この出力を画面へ出さないのは、同じ対象が直列フェーズで実行し直され、git 自身が
/// 失敗の理由をその場で表示するためである（二重に理由を並べない）。とはいえ黙って捨てると
/// 「並列フェーズで何が起きたのか」を後から追う手段が無くなるため、`FUZGIT_DEBUG=1` の
/// ときだけ残す。**ログへ出すだけであり、fuzgit はこの内容を解釈しない。**
fn log_discarded_output(args: &[&str], output: &std::process::Output) {
    if !debug_enabled() {
        return;
    }

    // ログの書き込み失敗で本来の処理を止めたくないため結果を破棄する（[`log_command`] と同じ）
    let _ = writeln!(
        std::io::stderr(),
        "[fuzgit] discarded {command} (code {code})\n{stdout}{stderr}",
        command = command_display(args),
        code = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        stdout = String::from_utf8_lossy(&output.stdout),
        stderr = String::from_utf8_lossy(&output.stderr),
    );
}

/// `git` を `directory` で実行し、終了コードと標準出力を返す（**(A) 系**）。
///
/// [`capture_git_in`] と違い、非ゼロ終了をエラーにしない。`git merge-tree --write-tree` の
/// ように「終了コード 1 ＝コンフリクトあり」を正常系として扱うコマンドや、
/// `git rev-parse --verify --quiet` のように「非ゼロ終了＝解決できなかった」を
/// 結果として受け取りたいコマンドのために用意する。
///
/// 標準エラーは呼び出し側へ返さない。このヘルパを使うのは終了コード自体が意味を持つ
/// コマンドに限られ、失敗理由の提示より終了コードによる分岐が目的であるため。
///
/// # Errors
///
/// - `git` が PATH 上に無い場合は [`Error::GitNotFound`]
/// - 起動に失敗した場合は [`Error::GitSpawnFailed`]
/// - シグナルで終了させられ終了コードが得られない場合は [`Error::GitCommandFailed`]
pub fn capture_git_with_status_in(directory: &Path, args: &[&str]) -> Result<(i32, Vec<u8>)> {
    let output = build_command(Some(directory), args, LocaleIntent::Fixed)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| map_spawn_error(source, args))?;

    // シグナルによる終了では終了コードが得られない。終了コードで分岐する用途のヘルパである以上、
    // 判断できないまま続行せず失敗として扱う
    let Some(code) = output.status.code() else {
        return Err(Error::GitCommandFailed {
            args: display_args(args),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    };

    Ok((code, output.stdout))
}

/// `git merge-tree --write-tree` の終了コードが表す結果。
///
/// merge のドライランは「コンフリクトあり」を非ゼロ終了（1）で伝えるため、
/// 終了コードを一律にエラーとして扱えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeTreeOutcome {
    /// コンフリクトなくマージできる。
    Clean,
    /// コンフリクトが発生する。
    Conflicted,
    /// マージ判定そのものが失敗した（Git 2.38 未満・不正なリビジョンなど）。
    ///
    /// 予測は補助情報であり、この場合は予測表示を省略して主要動作を続行する。
    Failed,
}

impl MergeTreeOutcome {
    /// `git merge-tree --write-tree` の終了コードから結果を判定する。
    ///
    /// git の仕様は 0 ＝クリーン / 1 ＝コンフリクトあり / それ以外＝エラー。
    /// 負の値（終了コードとしては現れない）も判断できない値としてエラー側へ倒す。
    #[must_use]
    pub fn from_exit_code(code: i32) -> Self {
        match code {
            0 => MergeTreeOutcome::Clean,
            1 => MergeTreeOutcome::Conflicted,
            _ => MergeTreeOutcome::Failed,
        }
    }
}

/// `git` を実行して標準出力をキャプチャする。`directory` 指定時はそこを作業ディレクトリとする。
fn capture(directory: Option<&Path>, args: &[&str], intent: LocaleIntent) -> Result<Vec<u8>> {
    Ok(capture_output(directory, args, intent)?.stdout)
}

/// `git` を実行し、成功した場合の出力（標準出力・標準エラーの両方）を返す。
///
/// 標準入力は閉じ、対話プロンプトで固まらないようにする。
fn capture_output(
    directory: Option<&Path>,
    args: &[&str],
    intent: LocaleIntent,
) -> Result<std::process::Output> {
    let output = build_command(directory, args, intent)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| map_spawn_error(source, args))?;

    if output.status.success() {
        Ok(output)
    } else {
        Err(Error::GitCommandFailed {
            args: display_args(args),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// 作業ツリールート基準の相対パスを、git へ渡すパススペックへ変換する。
///
/// `git status --porcelain` や `git ls-tree --full-tree` が返すパスはリポジトリルート基準だが、
/// git のパス引数はカレントディレクトリ基準で解釈されるため、サブディレクトリから実行すると
/// 対象がずれる。また git のパススペックは既定でワイルドカードとして解釈されるため、
/// `a[1].txt` のような名前のファイルが取りこぼされる。
/// `:(top,literal)` を付けて「ルート基準」「ワイルドカード解釈なし」を明示することで
/// 両方を防ぐ（このパススペックは常に `--` の後ろに置く）。
#[must_use]
pub fn pathspec(path: &str) -> String {
    format!(":(top,literal){path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_git_returns_stdout_on_success() {
        let stdout = capture_git(&["--version"]).expect("git --version should succeed");
        let text = String::from_utf8(stdout).expect("git --version emits utf-8");
        assert!(
            text.starts_with("git version"),
            "unexpected output: {text:?}"
        );
    }

    #[test]
    fn capture_git_reports_stderr_on_failure() {
        let err =
            capture_git(&["fuzgit-no-such-subcommand"]).expect_err("unknown subcommand must fail");

        match err {
            Error::GitCommandFailed { args, stderr } => {
                assert_eq!(args, "fuzgit-no-such-subcommand");
                assert!(!stderr.trim().is_empty(), "stderr should be captured");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn capture_git_stderr_in_returns_the_report_written_to_stderr() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-capture-stderr");
        init_repository(dir.path());

        // `git worktree list` は標準出力へ、`--dry-run --verbose` の prune は標準エラーへ出す。
        // 前者を stderr として読むと空になることで、読み取る側を取り違えていないことが分かる
        let stderr = capture_git_stderr_in(
            Language::Japanese,
            dir.path(),
            &["worktree", "list", "--porcelain"],
        )
        .expect("git worktree list should succeed");

        assert!(
            stderr.is_empty(),
            "stdout must not leak into the stderr capture: {stderr:?}"
        );
    }

    #[test]
    fn run_git_reports_only_the_subcommand_on_failure() {
        // 未知のサブコマンドは何も変更せずに非ゼロ終了する（git 自身の説明は端末へ出る）
        let err = run_git(
            Language::Japanese,
            &["fuzgit-no-such-subcommand", "--", ":(top,literal)a[1].txt"],
        )
        .expect_err("unknown subcommand must fail");

        match err {
            Error::GitRunFailed { command, code } => {
                assert_eq!(command, "git fuzgit-no-such-subcommand");
                assert_eq!(code, Some(1));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn run_git_in_runs_in_the_given_directory() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-run-in");
        init_repository(dir.path());

        // ローカル設定への書き込みは、実行されたリポジトリが 1 つに定まることを後から確認できる
        // （出力を持たないコマンドのため、継承 stdio でもテスト出力を汚さない）
        run_git_in(
            Language::Japanese,
            dir.path(),
            &["config", "--local", "fuzgit.probe", "written"],
        )
        .expect("git config should succeed in the temporary repository");

        let stdout = capture_git_in(dir.path(), &["config", "--local", "--get", "fuzgit.probe"])
            .expect("the value should be readable back");
        let text = String::from_utf8(stdout).expect("git config emits utf-8");

        assert_eq!(text.trim(), "written");
    }

    #[test]
    fn run_git_in_reports_a_failure_as_a_run_failure() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-run-in-failure");
        init_repository(dir.path());

        // 未設定のキーの読み出しは、何も出力せずに非ゼロ終了する
        let err = run_git_in(
            Language::Japanese,
            dir.path(),
            &["config", "--local", "--get", "fuzgit.missing"],
        )
        .expect_err("an unset key must fail");

        match err {
            Error::GitRunFailed { command, code } => {
                assert_eq!(command, "git config");
                assert_ne!(code, Some(0));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_command_display_stops_at_the_subcommand() {
        // pathspec の列を再掲すると、端末に出ている git 本来のメッセージが埋もれる
        assert_eq!(
            command_display(&["commit", "--", ":(top,literal)src/cli.rs"]),
            "git commit"
        );
    }

    #[test]
    fn a_command_display_omits_options_and_operands() {
        assert_eq!(
            command_display(&["push", "--set-upstream", "origin"]),
            "git push"
        );
        assert_eq!(command_display(&["stash", "push", "--"]), "git stash");
    }

    #[test]
    fn a_command_display_without_arguments_names_git_itself() {
        assert_eq!(command_display(&[]), "git");
    }

    #[test]
    fn display_args_joins_with_spaces() {
        assert_eq!(
            display_args(&["log", "--oneline", "-n", "5"]),
            "log --oneline -n 5"
        );
    }

    #[test]
    fn capture_git_in_runs_in_the_given_directory() {
        use crate::test_support::{TempDir, commit, init_repository};

        let dir = TempDir::new("exec-capture-in");
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");

        let stdout = capture_git_in(dir.path(), &["rev-parse", "HEAD"])
            .expect("rev-parse should succeed in the temporary repository");

        let text = String::from_utf8(stdout).expect("rev-parse emits utf-8");
        assert_eq!(text.trim(), head);
    }

    #[test]
    fn capture_git_with_status_in_returns_the_output_of_a_successful_command() {
        use crate::test_support::{TempDir, commit, init_repository};

        let dir = TempDir::new("exec-status-ok");
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");

        let (code, stdout) = capture_git_with_status_in(dir.path(), &["rev-parse", "HEAD"])
            .expect("rev-parse should run");

        assert_eq!(code, 0);
        let text = String::from_utf8(stdout).expect("rev-parse emits utf-8");
        assert_eq!(text.trim(), head);
    }

    #[test]
    fn capture_git_with_status_in_reports_a_non_zero_exit_without_failing() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-status-non-zero");
        init_repository(dir.path());

        // `--verify --quiet` は解決できない参照に対しメッセージ無しで非ゼロ終了する
        let (code, stdout) = capture_git_with_status_in(
            dir.path(),
            &["rev-parse", "--verify", "--quiet", "refs/heads/missing"],
        )
        .expect("a non-zero exit must not be an error");

        assert_ne!(code, 0, "a missing reference must not exit with 0");
        assert!(stdout.is_empty(), "unexpected output: {stdout:?}");
    }

    #[test]
    fn a_clean_merge_tree_exit_code_means_no_conflict() {
        assert_eq!(MergeTreeOutcome::from_exit_code(0), MergeTreeOutcome::Clean);
    }

    #[test]
    fn a_merge_tree_exit_code_of_one_means_conflicts() {
        assert_eq!(
            MergeTreeOutcome::from_exit_code(1),
            MergeTreeOutcome::Conflicted
        );
    }

    #[test]
    fn any_other_merge_tree_exit_code_means_the_prediction_failed() {
        // 2 以上は git 側のエラー（Git 2.38 未満の未知オプション 129、fatal の 128 を含む）
        for code in [2, 3, 42, 128, 129, i32::MAX] {
            assert_eq!(
                MergeTreeOutcome::from_exit_code(code),
                MergeTreeOutcome::Failed,
                "exit code {code} must be treated as a failure"
            );
        }
    }

    #[test]
    fn a_negative_merge_tree_exit_code_means_the_prediction_failed() {
        assert_eq!(
            MergeTreeOutcome::from_exit_code(-1),
            MergeTreeOutcome::Failed
        );
    }

    #[test]
    fn a_pathspec_is_rooted_and_literal() {
        assert_eq!(pathspec("src/main.rs"), ":(top,literal)src/main.rs");
    }

    #[test]
    fn a_pathspec_keeps_wildcard_characters_verbatim() {
        assert_eq!(pathspec("dir/a[1].txt"), ":(top,literal)dir/a[1].txt");
        assert_eq!(pathspec("with space.txt"), ":(top,literal)with space.txt");
    }

    /// 組み立てたロケール環境を `Command` へ適用し、変更されたキーと値を読み出す。
    ///
    /// 確かめたいのは実際に子プロセスへ渡る内容であるため、[`LocaleEnvironment`] の内部表現では
    /// なく `Command::get_envs()` を経由する。親環境の `LC_ALL` は引数で与える
    /// （`std::env` を書き換えると並列実行される他のテストと干渉するため）。
    fn applied(intent: LocaleIntent, lc_all: Option<&str>) -> Vec<(String, Option<String>)> {
        let lc_all = lc_all.map(OsStr::new);
        let mut command = Command::new("git");
        locale_environment(intent, lc_all).apply(&mut command);

        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    /// 適用結果から 1 キー分の変更を取り出す。
    ///
    /// `None` は「そのキーに触れていない」、`Some(None)` は「削除した」、
    /// `Some(Some(値))` は「その値を設定した」を表す。
    fn change(applied: &[(String, Option<String>)], key: &str) -> Option<Option<String>> {
        applied
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    }

    #[test]
    fn lc_all_is_copied_into_every_category_before_it_is_removed() {
        let applied = applied(LocaleIntent::Fixed, Some("fr_FR.UTF-8"));

        // LC_ALL は個別カテゴリより強く、外さないと LC_MESSAGES の指定が一切効かない。
        // 外したことで文字種・照合順序まで変えてしまわないよう、値は各カテゴリへ転記する
        assert_eq!(change(&applied, LC_ALL_ENV), Some(None));
        for key in [
            "LC_CTYPE",
            "LC_COLLATE",
            "LC_MONETARY",
            "LC_NUMERIC",
            "LC_TIME",
        ] {
            assert_eq!(
                change(&applied, key),
                Some(Some("fr_FR.UTF-8".to_owned())),
                "{key} must inherit the value of LC_ALL"
            );
        }
    }

    #[test]
    fn nothing_is_expanded_when_the_parent_has_no_lc_all() {
        let applied = applied(LocaleIntent::Display(Language::Japanese), None);

        assert_eq!(change(&applied, LC_ALL_ENV), None);
        for key in LOCALE_CATEGORY_ENVS {
            assert_eq!(
                change(&applied, key),
                None,
                "{key} must be left to the parent environment"
            );
        }
    }

    #[test]
    fn a_parsed_run_pins_the_message_locale_without_touching_language() {
        let applied = applied(LocaleIntent::Fixed, None);

        assert_eq!(
            change(&applied, LC_MESSAGES_ENV),
            Some(Some("C".to_owned()))
        );
        // LC_MESSAGES=C のとき GNU gettext は LANGUAGE を無視する（調査 T-261）
        assert_eq!(change(&applied, LANGUAGE_ENV), None);
    }

    #[test]
    fn a_displayed_run_sets_the_language_without_touching_the_message_locale() {
        let applied = applied(LocaleIntent::Display(Language::Japanese), None);

        assert_eq!(change(&applied, LANGUAGE_ENV), Some(Some("ja".to_owned())));
        // 存在しないロケール名を推測して設定すると、データが無い環境で黙って C へ落ちる
        assert_eq!(change(&applied, LC_MESSAGES_ENV), None);
    }

    #[test]
    fn the_fixed_message_locale_wins_over_the_expanded_lc_all() {
        // 等価変換が設定した LC_MESSAGES を Fixed が上書きし、最終値だけが残る
        let applied = applied(LocaleIntent::Fixed, Some("ja_JP.UTF-8"));

        assert_eq!(
            change(&applied, LC_MESSAGES_ENV),
            Some(Some("C".to_owned()))
        );
    }

    #[test]
    fn lc_all_never_reaches_the_child_process() {
        for intent in [
            LocaleIntent::Fixed,
            LocaleIntent::Display(Language::English),
        ] {
            let applied = applied(intent, Some("de_DE.UTF-8"));

            assert_eq!(
                change(&applied, LC_ALL_ENV),
                Some(None),
                "{intent:?} must remove LC_ALL"
            );
        }
    }

    #[test]
    fn debug_logging_is_enabled_only_by_the_documented_value() {
        assert!(is_debug_enabled(Some("1")));
    }

    #[test]
    fn debug_logging_is_disabled_when_the_variable_is_unset() {
        assert!(!is_debug_enabled(None));
    }

    #[test]
    fn debug_logging_is_disabled_for_an_empty_value() {
        // 空文字は「設定されているが有効化はしていない」状態として無効に倒す
        assert!(!is_debug_enabled(Some("")));
    }

    #[test]
    fn debug_logging_is_disabled_for_any_other_value() {
        for value in ["0", "true", "yes", "on", "2", " 1", "1\n", "01"] {
            assert!(
                !is_debug_enabled(Some(value)),
                "{value:?} must not enable debug logging"
            );
        }
    }

    #[test]
    fn a_debug_line_shows_the_command_that_is_about_to_run() {
        // 表示・対話系 (B) は解決された表示言語を子プロセスへ伝える
        let intent = LocaleIntent::Display(Language::Japanese);

        assert_eq!(
            debug_line(
                None,
                &["switch", "feature"],
                intent,
                &locale_environment(intent, None)
            ),
            "[fuzgit] (B) git switch feature [LANGUAGE=ja]"
        );
    }

    #[test]
    fn a_debug_line_names_the_directory_when_the_command_runs_elsewhere() {
        assert_eq!(
            debug_line(
                Some(Path::new("/tmp/repo")),
                &["status", "--porcelain", "-z"],
                LocaleIntent::Fixed,
                &locale_environment(LocaleIntent::Fixed, None)
            ),
            "[fuzgit] (A) (cwd: /tmp/repo) git status --porcelain -z [LC_MESSAGES=C]"
        );
    }

    #[test]
    fn a_debug_line_keeps_the_arguments_verbatim() {
        // パススペックの取りこぼしを追えるよう、引数は加工せずそのまま並べる
        assert_eq!(
            debug_line(
                None,
                &["add", "--", ":(top,literal)dir/a[1].txt"],
                LocaleIntent::Fixed,
                &locale_environment(LocaleIntent::Fixed, None)
            ),
            "[fuzgit] (A) git add -- :(top,literal)dir/a[1].txt [LC_MESSAGES=C]"
        );
    }

    /// 子プロセスへ実際に渡ったロケール環境変数を読み出すための git エイリアス名。
    ///
    /// 公開関数は組み立てた [`Command`] をその場で実行するため、外から環境を覗く手段が無い。
    /// そこで `git` の `!` エイリアスとして `env` を起動し、その出力を観測する。
    /// **観測のためにテストがシェルを経由するだけであり、fuzgit 本体が git を
    /// 引数配列で起動することに変わりはない。**
    const ENV_ALIAS: &str = "fuzgitenv";

    /// [`ENV_ALIAS`] を標準出力へ書き出す定義（`-c` で一時的にだけ与える）。
    const ENV_ALIAS_TO_STDOUT: &str = "alias.fuzgitenv=!env";

    /// [`ENV_ALIAS`] を標準エラーへ書き出す定義（[`capture_git_stderr_in`] の検証用）。
    const ENV_ALIAS_TO_STDERR: &str = "alias.fuzgitenv=!env 1>&2";

    /// [`ENV_ALIAS`] を作業ツリー直下のファイルへ書き出す定義。
    ///
    /// 継承 stdio の実行（[`run_git`] / [`run_git_in`]）は出力をキャプチャできないため
    /// ファイル経由で観測する。`!` エイリアスは作業ツリーの最上位で実行されるため、
    /// 相対パスで一時リポジトリ直下に出力される。
    const ENV_ALIAS_TO_FILE: &str = "alias.fuzgitenv=!env > env.txt";

    /// [`ENV_ALIAS_TO_FILE`] の出力先ファイル名。
    const ENV_FILE: &str = "env.txt";

    /// ディレクトリを引数に取らない公開関数でも一時リポジトリで実行できるよう、
    /// `-C` で作業ディレクトリを与えた引数列を組み立てる。
    fn env_args<'a>(directory: &'a str, alias: &'a str) -> [&'a str; 5] {
        ["-C", directory, "-c", alias, ENV_ALIAS]
    }

    /// 一時ディレクトリのパスを git の引数として渡せる文字列にする。
    fn path_argument(directory: &Path) -> &str {
        directory
            .to_str()
            .expect("the temporary directory path should be utf-8")
    }

    /// `env` の出力から 1 つの環境変数の値を取り出す。
    fn variable(output: &[u8], key: &str) -> Option<String> {
        let text = String::from_utf8_lossy(output).into_owned();
        let prefix = format!("{key}=");

        text.lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .map(str::to_owned)
    }

    /// (B) 系で**書き換わらないはず**の `LC_MESSAGES` の値。
    ///
    /// 親に `LC_ALL` があれば等価変換によりその値が入り、無ければ親の `LC_MESSAGES` が
    /// そのまま残る。親の環境は読むだけで書き換えない（テストは並列実行されるため）。
    fn inherited_message_locale() -> Option<String> {
        std::env::var(LC_ALL_ENV)
            .ok()
            .or_else(|| std::env::var(LC_MESSAGES_ENV).ok())
    }

    /// (A) 系で**書き換わらないはず**の `LANGUAGE` の値（親からそのまま引き継がれる）。
    fn inherited_language() -> Option<String> {
        std::env::var(LANGUAGE_ENV).ok()
    }

    #[test]
    fn a_displayed_capture_passes_the_language_to_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-display");
        init_repository(dir.path());

        for language in [Language::Japanese, Language::English] {
            let stdout = capture_git_display(
                language,
                &env_args(path_argument(dir.path()), ENV_ALIAS_TO_STDOUT),
            )
            .expect("the env alias should succeed");

            assert_eq!(
                variable(&stdout, LANGUAGE_ENV).as_deref(),
                Some(language.code()),
                "{language:?} must reach the child process"
            );
            assert_eq!(
                variable(&stdout, LC_MESSAGES_ENV),
                inherited_message_locale(),
                "(B) must not rewrite {LC_MESSAGES_ENV}"
            );
        }
    }

    #[test]
    fn a_displayed_capture_in_a_directory_passes_the_language_to_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-display-in");
        init_repository(dir.path());

        let stdout = capture_git_display_in(
            Language::Japanese,
            dir.path(),
            &["-c", ENV_ALIAS_TO_STDOUT, ENV_ALIAS],
        )
        .expect("the env alias should succeed");

        assert_eq!(variable(&stdout, LANGUAGE_ENV).as_deref(), Some("ja"));
        assert_eq!(
            variable(&stdout, LC_MESSAGES_ENV),
            inherited_message_locale()
        );
    }

    #[test]
    fn a_displayed_stderr_capture_passes_the_language_to_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-stderr");
        init_repository(dir.path());

        let stderr = capture_git_stderr_in(
            Language::English,
            dir.path(),
            &["-c", ENV_ALIAS_TO_STDERR, ENV_ALIAS],
        )
        .expect("the env alias should succeed");

        assert_eq!(variable(&stderr, LANGUAGE_ENV).as_deref(), Some("en"));
        assert_eq!(
            variable(&stderr, LC_MESSAGES_ENV),
            inherited_message_locale()
        );
    }

    #[test]
    fn an_inherited_run_passes_the_language_to_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-run");
        init_repository(dir.path());

        run_git(
            Language::English,
            &env_args(path_argument(dir.path()), ENV_ALIAS_TO_FILE),
        )
        .expect("the env alias should succeed");

        let written = std::fs::read(dir.path().join(ENV_FILE)).expect("the alias should write");

        assert_eq!(variable(&written, LANGUAGE_ENV).as_deref(), Some("en"));
        assert_eq!(
            variable(&written, LC_MESSAGES_ENV),
            inherited_message_locale()
        );
    }

    #[test]
    fn an_inherited_run_in_a_directory_passes_the_language_to_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-run-in");
        init_repository(dir.path());

        run_git_in(
            Language::Japanese,
            dir.path(),
            &["-c", ENV_ALIAS_TO_FILE, ENV_ALIAS],
        )
        .expect("the env alias should succeed");

        let written = std::fs::read(dir.path().join(ENV_FILE)).expect("the alias should write");

        assert_eq!(variable(&written, LANGUAGE_ENV).as_deref(), Some("ja"));
        assert_eq!(
            variable(&written, LC_MESSAGES_ENV),
            inherited_message_locale()
        );
    }

    #[test]
    fn a_parsed_capture_pins_the_message_locale_in_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-parsed");
        init_repository(dir.path());

        let stdout = capture_git(&env_args(path_argument(dir.path()), ENV_ALIAS_TO_STDOUT))
            .expect("the env alias should succeed");

        assert_eq!(variable(&stdout, LC_MESSAGES_ENV).as_deref(), Some("C"));
        assert_eq!(
            variable(&stdout, LANGUAGE_ENV),
            inherited_language(),
            "(A) must not touch {LANGUAGE_ENV}"
        );
    }

    #[test]
    fn a_parsed_capture_in_a_directory_pins_the_message_locale_in_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-parsed-in");
        init_repository(dir.path());

        let stdout = capture_git_in(dir.path(), &["-c", ENV_ALIAS_TO_STDOUT, ENV_ALIAS])
            .expect("the env alias should succeed");

        assert_eq!(variable(&stdout, LC_MESSAGES_ENV).as_deref(), Some("C"));
        assert_eq!(variable(&stdout, LANGUAGE_ENV), inherited_language());
    }

    #[test]
    fn a_parsed_capture_with_status_pins_the_message_locale_in_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-env-parsed-status");
        init_repository(dir.path());

        let (code, stdout) =
            capture_git_with_status_in(dir.path(), &["-c", ENV_ALIAS_TO_STDOUT, ENV_ALIAS])
                .expect("the env alias should run");

        assert_eq!(code, 0);
        assert_eq!(variable(&stdout, LC_MESSAGES_ENV).as_deref(), Some("C"));
        assert_eq!(variable(&stdout, LANGUAGE_ENV), inherited_language());
    }

    #[test]
    fn a_debug_line_annotates_a_parsed_run_with_the_fixed_message_locale() {
        // (A) は fuzgit がパースする実行。ログだけで「英語に固定した」ことが分かる必要がある
        let intent = LocaleIntent::Fixed;
        let line = debug_line(None, &["log"], intent, &locale_environment(intent, None));

        assert!(line.contains("(A)"), "unexpected line: {line}");
        assert!(
            line.contains("LC_MESSAGES=C"),
            "the pinned message locale should be logged: {line}"
        );
    }

    #[test]
    fn a_debug_line_annotates_a_displayed_run_with_the_resolved_language() {
        for language in [Language::Japanese, Language::English] {
            let intent = LocaleIntent::Display(language);
            let line = debug_line(
                None,
                &["switch", "main"],
                intent,
                &locale_environment(intent, None),
            );

            assert!(line.contains("(B)"), "unexpected line: {line}");
            assert!(
                line.contains(&format!("LANGUAGE={code}", code = language.code())),
                "the resolved language should be logged: {line}"
            );
        }
    }

    #[test]
    fn a_debug_line_reports_the_expanded_and_removed_lc_all() {
        // 「なぜこの言語で出たのか」を追うには、外した LC_ALL と転記先まで見える必要がある
        let intent = LocaleIntent::Fixed;
        let locale = locale_environment(intent, Some(OsStr::new("fr_FR.UTF-8")));

        assert_eq!(
            debug_line(None, &["log"], intent, &locale),
            "[fuzgit] (A) git log [LC_ALL=(unset) LC_CTYPE=fr_FR.UTF-8 LC_COLLATE=fr_FR.UTF-8 \
             LC_MESSAGES=C LC_MONETARY=fr_FR.UTF-8 LC_NUMERIC=fr_FR.UTF-8 LC_TIME=fr_FR.UTF-8]"
        );
    }

    /// 並列フェーズ以外では**書き換わらないはず**の環境変数の、親から引き継がれる値。
    ///
    /// 親の環境は読むだけで書き換えない（テストは並列実行されるため。
    /// [`inherited_message_locale`] と同じ方針）。
    fn inherited_value(key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    #[test]
    fn the_noninteractive_environment_closes_every_way_of_asking() {
        // 端末プロンプトと askpass はどちらか一方を塞いでも、もう一方から訊かれてしまう
        assert_eq!(
            noninteractive_environment(None),
            vec![
                (GIT_TERMINAL_PROMPT_ENV, OsString::from("0")),
                (GIT_ASKPASS_ENV, OsString::new()),
                (GIT_SSH_COMMAND_ENV, OsString::from("ssh -o BatchMode=yes")),
            ]
        );
    }

    #[test]
    fn an_inherited_ssh_command_keeps_its_value_and_gains_batch_mode() {
        // 捨ててしまうと、利用者が指定した鍵や踏み台ごと失われる
        let environment = noninteractive_environment(Some(OsStr::new("ssh -i /tmp/key")));

        assert_eq!(
            ssh_command_of(&environment),
            Some(OsStr::new("ssh -i /tmp/key -o BatchMode=yes"))
        );
    }

    #[test]
    fn an_empty_ssh_command_counts_as_unset() {
        // そのまま追記すると、コマンド名の無い " -o BatchMode=yes" になる
        let environment = noninteractive_environment(Some(OsStr::new("")));

        assert_eq!(
            ssh_command_of(&environment),
            Some(OsStr::new("ssh -o BatchMode=yes"))
        );
    }

    /// 組み立て結果から `GIT_SSH_COMMAND` の値だけを取り出す。
    fn ssh_command_of<'a>(environment: &'a [(&'static str, OsString)]) -> Option<&'a OsStr> {
        environment
            .iter()
            .find(|(key, _)| *key == GIT_SSH_COMMAND_ENV)
            .map(|(_, value)| value.as_os_str())
    }

    #[test]
    fn a_noninteractive_capture_forbids_interaction_in_the_child_process() {
        use crate::test_support::{TempDir, init_repository};

        let dir = TempDir::new("exec-noninteractive-env");
        init_repository(dir.path());

        let run = capture_git_noninteractive_in(
            Language::English,
            dir.path(),
            &["-c", ENV_ALIAS_TO_STDOUT, ENV_ALIAS],
        )
        .expect("the env alias should succeed");

        assert_eq!(
            variable(&run.stdout, GIT_TERMINAL_PROMPT_ENV).as_deref(),
            Some("0")
        );
        assert_eq!(variable(&run.stdout, GIT_ASKPASS_ENV).as_deref(), Some(""));

        let ssh = variable(&run.stdout, GIT_SSH_COMMAND_ENV)
            .expect("the ssh command should be set for the child process");
        assert!(
            ssh.ends_with("-o BatchMode=yes"),
            "ssh must not be able to ask for a passphrase: {ssh}"
        );

        // (B) 系であり、表示言語は従来どおり子プロセスへ伝わる
        assert_eq!(variable(&run.stdout, LANGUAGE_ENV).as_deref(), Some("en"));
    }

    #[test]
    fn a_noninteractive_capture_keeps_stdout_and_stderr_apart() {
        use crate::test_support::{TempDir, init_repository};

        // どちらへ出たかを保ったまま渡さないと、呼び出し側が元の順で書き出せない
        let dir = TempDir::new("exec-noninteractive-streams");
        init_repository(dir.path());

        let on_stdout = capture_git_noninteractive_in(
            Language::English,
            dir.path(),
            &["-c", ENV_ALIAS_TO_STDOUT, ENV_ALIAS],
        )
        .expect("the env alias should succeed");
        let on_stderr = capture_git_noninteractive_in(
            Language::English,
            dir.path(),
            &["-c", ENV_ALIAS_TO_STDERR, ENV_ALIAS],
        )
        .expect("the env alias should succeed");

        assert!(variable(&on_stdout.stdout, GIT_TERMINAL_PROMPT_ENV).is_some());
        assert!(variable(&on_stderr.stderr, GIT_TERMINAL_PROMPT_ENV).is_some());
    }

    #[test]
    fn a_failing_noninteractive_capture_reports_only_the_exit_code() {
        use crate::test_support::{TempDir, init_repository};

        // 失敗の理由は直列フェーズで git 自身が出す。ここで抱え込むと解釈したくなる
        let dir = TempDir::new("exec-noninteractive-failure");
        init_repository(dir.path());

        let error = capture_git_noninteractive_in(
            Language::English,
            dir.path(),
            &["rev-parse", "--verify", "refs/heads/does-not-exist"],
        )
        .expect_err("an unknown revision should fail");

        assert!(
            matches!(error, Error::GitRunFailed { .. }),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn the_other_execution_paths_leave_the_interaction_settings_alone() {
        use crate::test_support::{TempDir, init_repository};

        // 抑止は並列フェーズだけに閉じる。直列フェーズは認証プロンプトを出せる必要がある
        let dir = TempDir::new("exec-noninteractive-isolation");
        init_repository(dir.path());

        let displayed = capture_git_display(
            Language::English,
            &env_args(path_argument(dir.path()), ENV_ALIAS_TO_STDOUT),
        )
        .expect("the env alias should succeed");
        let fixed = capture_git_in(dir.path(), &["-c", ENV_ALIAS_TO_STDOUT, ENV_ALIAS])
            .expect("the env alias should succeed");

        for key in [
            GIT_TERMINAL_PROMPT_ENV,
            GIT_ASKPASS_ENV,
            GIT_SSH_COMMAND_ENV,
        ] {
            assert_eq!(
                variable(&displayed, key),
                inherited_value(key),
                "{key} must be inherited untouched by capture_git_display"
            );
            assert_eq!(
                variable(&fixed, key),
                inherited_value(key),
                "{key} must be inherited untouched by capture_git_in"
            );
        }
    }

    #[test]
    fn an_inherited_stdio_run_leaves_the_interaction_settings_alone() {
        use crate::test_support::{TempDir, init_repository};

        // 直列フェーズがこの経路であり、ここまで抑止されると認証プロンプトに応答できなくなる
        let dir = TempDir::new("exec-noninteractive-serial");
        init_repository(dir.path());

        run_git_in(
            Language::English,
            dir.path(),
            &["-c", ENV_ALIAS_TO_FILE, ENV_ALIAS],
        )
        .expect("the env alias should succeed");

        let written = std::fs::read(dir.path().join(ENV_FILE)).expect("the alias should write");

        for key in [
            GIT_TERMINAL_PROMPT_ENV,
            GIT_ASKPASS_ENV,
            GIT_SSH_COMMAND_ENV,
        ] {
            assert_eq!(
                variable(&written, key),
                inherited_value(key),
                "{key} must be inherited untouched by run_git_in"
            );
        }
    }
}
