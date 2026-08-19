//! 長時間実行の完了をデスクトップ通知で知らせる（FR-29）。
//!
//! 対象は `gz fetch --siblings` と `gz pull` の 2 つだけである。どちらも通信を伴い、
//! 対象が多ければ数十秒に及ぶため、端末から離れたユーザーへ完了を伝える価値がある
//! （`gz pull` は直列実行のままであり、通知は並列化（FR-28）の有無に依存しない）。
//!
//! # git 以外の外部コマンドを起動する唯一の場所
//!
//! 通知コマンドの起動を [`crate::git::exec`] へ混ぜない。`exec` は「git を実行する」ための
//! 不変条件（(A)/(B) の分類・ロケールの適用・デバッグログ）を持っており、別のコマンドを
//! そこへ通すとその不変条件が意味を失うためである。共有するのはデバッグログの体裁
//! （`[fuzgit]` 接頭辞と `FUZGIT_DEBUG=1` の判定）だけに留める。
//!
//! # 通知は補助であり、伝達の主経路にしない
//!
//! 端末アプリに通知許可が無ければ通知は表示されず、`notify-send` が存在しない Linux も
//! 珍しくない。したがって集計は従来どおり必ず標準エラーへ書き出し、通知の成否は
//! 主処理の終了コードにも出力にも影響させない（起動失敗・非ゼロ終了はいずれも握り潰し、
//! 理由は `FUZGIT_DEBUG=1` のときだけデバッグログへ出す）。
//!
//! # 通知に載せるのは固定書式と件数だけ
//!
//! macOS の `osascript -e` は引数を **AppleScript として解釈する**ため、リポジトリ名・
//! ブランチ名・パスといったユーザー由来の文字列を埋め込むとスクリプト片の注入面になる
//! （`display notification "…"` の文字列リテラルを閉じられる）。エスケープを自前で
//! 実装するのではなく、**可変部分を持たない**ことで構造的に防ぐ。呼び出し側が渡せるのは
//! 引数を取らない文言メソッド（`notification_title`）と、件数だけを引数に取る文言メソッド
//! （`run_summary`）の結果に限られる（design.md「並列 fetch と完了通知のセキュリティ上の考慮」）。

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::Error;
use crate::git::exec::{DEBUG_PREFIX, debug_enabled};
use crate::i18n::Messages;

/// 通知の有効・無効を指定する git config のキー。
const NOTIFY_CONFIG_KEY: &str = "fuzgit.notify";

/// 通知を出す下限の所要時間。
///
/// これ未満の実行では通知しない。短い実行では端末を見たままであることがほとんどで、
/// そこへ通知を出すと「見ている画面と同じ内容が二重に届く」だけになるためである。
///
/// **10 秒**とした根拠は次のとおり。fuzgit が通知する 2 つの実行は、いずれも 1 対象あたり
/// 数秒（接続の確立と往復遅延が主）であり、対象が 1〜2 件なら数秒で終わって画面の前にいる。
/// 一方で 10 秒を超えるのは対象が数件以上あるときで、その頃には別の作業へ移っている
/// （FR-29 が想定する「端末から離れる」状況）。閾値を短く（例: 3 秒）すると日常的な
/// 1〜2 件の取得でも通知が出て煩わしくなり、長く（例: 30 秒）すると通知が要る場面の
/// 多くを取りこぼす。設定項目は増やさない（requirements.md FR-29）ため、
/// 固定値をこの中間に置く。
const NOTIFY_THRESHOLD: Duration = Duration::from_secs(10);

/// macOS の通知コマンド。
const MACOS_NOTIFIER: &str = "osascript";

/// [`MACOS_NOTIFIER`] へスクリプトを直接渡すオプション。
const MACOS_SCRIPT_OPTION: &str = "-e";

/// Linux（freedesktop.org の通知仕様）の通知コマンド。
const LINUX_NOTIFIER: &str = "notify-send";

/// `fuzgit.notify` の設定値。
///
/// `bool` ではなく列挙型にするのは、[`should_notify`] の引数として渡ったときに
/// 何の真偽値なのかが呼び出し側で読み取れるようにするため（`PruneMode` と同じ方針）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifySetting {
    /// 通知を出す（`fuzgit.notify` が真）。
    Enabled,
    /// 通知を出さない（既定。未設定または `fuzgit.notify` が偽）。
    Disabled,
}

