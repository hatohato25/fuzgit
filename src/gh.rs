//! `gh`（GitHub CLI）の実行（FR-34 / FR-35）。
//!
//! # なぜ [`crate::git::exec`] に通さないのか
//!
//! `exec` は「git を実行する」ための不変条件を持っている。とくに
//! [`LocaleIntent`](crate::git::exec) による **(A) fuzgit がパースする / (B) ユーザーが読む**
//! の分離（FR-26）は、git が翻訳カタログを持つことを前提にした仕組みである。
//! **`gh` の出力は英語のみ**であり、この分類を持ち込んでも意味を持たないどころか、
//! 「ロケールを加工した」という誤った記録がデバッグログに残る。
//!
//! したがって `gh` は独立したモジュールで扱い、`exec` と共有するのはデバッグログの体裁
//! （`[fuzgit]` 接頭辞と `FUZGIT_DEBUG=1` の判定）だけに留める。
//! [`crate::notify`] を独立させたのと同じ判断である。
//!
//! # このモジュールが代わりに持つ不変条件は「非対話」である
//!
//! パースする呼び出し（[`capture_gh`]）は、出力に**プロンプト・更新通知・ページャの制御列が
//! 混ざらないこと**を保証しなければならない。混ざればフィールド数が合わなくなり、
//! パースが壊れる。これを実行のたびの注意ではなく、
//! [`NONINTERACTIVE_ENV`] を必ず適用することで構造的に担保する。
//!
//! # `gh` は必須依存ではない
//!
//! 制約条件（requirements.md）が前提とする外部コマンドは `git` だけである。
//! `gh` が無い場合は [`Error::GhNotFound`] を返し、**`gz pr` だけが停止する**。
//! 他のコマンドはこのモジュールを通らないため影響を受けない。

use std::io::Write as _;
use std::process::{Command, Stdio};

use crate::error::{Error, Result};
use crate::git::exec::{DEBUG_PREFIX, debug_enabled};

/// 実行する GitHub CLI のプログラム名。
const PROGRAM: &str = "gh";

/// `gh` が「資格情報が無い」ことを示す終了コード。
///
/// **この 1 つだけを特別扱いする。**無効・期限切れのトークン（HTTP 401）は 4 ではなく
/// 1 で返るため（実測確認済み。design.md「実機で確認済みの前提」）、
/// 「非ゼロなら未認証」と扱ってはならない。
const EXIT_UNAUTHENTICATED: i32 = 4;

/// パースする呼び出しに必ず適用する非対話の設定。
///
/// - `GH_PROMPT_DISABLED`: 対話プロンプトを出させない
/// - `GH_NO_UPDATE_NOTIFIER`: 更新通知を出力へ混ぜさせない
/// - `GH_PAGER`: ページャを経由させない（空文字はページャ無効の指定）
///
/// いずれも**出力を壊さないため**の設定であり、認証や通信の挙動には関与しない。
const NONINTERACTIVE_ENV: [(&str, &str); 3] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("GH_PAGER", ""),
];