/// 起動する通知コマンドの種類。
///
/// 実行する OS を型で表し、コマンドの組み立て（[`notify_command`]）を純関数に保つ。
/// こうしないと macOS 向けの組み立てを Linux 上で検証できない（その逆も同じ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Notifier {
    /// macOS の `osascript`。引数は AppleScript として解釈される。
    Osascript,
    /// Linux の `notify-send`。
    NotifySend,
}

/// `fuzgit.notify` を読み、通知を出す設定かどうかを返す。
///
/// **`git` プロセスを起動しない。**`gix` のプロセス内読み取り（`config_snapshot`）で
/// 現在のリポジトリの設定（system / global / local / worktree の階層がそのまま効く）から
/// 引く（`fuzgit.fetchJobs` と同じ経路。design.md「設定の読み取り」）。
///
/// 呼び出し側は `gz fetch --siblings` と `gz pull` の**両方**であり、いずれも
/// **通信を始める前に**これを解決する。不正な設定のまま長い実行を終えてから
/// エラーにすると、直し方が分かるのが数十秒後になるためである。
///
/// # Errors
///
/// `fuzgit.notify` に真偽値として解釈できない値が設定されている場合は
/// [`Error::InvalidNotify`]。既定（無効）へ黙って倒さないのは、有効にしたつもりの設定が
/// 効かないまま実行が進むことになるためである（暗黙のフォールバック禁止）。
pub fn notify_setting(repository: &gix::Repository) -> crate::error::Result<NotifySetting> {
    Ok(parse_notify(notify_configuration(repository)?))
}

/// `git config fuzgit.notify` を git のブール値として読む（取得層）。
///
/// 解釈そのものは `gix` に任せる。git のブール値は `true` / `false` / `1` / `0` / `on` /
/// `off` / `yes` / `no` に加えて**値なし**（`[fuzgit]` セクションに `notify` とだけ書く形）を
/// 真として扱う規則であり、これを fuzgit 側で書き直すと git 本体との差が生まれるためである
/// （値なしの設定は文字列としては読み出せず、boolean 取得 API でしか区別できない）。
///
/// 「未設定をどう扱うか」の判断は [`parse_notify`] が持ち、この関数は読み取りだけを担う。
///
/// # Errors
///
/// 真偽値として解釈できない値は [`Error::InvalidNotify`]。読み取った値そのものは
/// `gix` のエラーが保持しているものを使う（UTF-8 でない値はロッシー変換する）。
fn notify_configuration(repository: &gix::Repository) -> crate::error::Result<Option<bool>> {
    repository
        .config_snapshot()
        .try_boolean(NOTIFY_CONFIG_KEY)
        .map_err(|error| Error::InvalidNotify {
            // `gix` のエラーは解釈できなかった入力そのものを保持しているため、
            // 設定をもう一度引き直して「値が見つからない」場合を考える必要が無い
            value: error.input.to_string(),
        })
}

/// 読み取った `fuzgit.notify` の値を通知の設定へ解釈する（純関数）。
///
/// 未設定（`None`）は [`NotifySetting::Disabled`] とする。通知は明示的に有効化するもので
/// あり、更新しただけで黙って通知を出し始めない（requirements.md FR-29「既定で無効」）。
fn parse_notify(setting: Option<bool>) -> NotifySetting {
    match setting {
        Some(true) => NotifySetting::Enabled,
        Some(false) | None => NotifySetting::Disabled,
    }
}

/// 通知を発火するかどうかを判定する（純関数）。
///
/// 設定が有効で、かつ実行の所要時間が [`NOTIFY_THRESHOLD`] 以上の場合にだけ通知する。
/// `elapsed` の計測は呼び出し側が [`std::time::Instant`]（単調時計）で行う。壁時計を
/// 使わないのは、実行中に時刻が調整されると所要時間が負になったり跳ねたりするためである。
pub fn should_notify(setting: NotifySetting, elapsed: Duration) -> bool {
    match setting {
        NotifySetting::Enabled => elapsed >= NOTIFY_THRESHOLD,
        NotifySetting::Disabled => false,
    }
}

/// 完了通知を出す。**成否は呼び出し側へ返さない。**
///
/// 戻り値を持たないのは、通知が出たかどうかで主処理の結果を変えないことを型で示すため
/// である（通知コマンドが無い環境・通知を許可していない端末アプリでは表示されないが、
/// それは失敗ではない）。表示できなかった理由は `FUZGIT_DEBUG=1` のときだけ
/// デバッグログへ出す。
///
/// `title` / `body` には**引数を取らない文言と件数だけで組み立てた文字列**を渡すこと。
/// リポジトリ名・ブランチ名・パスを渡してはならない（モジュールの doc comment を参照）。
/// `messages` を受け取るのは、どの表示言語で通知したのかをデバッグログへ残すためである
/// （通知の文言が想定と違うときに、言語解決とコマンド起動のどちらの問題かを切り分ける）。
pub fn notify(messages: &dyn Messages, title: &str, body: &str) {
    let (program, arguments) = notify_command(current_notifier(), title, body);
    run_notifier(messages, program, &arguments);
}

/// 実行中の OS に対応する通知コマンドを選ぶ。
///
/// 配布対象は macOS と Linux だけであり（design.md「完了通知の実現方式」）、
/// それ以外の OS は `notify-send` を試して見つからずに諦める経路へ入る。
fn current_notifier() -> Notifier {
    if cfg!(target_os = "macos") {
        Notifier::Osascript
    } else {
        Notifier::NotifySend
    }
}

/// 通知コマンドの名前と引数配列を組み立てる（純関数）。
///
/// macOS は `osascript -e 'display notification "<本文>" with title "<タイトル>"'`、
/// Linux は `notify-send <タイトル> <本文>`。いずれも引数配列で渡し、シェルを経由しない
/// （git の実行と同じ規則）。
///
/// **AppleScript の文字列リテラルをエスケープしない。**エスケープが要らないのは
/// `title` / `body` に可変部分（ユーザー由来の文字列）が入らないためであり、この前提を
/// 崩す呼び出しを足してはならない（モジュールの doc comment を参照）。
fn notify_command(notifier: Notifier, title: &str, body: &str) -> (&'static str, Vec<String>) {
    match notifier {
        Notifier::Osascript => (
            MACOS_NOTIFIER,
            vec![
                MACOS_SCRIPT_OPTION.to_owned(),
                format!("display notification \"{body}\" with title \"{title}\""),
            ],
        ),
        Notifier::NotifySend => (LINUX_NOTIFIER, vec![title.to_owned(), body.to_owned()]),
    }
}

/// 通知コマンドを起動し、失敗しても何も返さずに戻る。
///
/// 標準入力は塞ぎ、標準出力・標準エラーはキャプチャする。通知コマンドの出力を端末へ
/// そのまま流すと、fuzgit の集計行の間に無関係な行（`notify-send` の警告など）が
/// 割り込むためである。キャプチャした内容は `FUZGIT_DEBUG=1` のときだけログへ出し、
/// **fuzgit はこれを解釈しない**。
fn run_notifier(messages: &dyn Messages, program: &str, arguments: &[String]) {
    let result = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output();

    match result {
        // 通知が表示されたかどうかまでは分からない（端末アプリの通知許可に依る）。
        // fuzgit が知り得るのは「通知コマンドが正常終了した」ことまでである
        Ok(output) if output.status.success() => {}
        Ok(output) => log_failure(
            messages,
            program,
            &format!(
                "exit {code}: {stderr}",
                code = output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                stderr = String::from_utf8_lossy(&output.stderr).trim_end(),
            ),
        ),
        // コマンド不在（`ErrorKind::NotFound`）もここへ来る。`notify-send` が入っていない
        // Linux は普通にあるため、これは失敗ではなく「通知しない」という結果である
        Err(error) => log_failure(messages, program, &error.to_string()),
    }
}