/// `gh` を実行して標準出力をキャプチャする（**パース用**）。
///
/// 非対話を固定するため、[`NONINTERACTIVE_ENV`] を適用し標準入力を閉じる。
/// 標準エラーもキャプチャし、失敗時にそのまま利用者へ見せる
/// （fuzgit 側で原因を推測しないため）。
///
/// # Errors
///
/// - `gh` が PATH 上に無い場合は [`Error::GhNotFound`]
/// - 起動に失敗した場合は [`Error::GhSpawnFailed`]
/// - 終了コード 4（資格情報なし）の場合は [`Error::GhUnauthenticated`]
/// - その他の非ゼロ終了は [`Error::GhCommandFailed`]（`gh` の標準エラーを添える）
pub fn capture_gh(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = build_command(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = command
        .output()
        .map_err(|source| map_spawn_error(source, args))?;

    if output.status.success() {
        return Ok(output.stdout);
    }

    Err(map_failure(args, output.status.code(), &output.stderr))
}

/// `gh` を標準入出力を継承したまま実行する（**表示用**）。
///
/// `gh pr checkout` / `gh pr view` / `gh pr diff` に用いる。`gh` 自身の出力
/// （進捗・認証プロンプト・ページャ）をそのまま利用者へ見せたいため、
/// [`NONINTERACTIVE_ENV`] は**適用しない**。ここで出力を壊す心配は無く、
/// 認証が必要になった場合に応答できることの方が重要である。
///
/// # Errors
///
/// - `gh` が PATH 上に無い場合は [`Error::GhNotFound`]
/// - 起動に失敗した場合は [`Error::GhSpawnFailed`]
/// - 終了コード 4（資格情報なし）の場合は [`Error::GhUnauthenticated`]
/// - その他の非ゼロ終了は [`Error::GhRunFailed`]（詳細は `gh` が端末へ出力済み）
pub fn run_gh(args: &[&str]) -> Result<()> {
    log(args, false);

    let status = Command::new(PROGRAM)
        .args(args)
        .status()
        .map_err(|source| map_spawn_error(source, args))?;

    if status.success() {
        return Ok(());
    }

    if status.code() == Some(EXIT_UNAUTHENTICATED) {
        return Err(Error::GhUnauthenticated);
    }

    Err(Error::GhRunFailed {
        command: display_command(args),
        code: status.code(),
    })
}

/// キャプチャ用の `gh` コマンドを組み立てる。
fn build_command(args: &[&str]) -> Command {
    let mut command = Command::new(PROGRAM);
    command.args(args);
    for (key, value) in NONINTERACTIVE_ENV {
        command.env(key, value);
    }

    log(args, true);

    command
}

/// `gh` の起動失敗を、原因に応じたドメインエラーへ変換する。
///
/// [`crate::git::exec`] の `map_spawn_error` と同型だが、**不在は環境の異常ではない**。
/// `gh` は任意依存であり、無いことは想定内の状態である。
fn map_spawn_error(source: std::io::Error, args: &[&str]) -> Error {
    if source.kind() == std::io::ErrorKind::NotFound {
        Error::GhNotFound
    } else {
        Error::GhSpawnFailed {
            args: display_args(args),
            source,
        }
    }
}

/// 非ゼロ終了を、終了コードに応じたドメインエラーへ変換する。
///
/// **分岐は 2 つだけである。**終了コード 4 のときだけ未認証として扱い、
/// それ以外は原因を推測せず `gh` の標準エラーをそのまま添える。
fn map_failure(args: &[&str], code: Option<i32>, stderr: &[u8]) -> Error {
    if code == Some(EXIT_UNAUTHENTICATED) {
        return Error::GhUnauthenticated;
    }

    Error::GhCommandFailed {
        args: display_args(args),
        stderr: String::from_utf8_lossy(stderr).trim_end().to_owned(),
    }
}

/// 表示・ログ用に引数配列をスペース区切りで連結する。
fn display_args(args: &[&str]) -> String {
    args.join(" ")
}

/// [`Error::GhRunFailed`] に載せるコマンド名（`gh pr checkout` 等）を組み立てる。
///
/// 引数を全部並べると本当の原因（`gh` が端末へ出したメッセージ）が埋もれるため、
/// サブコマンドまでに留める（[`Error::GitRunFailed`] と同じ判断）。
fn display_command(args: &[&str]) -> String {
    let subcommands: Vec<&str> = args
        .iter()
        .take_while(|arg| !arg.starts_with('-'))
        .take(2)
        .copied()
        .collect();

    if subcommands.is_empty() {
        PROGRAM.to_owned()
    } else {
        format!("{PROGRAM} {rest}", rest = subcommands.join(" "))
    }
}

/// デバッグログの 1 行を組み立てる。
///
/// 行頭の `(gh)` は git の `(A)` / `(B)` と見分けるための印である。`gh` にロケールの
/// 加工は無いため、末尾に出すのは非対話の設定を適用したかどうかだけになる。
fn debug_line(args: &[&str], noninteractive: bool) -> String {
    let environment = if noninteractive {
        NONINTERACTIVE_ENV
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        "inherited".to_owned()
    };

    format!(
        "{DEBUG_PREFIX} (gh) {PROGRAM} {args} [{environment}]",
        args = display_args(args)
    )
}

/// これから実行する `gh` コマンドをデバッグログとして標準エラーへ出力する。
fn log(args: &[&str], noninteractive: bool) {
    if !debug_enabled() {
        return;
    }

    // ログの書き込み失敗で本来の処理を止めない（[`crate::git::exec`] と同じ扱い）
    let _ = writeln!(std::io::stderr(), "{}", debug_line(args, noninteractive));
}

/// `jq` の `@tsv` が施したエスケープを元の文字へ戻す。
///
/// `@tsv` は値に含まれる**タブ・改行・復帰・バックスラッシュの 4 種だけ**を
/// `\t` / `\n` / `\r` / `\\`（いずれも 2 文字）へ置き換える。**この集合は閉じている**
/// ため、JSON クレートを導入せずに元の文字列を復元できる（実測確認済み。
/// design.md「実機で確認済みの前提」）。
///
/// 4 種以外の `\` + 文字は**そのまま 2 文字で返す**。`gh` が出さない組み合わせを
/// 勝手に解釈すると、本文に書かれた `\d` のような文字列を壊してしまうためである。
/// 末尾が `\` で終わる場合も同じくそのまま返す。
pub fn unescape_tsv(field: &str) -> String {
    // エスケープが 1 つも無いのが大半であり、その場合は複製だけで済ませる
    if !field.contains('\\') {
        return field.to_owned();
    }

    let mut decoded = String::with_capacity(field.len());
    let mut characters = field.chars();

    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }

        match characters.next() {
            Some('t') => decoded.push('\t'),
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('\\') => decoded.push('\\'),
            // 未知の組み合わせは解釈せず、読んだ 2 文字をそのまま戻す
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
            // 末尾の `\` は対になる文字が無い。落とさずそのまま残す
            None => decoded.push('\\'),
        }
    }

    decoded
}