/// 通知できなかった理由をデバッグログへ出す。
///
/// 出力先を標準エラーにするのは、標準出力をハッシュ・タグ名のパイプ用途に空けておくため
/// （[`crate::git::exec`] のデバッグログと同じ）。表示言語を併記するのは、通知の文言が
/// 想定と違うときに言語解決の結果を切り分けられるようにするためである。
fn log_failure(messages: &dyn Messages, program: &str, reason: &str) {
    if !debug_enabled() {
        return;
    }

    // ログの書き込み失敗で主処理を止めたくないため結果を破棄する（`exec` の `log_command` と同じ）
    let _ = writeln!(
        std::io::stderr(),
        "{DEBUG_PREFIX} notification skipped ({program}) [LANGUAGE={language}]: {reason}",
        language = messages.language().code(),
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::i18n::Language;
    use crate::test_support::{TempDir, git_in, init_repository};

    /// 通知に載せてはならないユーザー由来の文字列（テスト用の見本）。
    const USER_SUPPLIED: &str = "secret-repo";

    fn open_isolated(path: &Path) -> gix::Repository {
        gix::open_opts(path, gix::open::Options::isolated())
            .expect("initialized repository must be openable")
    }

    /// `fuzgit.notify` に `value` を設定したリポジトリを開く。
    fn repository_with(label: &str, value: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        git_in(dir.path(), &["config", NOTIFY_CONFIG_KEY, value]);
        let repository = open_isolated(dir.path());

        (dir, repository)
    }

    #[test]
    fn the_true_spellings_of_git_enable_the_notification() {
        for value in ["true", "1", "on", "yes"] {
            let (_dir, repository) = repository_with("notify-true", value);

            assert_eq!(
                notify_setting(&repository).expect("a boolean must be accepted"),
                NotifySetting::Enabled,
                "`{value}` must enable the notification"
            );
        }
    }

    #[test]
    fn the_false_spellings_of_git_disable_the_notification() {
        for value in ["false", "0", "off", "no"] {
            let (_dir, repository) = repository_with("notify-false", value);

            assert_eq!(
                notify_setting(&repository).expect("a boolean must be accepted"),
                NotifySetting::Disabled,
                "`{value}` must disable the notification"
            );
        }
    }

    #[test]
    fn a_key_without_a_value_is_read_as_true_like_git_does() {
        // `git config` は値なしの設定を書けないため、設定ファイルへ直接書く。
        // 値なしを真とみなすのは git 本体の規則であり、fuzgit はそれに従う
        let dir = TempDir::new("notify-implicit");
        init_repository(dir.path());
        let config = dir.path().join(".git").join("config");
        let mut contents = std::fs::read_to_string(&config).expect("the config must be readable");
        contents.push_str("[fuzgit]\n\tnotify\n");
        std::fs::write(&config, contents).expect("the config must be writable");

        assert_eq!(
            notify_setting(&open_isolated(dir.path())).expect("a value-less key must be accepted"),
            NotifySetting::Enabled
        );
    }

    #[test]
    fn an_unset_key_leaves_the_notification_disabled() {
        let dir = TempDir::new("notify-unset");
        init_repository(dir.path());

        assert_eq!(
            notify_configuration(&open_isolated(dir.path())).expect("an unset key is not an error"),
            None
        );
        assert_eq!(
            notify_setting(&open_isolated(dir.path())).expect("an unset key is not an error"),
            NotifySetting::Disabled
        );
    }

    #[test]
    fn a_value_that_is_not_a_boolean_stops_the_command() {
        let (_dir, repository) = repository_with("notify-invalid", "sometimes");

        let error =
            notify_setting(&repository).expect_err("an invalid value must stop the command");

        match error {
            Error::InvalidNotify { value } => assert_eq!(value, "sometimes"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn an_empty_value_is_read_as_false_like_git_does() {
        // 空文字を「未設定」とみなす `fuzgit.fetchJobs` とは扱いが異なる。ここは git の
        // ブール値解釈（空文字は偽）へ委ねており、`git config --bool` と結果が一致する
        let (_dir, repository) = repository_with("notify-empty", "");

        assert_eq!(
            notify_setting(&repository).expect("an empty value must be accepted"),
            NotifySetting::Disabled
        );
    }

    #[test]
    fn the_invalid_value_is_reported_in_both_languages() {
        let error = Error::InvalidNotify {
            value: "sometimes".to_owned(),
        };

        let japanese = Language::Japanese.messages().errors().describe(&error);
        let english = Language::English.messages().errors().describe(&error);

        for described in [&japanese, &english] {
            assert!(
                described.contains("sometimes") && described.contains(NOTIFY_CONFIG_KEY),
                "the value and the setting must be named: {described}"
            );
        }
        assert_ne!(japanese, english, "the description must be translated");
    }

    #[test]
    fn the_threshold_is_ten_seconds() {
        // 設定項目を増やさない固定値であるため、値そのものを固定して意図しない変更を検出する
        assert_eq!(NOTIFY_THRESHOLD, Duration::from_secs(10));
    }

    #[test]
    fn a_run_that_reaches_the_threshold_notifies_when_it_is_enabled() {
        assert!(should_notify(NotifySetting::Enabled, NOTIFY_THRESHOLD));
        assert!(should_notify(
            NotifySetting::Enabled,
            NOTIFY_THRESHOLD + Duration::from_secs(1)
        ));
    }

    #[test]
    fn a_short_run_does_not_notify_even_when_it_is_enabled() {
        assert!(!should_notify(
            NotifySetting::Enabled,
            NOTIFY_THRESHOLD - Duration::from_millis(1)
        ));
        assert!(!should_notify(NotifySetting::Enabled, Duration::ZERO));
    }

    #[test]
    fn a_disabled_setting_never_notifies_however_long_the_run_was() {
        for elapsed in [Duration::ZERO, NOTIFY_THRESHOLD, NOTIFY_THRESHOLD * 60] {
            assert!(!should_notify(NotifySetting::Disabled, elapsed));
        }
    }

    #[test]
    fn macos_passes_a_single_applescript_statement_to_osascript() {
        let (program, arguments) = notify_command(Notifier::Osascript, "gz pull", "3 succeeded");

        assert_eq!(program, "osascript");
        assert_eq!(
            arguments,
            [
                "-e",
                "display notification \"3 succeeded\" with title \"gz pull\""
            ]
        );
    }

    #[test]
    fn linux_passes_the_title_and_the_body_as_two_arguments() {
        let (program, arguments) = notify_command(Notifier::NotifySend, "gz pull", "3 succeeded");

        assert_eq!(program, "notify-send");
        assert_eq!(arguments, ["gz pull", "3 succeeded"]);
    }

    #[test]
    fn the_wording_of_the_notification_carries_no_user_supplied_string() {
        // 呼び出し側が渡すのは「引数を取らないタイトル」と「件数だけを引数に取る本文」で
        // あり、リポジトリ名・ブランチ名は組み立てに現れない
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();
            let body = messages.common().run_summary(3, 1);

            for title in [
                messages.fetch().notification_title(),
                messages.pull().notification_title(),
            ] {
                for notifier in [Notifier::Osascript, Notifier::NotifySend] {
                    let (_program, arguments) = notify_command(notifier, title, &body);

                    for argument in &arguments {
                        assert!(
                            !argument.contains(USER_SUPPLIED),
                            "no user supplied string may reach the notifier: {argument}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_applescript_statement_stays_a_single_literal_for_every_wording() {
        // タイトル・本文が AppleScript の文字列リテラルを閉じられないことを、実際に使う
        // 文言すべてについて固定する（`"` と `\` が現れないこと）
        for language in [Language::Japanese, Language::English] {
            let messages = language.messages();
            let body = messages.common().run_summary(12, 0);

            for title in [
                messages.fetch().notification_title(),
                messages.pull().notification_title(),
            ] {
                for text in [title, body.as_str()] {
                    assert!(
                        !text.contains('"') && !text.contains('\\'),
                        "the wording must not be able to close the AppleScript literal: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_missing_notifier_leaves_the_caller_untouched() {
        // コマンドが見つからない環境（`notify-send` の無い Linux・CI）でも、
        // `notify` は panic せず何も返さずに戻る
        run_notifier(
            Language::English.messages(),
            "fuzgit-notifier-that-does-not-exist",
            &["--title".to_owned()],
        );
    }

    #[test]
    fn a_notifier_that_fails_leaves_the_caller_untouched() {
        // 非ゼロ終了も握り潰す。`false` はどの環境にもある「必ず失敗するコマンド」
        run_notifier(Language::Japanese.messages(), "false", &[]);
    }
}