/// `@tsv` の 1 行を、期待するフィールド数に分解して復号する。
///
/// フィールド区切りは**実タブ**であり、値に含まれるタブは `\t` へ退避されているため
/// 曖昧さは無い。フィールド数が想定と違う行は**黙って捨てず**エラーにする
/// （暗黙のフォールバック禁止）。
///
/// # Errors
///
/// フィールド数が `expected` と一致しない場合は [`Error::GhOutputMalformed`]。
pub fn split_tsv(line: &str, expected: usize) -> Result<Vec<String>> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != expected {
        return Err(Error::GhOutputMalformed {
            expected,
            found: fields.len(),
        });
    }

    Ok(fields.iter().map(|field| unescape_tsv(field)).collect())
}

/// `gh` が PATH 上にあるかを調べる。
///
/// finder を開く前に不在を検出して停止するために用いる（選ばせたあとに失敗させない）。
/// 実行はせず、`PATH` の探索だけを行う。
pub fn is_available() -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&paths).any(|directory| {
        let candidate = directory.join(PROGRAM);
        candidate.is_file() && is_executable(&candidate)
    })
}

/// パスが実行可能かを調べる。
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

/// 実行権限を確認できないプラットフォームでは、存在をもって実行可能とみなす。
#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noninteractive_settings_cover_prompts_notifications_and_the_pager() {
        let keys: Vec<&str> = NONINTERACTIVE_ENV.iter().map(|(key, _)| *key).collect();

        assert_eq!(
            keys,
            ["GH_PROMPT_DISABLED", "GH_NO_UPDATE_NOTIFIER", "GH_PAGER"],
            "パース用の呼び出しは出力を壊す 3 経路をすべて塞ぐこと"
        );
    }

    #[test]
    fn only_exit_code_four_is_treated_as_unauthenticated() {
        // 資格情報が無い場合だけが 4。無効なトークン（401）は 1 で返るため、
        // 「非ゼロなら未認証」と扱ってはならない（実測確認済み）
        assert!(matches!(
            map_failure(&["pr", "list"], Some(4), b""),
            Error::GhUnauthenticated
        ));

        for code in [1, 2, 3, 5, 128] {
            assert!(
                matches!(
                    map_failure(&["pr", "list"], Some(code), b"boom"),
                    Error::GhCommandFailed { .. }
                ),
                "終了コード {code} を未認証として扱ってはならない"
            );
        }
    }

    #[test]
    fn the_failure_carries_the_stderr_of_gh_without_interpreting_it() {
        let error = map_failure(
            &["pr", "list"],
            Some(1),
            b"none of the git remotes configured for this repository point to a known GitHub host.\n",
        );

        let Error::GhCommandFailed { args, stderr } = error else {
            panic!("非ゼロ終了は GhCommandFailed になること");
        };
        assert_eq!(args, "pr list");
        assert!(
            stderr.contains("known GitHub host"),
            "gh の標準エラーをそのまま添えること: {stderr}"
        );
        assert!(
            !stderr.ends_with('\n'),
            "末尾の改行だけは落とす（表示の都合）: {stderr:?}"
        );
    }

    #[test]
    fn a_missing_executable_is_not_a_spawn_failure() {
        let error = map_spawn_error(
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            &["pr", "list"],
        );

        assert!(
            matches!(error, Error::GhNotFound),
            "gh は任意依存であり、不在は専用エラーで伝える"
        );
    }

    #[test]
    fn other_spawn_failures_keep_the_arguments() {
        let error = map_spawn_error(
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            &["pr", "list"],
        );

        assert!(matches!(error, Error::GhSpawnFailed { args, .. } if args == "pr list"));
    }

    #[test]
    fn the_command_name_stops_at_the_subcommands() {
        assert_eq!(display_command(&["pr", "checkout", "12"]), "gh pr checkout");
        assert_eq!(
            display_command(&["pr", "list", "--json", "number"]),
            "gh pr list"
        );
        assert_eq!(display_command(&["--version"]), "gh");
    }

    #[test]
    fn the_four_documented_escapes_are_decoded() {
        assert_eq!(unescape_tsv(r"tab\there"), "tab\there");
        assert_eq!(unescape_tsv(r"nl\nhere"), "nl\nhere");
        assert_eq!(unescape_tsv(r"cr\rhere"), "cr\rhere");
        assert_eq!(unescape_tsv(r"bs\\here"), r"bs\here");
    }

    #[test]
    fn an_unknown_escape_is_returned_untouched() {
        // `gh` が出さない組み合わせを勝手に解釈すると、本文に書かれた文字列を壊す
        assert_eq!(unescape_tsv(r"\d{4}"), r"\d{4}");
        assert_eq!(unescape_tsv(r"C:\path"), r"C:\path");
    }

    #[test]
    fn a_trailing_backslash_is_kept() {
        assert_eq!(unescape_tsv(r"ends with\"), "ends with\\");
    }

    #[test]
    fn a_field_without_escapes_is_returned_as_is() {
        assert_eq!(unescape_tsv("plain text"), "plain text");
        assert_eq!(unescape_tsv(""), "");
    }

    #[test]
    fn a_line_is_split_on_real_tabs_and_then_decoded() {
        // 区切りは**実タブ**（`\t`）、値に含まれるタブは**2 文字の `\\t`** に退避されている。
        // この書き分けが成り立つからこそ、値にタブが入っても曖昧さが出ない
        let line = concat!("1", "\t", "feature", "\t", r"title with a\ttab");
        let fields = split_tsv(line, 3).expect("フィールド数が一致する行はパースできること");

        assert_eq!(fields, ["1", "feature", "title with a\ttab"]);
    }

    #[test]
    fn a_line_with_the_wrong_field_count_is_an_error() {
        let error = split_tsv("1\tfeature", 3).expect_err("想定と違う行は黙って捨てない");

        assert!(matches!(
            error,
            Error::GhOutputMalformed {
                expected: 3,
                found: 2
            }
        ));
    }

    #[test]
    fn an_empty_field_keeps_the_field_count() {
        // `.author` が null のとき `.author.login` は空フィールドになる（実測確認済み）。
        // フィールド数が保たれることがパースの前提である
        let fields = split_tsv("9\t\ttitle", 3).expect("空フィールドがあっても数は保たれる");

        assert_eq!(fields, ["9", "", "title"]);
    }

    #[test]
    fn the_debug_line_marks_gh_and_records_whether_it_was_noninteractive() {
        let captured = debug_line(&["pr", "list"], true);
        assert!(captured.starts_with("[fuzgit] (gh) gh pr list ["));
        assert!(captured.contains("GH_PROMPT_DISABLED=1"));
        assert!(captured.contains("GH_PAGER="));

        let inherited = debug_line(&["pr", "checkout", "1"], false);
        assert!(
            inherited.ends_with("[inherited]"),
            "継承 stdio の実行では非対話の設定を適用しないことが読み取れること: {inherited}"
        );
    }
}
