//! 表示言語の解決（FR-25）。
//!
//! 解決順は次の 5 層で、上位の層で決まった時点で下位の層は参照しない。
//!
//! | 優先 | 取得元 |
//! |---|---|
//! | 1 | `--lang <ja\|en\|auto>` |
//! | 2 | `FUZGIT_LANG` 環境変数 |
//! | 3 | `git config fuzgit.lang` |
//! | 4 | `LC_ALL` → `LC_MESSAGES` → `LANGUAGE` → `LANG` |
//! | 5 | フォールバック（`en`） |
//!
//! 層 1〜3 は **fuzgit への明示的な指定**であるため、`ja` / `en` / `auto` 以外の値は
//! [`LanguageError`] で停止する（黙って既定へ倒さない）。層 4 は fuzgit への指示ではなく
//! **環境の記述**であるため、解釈できない値（`C` / `POSIX` / 未知の言語）はエラーにせず
//! 「`ja` ではない」と判定して層 5 へ進む。**この非対称は意図的**であり、単体テストで固定する。
//!
//! # 純関数と取得層の分離
//!
//! [`resolve`] / [`language_from_locale`] / [`scan_lang_flag`] は入力を引数で受け取る
//! 純関数とし、環境変数・git 設定を実際に読む処理は薄い取得層
//! （[`language_from_env`] / [`language_from_config`] / [`locale_from_env`]）に分ける。
//! `crate::git::exec` の `is_debug_enabled` が同じ理由（テストは並列実行されるため
//! `std::env::set_var` でプロセス全体の環境を書き換えられない）でこの形を採っており、
//! その既存パターンを踏襲する。

use std::ffi::{OsStr, OsString};
use std::fmt;

use gix::bstr::ByteSlice as _;
use thiserror::Error;

use super::Language;

/// 層 2 で参照する環境変数名。
pub const LANGUAGE_ENV: &str = "FUZGIT_LANG";

/// 層 3 で参照する git config のキー。
pub const LANGUAGE_CONFIG_KEY: &str = "fuzgit.lang";

/// 層 4 で参照する環境変数名。POSIX（`LC_ALL` → 個別 `LC_*` → `LANG`）と
/// GNU gettext（`LANGUAGE`）の優先順に合わせた並び。
const LOCALE_ENV_KEYS: [&str; 4] = ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"];

/// 層 1〜3 が受理する日本語の指定値。
const VALUE_JAPANESE: &str = "ja";

/// 層 1〜3 が受理する英語の指定値。
const VALUE_ENGLISH: &str = "en";

/// 層 1〜3 が受理する自動判定の指定値。
const VALUE_AUTO: &str = "auto";

/// 先読みが認識する `--lang VALUE` 形式の綴り。
const LANG_FLAG: &str = "--lang";

/// 先読みが認識する `--lang=VALUE` 形式の接頭辞。
const LANG_FLAG_PREFIX: &str = "--lang=";

/// 引数列の終端。これ以降は先読みの対象にしない。
const ARGUMENT_TERMINATOR: &str = "--";

/// 明示指定（層 1〜3）を与えた取得元。
///
/// エラーメッセージで「どこで指定された値が不正なのか」を示すために持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageSource {
    /// 層 1: コマンドライン引数 `--lang`。
    Flag,
    /// 層 2: 環境変数 `FUZGIT_LANG`。
    Env,
    /// 層 3: `git config fuzgit.lang`。
    Config,
}

impl fmt::Display for LanguageSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Flag => LANG_FLAG,
            Self::Env => LANGUAGE_ENV,
            Self::Config => "git config fuzgit.lang",
        };
        formatter.write_str(text)
    }
}

/// 言語の解決に失敗した理由。
///
/// **メッセージは英語で固定する。**このエラーが起きた時点では表示言語がまだ決まっておらず、
/// フォールバック言語が `en` である以上、英語で表示することは規定動作である
/// （暗黙のフォールバックではない）。
#[derive(Debug, Error)]
pub enum LanguageError {
    /// 層 1〜3 に `ja` / `en` / `auto` 以外の値が指定された。
    #[error("invalid value for {origin}: `{value}` (expected `ja`, `en`, or `auto`)")]
    InvalidValue {
        /// 不正な値を与えた取得元（層 1〜3）。
        origin: LanguageSource,
        /// 指定された値。UTF-8 でない値はロッシー変換された文字列になる。
        value: String,
    },

    /// 層 3（`git config fuzgit.lang`）の読み取り自体に失敗した。
    ///
    /// git 本体が壊れた設定でエラーにするのと同じ扱いとして停止させる。
    #[error("failed to read `fuzgit.lang` from the git configuration")]
    ConfigReadFailed {
        /// `gix` の設定読み込みエラー。
        ///
        /// 実体が大きいため [`LanguageError`] 全体が肥大化しないよう Box 化して保持する。
        #[source]
        source: Box<gix::config::file::init::from_paths::Error>,
    },
}

/// 言語解決の入力。
///
/// **この構造体を受け取る関数は環境変数も git 設定も読まない。**すべて呼び出し側が
/// 集めてから渡すことで、解決規則そのものを純関数として単体テストできるようにする。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LanguageInputs<'a> {
    /// 層 1: `--lang` の先読み結果（[`scan_lang_flag`]）。
    pub flag: Option<&'a str>,
    /// 層 2: 環境変数 `FUZGIT_LANG`（[`language_from_env`]）。
    pub env: Option<&'a str>,
    /// 層 3: `git config fuzgit.lang`（[`language_from_config`]）。
    pub config: Option<&'a str>,
    /// 層 4: ロケール環境変数の最初の非空値（[`locale_from_env`]）。
    pub locale: Option<&'a str>,
}

/// 層 1〜3 の明示指定を解釈した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplicitLanguage {
    /// 言語が確定した。
    Fixed(Language),
    /// `auto`。以降の明示層を飛ばして層 4（環境からの自動判定）へ進む。
    Auto,
}

/// 層 1〜3 の値を解釈する。
///
/// 受理するのは `ja` / `en` / `auto` の 3 つだけで、大文字小文字は区別する
/// （clap の `value_enum` が既定で区別するため、先読みとパース結果を一致させる）。
fn parse_explicit(origin: LanguageSource, value: &str) -> Result<ExplicitLanguage, LanguageError> {
    match value {
        VALUE_JAPANESE => Ok(ExplicitLanguage::Fixed(Language::Japanese)),
        VALUE_ENGLISH => Ok(ExplicitLanguage::Fixed(Language::English)),
        VALUE_AUTO => Ok(ExplicitLanguage::Auto),
        _ => Err(LanguageError::InvalidValue {
            origin,
            value: value.to_owned(),
        }),
    }
}

/// 入力から表示言語を決める（層 1〜5）。
///
/// 上位の層で決まった時点で下位の層は参照しない。`auto` はどの層に現れても
/// 「以降の明示層（層 1〜3）をすべて飛ばして層 4 へ進む」意味になるため、
/// 例えば `--lang auto` を与えると `FUZGIT_LANG` の値は検証も参照もされない。
///
/// # Errors
///
/// 層 1〜3 に `ja` / `en` / `auto` 以外の値が指定された場合は
/// [`LanguageError::InvalidValue`] を返す。層 4 は解釈できなくてもエラーにならない。
pub fn resolve(inputs: &LanguageInputs) -> Result<Language, LanguageError> {
    for (origin, value) in [
        (LanguageSource::Flag, inputs.flag),
        (LanguageSource::Env, inputs.env),
        (LanguageSource::Config, inputs.config),
    ] {
        let Some(value) = value else {
            continue;
        };

        match parse_explicit(origin, value)? {
            ExplicitLanguage::Fixed(language) => return Ok(language),
            ExplicitLanguage::Auto => break,
        }
    }

    Ok(inputs
        .locale
        .and_then(language_from_locale)
        .unwrap_or(Language::English))
}

/// 現在のプロセスの環境・引数列・リポジトリ設定から表示言語を決める（層 1〜5）。
///
/// [`resolve`] が純関数であるのに対し、こちらは取得層
/// （[`scan_lang_flag`] / [`language_from_env`] / [`language_from_config`] /
/// [`locale_from_env`]）をまとめて呼ぶ薄い composition であり、`main` の起動処理から
/// 1 回だけ呼ばれることを想定する。
///
/// `repository` は「リポジトリを開けた場合のみ」渡す。開けなかった場合（リポジトリ外での
/// 実行・`gz --help`）も言語は決まらなければならないため、`None` を渡してユーザー全体の
/// 設定から層 3 を引く。
///
/// # 層 3 を先読みしないことについて
///
/// 層 1・2 で言語が確定する場合でも `git config fuzgit.lang` の**読み取り**は行う
/// （[`LanguageInputs`] が値を eager に集める構造であるため）。ただし読み取った値の
/// **検証**は [`resolve`] が層 3 に到達したときだけ行われるので、`--lang` 指定時に
/// 設定側の不正値で停止することはない。
///
/// # Errors
///
/// 層 1〜3 に `ja` / `en` / `auto` 以外の値が指定された場合は
/// [`LanguageError::InvalidValue`]、層 3 の設定そのものを読めなかった場合は
/// [`LanguageError::ConfigReadFailed`] を返す。
pub fn resolve_from_environment(
    argv: &[OsString],
    repository: Option<&gix::Repository>,
) -> Result<Language, LanguageError> {
    let flag = scan_lang_flag(argv);
    let env = language_from_env();
    let config = language_from_config(repository)?;
    let locale = locale_from_env();

    resolve(&LanguageInputs {
        flag: flag.as_deref(),
        env: env.as_deref(),
        config: config.as_deref(),
        locale: locale.as_deref(),
    })
}

/// ロケール文字列を解釈して言語を判定する（層 4）。
///
/// 判定は次の順で行う。
///
/// 1. `LANGUAGE` 形式（`ja:en` のような `:` 区切りの優先リスト）の最初の要素を取る
/// 2. `.`（コードセット）と `@`（修飾子）以降を落とす
/// 3. `_`（地域）以降を落とす
/// 4. 残りが `ja` なら [`Language::Japanese`]、それ以外は `None`
///
/// **`None` はエラーではない。**層 4 は fuzgit への指示ではなく環境の記述であるため、
/// `C` / `POSIX` / 未知の言語は「`ja` ではない」と判定して層 5（`en`）へ進む。
pub fn language_from_locale(value: &str) -> Option<Language> {
    let preferred = value.split(':').next()?;
    let without_codeset = preferred.split(['.', '@']).next()?;
    let language = without_codeset.split('_').next()?;

    (language == VALUE_JAPANESE).then_some(Language::Japanese)
}

/// clap へ渡す前に引数列から `--lang` の値を取り出す（層 1）。
///
/// 先読みが必要なのは、clap がヘルプ・パーサエラーを出力して `exit` する**前に**
/// 言語が決まっていなければ、その文言を選べないため。
///
/// # 認識する綴りと、意図的に取りこぼす形
///
/// 認識するのは `--lang=VALUE` と `--lang VALUE` の 2 形式だけで、最初の `--` より前を
/// 対象とする。同じ綴りが複数回現れた場合は clap と同じく**最後の指定**を採る。
/// 次の形は意図的に取りこぼして `None` を返す。
///
/// - `--` より後ろに置かれた `--lang`（clap も引数として解釈しない）
/// - 値が UTF-8 でない場合（不正値かどうかの判定は [`resolve`] に委ねられないため
///   `None` とする。指定が無かった場合と同じ扱いになる）
/// - `--lang` が引数列の末尾にあり値が続かない場合（clap 側がエラーにする）
///
/// 取りこぼした場合、clap のヘルプ・パーサエラーだけが環境由来の言語で出力される
/// （fuzgit 本体は `--lang` で指定された言語で動く）。**乖離は clap の出力に限られる。**
pub fn scan_lang_flag(argv: &[OsString]) -> Option<String> {
    let mut found = None;
    let mut index = 0;

    while index < argv.len() {
        let argument = argv[index].as_os_str();

        if argument == OsStr::new(ARGUMENT_TERMINATOR) {
            break;
        }

        if let Some(value) = argument
            .to_str()
            .and_then(|argument| argument.strip_prefix(LANG_FLAG_PREFIX))
        {
            found = Some(value.to_owned());
            index += 1;
            continue;
        }

        if argument == OsStr::new(LANG_FLAG) {
            // 値が UTF-8 でない場合・値そのものが無い場合はいずれも None になる。
            found = argv
                .get(index + 1)
                .and_then(|value| value.to_str())
                .map(str::to_owned);
            index += 2;
            continue;
        }

        index += 1;
    }

    found
}

/// 環境変数の値を層 2 の入力へ変換する。
///
/// UTF-8 でない値は「未設定」に倒さず、ロッシー変換した文字列を**不正値**として
/// [`resolve`] へ渡す（fuzgit への明示的な指定である以上、黙って無視しない）。
/// 空文字は「未設定」として扱う（`export FUZGIT_LANG=` は指定を取り消す操作であり、
/// 層 4 のロケール変数で空値を未設定とみなすのと同じ規則）。
///
/// 値を引数で受け取るのは、`std::env::set_var` に依存せず単体テストできるようにするため。
fn language_env_input(value: Option<&OsStr>) -> Option<String> {
    let value = value?;

    if value.is_empty() {
        return None;
    }

    Some(value.to_string_lossy().into_owned())
}

/// 現在のプロセスの環境変数から層 2 の入力を取得する。
pub fn language_from_env() -> Option<String> {
    language_env_input(std::env::var_os(LANGUAGE_ENV).as_deref())
}

/// 候補列から最初の非空の値を選ぶ（層 4）。
///
/// 値を引数で受け取るのは、`std::env::set_var` に依存せず単体テストできるようにするため。
fn first_non_empty(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values.into_iter().flatten().find(|value| !value.is_empty())
}

/// 現在のプロセスの環境変数から層 4 の入力を取得する。
///
/// `LC_ALL` → `LC_MESSAGES` → `LANGUAGE` → `LANG` の順で最初の非空値を採る。
/// UTF-8 でない値もロッシー変換して「設定されている」と扱う（POSIX の優先順を保つため。
/// 層 4 は解釈できない値をエラーにせず層 5 へ進むので、実害は無い）。
pub fn locale_from_env() -> Option<String> {
    first_non_empty(
        LOCALE_ENV_KEYS
            .iter()
            .map(|key| std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())),
    )
}

/// `git config fuzgit.lang` を読む（層 3）。
///
/// `repository` が `Some` の場合はそのリポジトリの設定スナップショット
/// （system / global / local / worktree の階層がそのまま効く）から、`None` の場合は
/// ユーザー全体（git installation / system / global）の設定から引く。
/// **`git` プロセスは起動しない**（`gix` によるプロセス内読み取りのみ）。
///
/// # 「不在」「値あり」「読み取り失敗」の区別
///
/// - **不在**: `Ok(None)`。規定動作として層 4 へ進む
/// - **値あり**: `Ok(Some(value))`。妥当性（`ja` / `en` / `auto`）の判定は [`resolve`] の責務
/// - **読み取り失敗**: `Err`。設定ファイルの I/O・パース失敗は値の取得時ではなく
///   **設定の読み込み時**に現れる。`repository` が `Some` の場合、その失敗は
///   リポジトリを開いた時点（`gix::discover`）で既に検出されているため、
///   ここで新たに失敗することはない
///
/// # 制約
///
/// `repository` が `None` の経路（リポジトリ外）では、`gix` が
/// `includeIf` 条件をリポジトリ情報が無いものとして評価する。すなわち
/// `[includeIf "gitdir:…"]` 配下に書いた `fuzgit.lang` は**読み飛ばされる**
/// （失敗にはならない）。`GIT_CONFIG_*` 環境変数による上書きもこの経路では効かない。
pub fn language_from_config(
    repository: Option<&gix::Repository>,
) -> Result<Option<String>, LanguageError> {
    let value = match repository {
        Some(repository) => repository.config_snapshot().string(LANGUAGE_CONFIG_KEY),
        None => gix::config::File::from_globals()
            .map_err(|source| LanguageError::ConfigReadFailed {
                source: Box::new(source),
            })?
            .string(LANGUAGE_CONFIG_KEY),
    };

    // UTF-8 でない値は「未設定」に倒さず、不正値として resolve() へ渡す（層 2 と同じ扱い）。
    // 空文字は「未設定」として層 4 へ進む。
    Ok(value
        .map(|value| value.to_str_lossy().into_owned())
        .filter(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, git_in, init_repository};
    use std::path::Path;

    /// 開発者の `~/.gitconfig` に影響されずに検証するため、リポジトリ内の設定だけを
    /// 読み込む形でリポジトリを開く（`Options::isolated()`）。
    fn open_isolated(path: &Path) -> gix::Repository {
        gix::open_opts(path, gix::open::Options::isolated())
            .expect("initialized repository must be openable")
    }

    fn os_args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn language_from_locale_accepts_japanese_spellings() {
        assert_eq!(
            language_from_locale("ja_JP.UTF-8"),
            Some(Language::Japanese)
        );
        assert_eq!(language_from_locale("ja"), Some(Language::Japanese));
        assert_eq!(
            language_from_locale("ja_JP@cjknarrow"),
            Some(Language::Japanese)
        );
        assert_eq!(language_from_locale("ja:en"), Some(Language::Japanese));
    }

    #[test]
    fn language_from_locale_returns_none_for_other_locales() {
        assert_eq!(language_from_locale("en_US.UTF-8"), None);
        assert_eq!(language_from_locale("C"), None);
        assert_eq!(language_from_locale("C.UTF-8"), None);
        assert_eq!(language_from_locale("POSIX"), None);
        assert_eq!(language_from_locale(""), None);
        assert_eq!(language_from_locale("de_DE.UTF-8"), None);
    }

    #[test]
    fn language_from_locale_reads_only_the_first_entry_of_a_language_list() {
        // LANGUAGE は優先リストであり、先頭の要素だけが判定に効く。
        assert_eq!(language_from_locale("en:ja"), None);
    }

    #[test]
    fn resolve_prefers_the_flag_over_every_other_layer() {
        let language = resolve(&LanguageInputs {
            flag: Some("en"),
            env: Some("ja"),
            config: Some("ja"),
            locale: Some("ja_JP.UTF-8"),
        })
        .expect("`en` is a valid explicit value");

        assert_eq!(language, Language::English);
    }

    #[test]
    fn resolve_prefers_the_environment_over_the_configuration() {
        let language = resolve(&LanguageInputs {
            flag: None,
            env: Some("en"),
            config: Some("ja"),
            locale: Some("ja_JP.UTF-8"),
        })
        .expect("`en` is a valid explicit value");

        assert_eq!(language, Language::English);
    }

    #[test]
    fn resolve_prefers_the_configuration_over_the_locale() {
        let language = resolve(&LanguageInputs {
            flag: None,
            env: None,
            config: Some("ja"),
            locale: Some("en_US.UTF-8"),
        })
        .expect("`ja` is a valid explicit value");

        assert_eq!(language, Language::Japanese);
    }

    #[test]
    fn resolve_falls_back_to_the_locale_when_no_explicit_value_is_given() {
        let language = resolve(&LanguageInputs {
            flag: None,
            env: None,
            config: None,
            locale: Some("ja_JP.UTF-8"),
        })
        .expect("the locale layer never fails");

        assert_eq!(language, Language::Japanese);
    }

    #[test]
    fn resolve_treats_auto_in_any_explicit_layer_as_a_jump_to_the_locale() {
        for inputs in [
            LanguageInputs {
                flag: Some("auto"),
                env: Some("en"),
                config: Some("en"),
                locale: Some("ja_JP.UTF-8"),
            },
            LanguageInputs {
                flag: None,
                env: Some("auto"),
                config: Some("en"),
                locale: Some("ja_JP.UTF-8"),
            },
            LanguageInputs {
                flag: None,
                env: None,
                config: Some("auto"),
                locale: Some("ja_JP.UTF-8"),
            },
        ] {
            let language = resolve(&inputs).expect("`auto` is a valid explicit value");

            assert_eq!(
                language,
                Language::Japanese,
                "unexpected result: {inputs:?}"
            );
        }
    }

    #[test]
    fn resolve_skips_lower_explicit_layers_after_auto() {
        // `auto` より下の明示層は参照されないため、そこに不正値があっても停止しない。
        let language = resolve(&LanguageInputs {
            flag: Some("auto"),
            env: Some("klingon"),
            config: Some("klingon"),
            locale: Some("en_US.UTF-8"),
        })
        .expect("layers below `auto` are not consulted at all");

        assert_eq!(language, Language::English);
    }

    #[test]
    fn resolve_rejects_invalid_values_in_the_explicit_layers() {
        for (origin, inputs) in [
            (
                LanguageSource::Flag,
                LanguageInputs {
                    flag: Some("jp"),
                    ..LanguageInputs::default()
                },
            ),
            (
                LanguageSource::Env,
                LanguageInputs {
                    env: Some("ja_JP.UTF-8"),
                    ..LanguageInputs::default()
                },
            ),
            (
                LanguageSource::Config,
                LanguageInputs {
                    config: Some("JA"),
                    ..LanguageInputs::default()
                },
            ),
        ] {
            let err = resolve(&inputs).expect_err("explicit layers must reject unknown values");

            assert!(
                matches!(err, LanguageError::InvalidValue { origin: actual, .. } if actual == origin),
                "unexpected error for {origin}: {err}"
            );
        }
    }

    #[test]
    fn resolve_accepts_unknown_locales_without_failing() {
        // 層 1〜3 と層 4 の非対称を固定する。層 4 は環境の記述であり、
        // 解釈できない値でも停止せずフォールバック言語へ進む。
        for locale in ["klingon", "C", "POSIX", "", "de_DE.UTF-8"] {
            let language = resolve(&LanguageInputs {
                locale: Some(locale),
                ..LanguageInputs::default()
            })
            .expect("the locale layer never fails");

            assert_eq!(
                language,
                Language::English,
                "unexpected result for {locale}"
            );
        }
    }

    #[test]
    fn resolve_falls_back_to_english_when_every_layer_is_absent() {
        let language =
            resolve(&LanguageInputs::default()).expect("an empty input never fails to resolve");

        assert_eq!(language, Language::English);
    }

    #[test]
    fn invalid_value_error_names_the_source_and_the_accepted_values() {
        let err = resolve(&LanguageInputs {
            flag: Some("jp"),
            ..LanguageInputs::default()
        })
        .expect_err("`jp` is not an accepted value");

        assert_eq!(
            err.to_string(),
            "invalid value for --lang: `jp` (expected `ja`, `en`, or `auto`)"
        );
    }

    #[test]
    fn scan_lang_flag_reads_the_separate_value_form() {
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "--lang", "ja", "branch"])),
            Some("ja".to_owned())
        );
    }

    #[test]
    fn scan_lang_flag_reads_the_equals_form() {
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "--lang=ja", "branch"])),
            Some("ja".to_owned())
        );
    }

    #[test]
    fn scan_lang_flag_reads_the_flag_after_a_subcommand() {
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "branch", "--lang", "ja"])),
            Some("ja".to_owned())
        );
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "branch", "--lang=ja"])),
            Some("ja".to_owned())
        );
    }

    #[test]
    fn scan_lang_flag_ignores_the_flag_after_the_terminator() {
        // 意図的に取りこぼす形。clap も `--` より後ろを引数として解釈しないため、
        // 乖離するのは clap 自身が出力するヘルプ・パーサエラーの言語だけになる。
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "log", "--", "--lang", "ja"])),
            None
        );
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "log", "--", "--lang=ja"])),
            None
        );
    }

    #[test]
    fn scan_lang_flag_returns_none_without_the_flag() {
        assert_eq!(scan_lang_flag(&os_args(&["gz", "branch"])), None);
        assert_eq!(scan_lang_flag(&[]), None);
    }

    #[test]
    fn scan_lang_flag_returns_none_when_the_value_is_missing() {
        assert_eq!(scan_lang_flag(&os_args(&["gz", "branch", "--lang"])), None);
    }

    #[test]
    fn scan_lang_flag_takes_the_last_occurrence() {
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "--lang", "ja", "--lang=en", "branch"])),
            Some("en".to_owned())
        );
    }

    #[test]
    fn scan_lang_flag_does_not_read_a_similar_looking_argument() {
        assert_eq!(
            scan_lang_flag(&os_args(&["gz", "--language", "ja"])),
            None,
            "only the exact spellings are recognised"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_lang_flag_ignores_a_non_utf8_value() {
        use std::os::unix::ffi::OsStringExt as _;

        let argv = vec![
            OsString::from("gz"),
            OsString::from("--lang"),
            OsString::from_vec(vec![0xff, 0xfe]),
        ];

        assert_eq!(scan_lang_flag(&argv), None);
    }

    #[test]
    fn language_env_input_passes_the_value_through() {
        assert_eq!(
            language_env_input(Some(OsStr::new("ja"))),
            Some("ja".to_owned())
        );
        assert_eq!(
            language_env_input(Some(OsStr::new("klingon"))),
            Some("klingon".to_owned()),
            "the validity of the value is decided by resolve()"
        );
    }

    #[test]
    fn language_env_input_treats_absent_and_empty_values_as_unset() {
        assert_eq!(language_env_input(None), None);
        assert_eq!(language_env_input(Some(OsStr::new(""))), None);
    }

    #[cfg(unix)]
    #[test]
    fn language_env_input_reports_a_non_utf8_value_as_an_invalid_value() {
        use std::os::unix::ffi::OsStringExt as _;

        let value = OsString::from_vec(vec![0xff, 0xfe]);

        let input = language_env_input(Some(&value)).expect("a non-utf8 value is not `unset`");
        let err = resolve(&LanguageInputs {
            env: Some(&input),
            ..LanguageInputs::default()
        })
        .expect_err("a non-utf8 value can never be `ja`, `en` or `auto`");

        assert!(matches!(
            err,
            LanguageError::InvalidValue {
                origin: LanguageSource::Env,
                ..
            }
        ));
    }

    #[test]
    fn first_non_empty_prefers_the_earlier_variable() {
        // 並びは LC_ALL → LC_MESSAGES → LANGUAGE → LANG。
        let selected = first_non_empty([
            Some("ja_JP.UTF-8".to_owned()),
            Some("en_US.UTF-8".to_owned()),
            None,
            Some("C".to_owned()),
        ]);

        assert_eq!(selected, Some("ja_JP.UTF-8".to_owned()));
    }

    #[test]
    fn first_non_empty_skips_empty_values() {
        let selected = first_non_empty([
            Some(String::new()),
            None,
            Some(String::new()),
            Some("ja_JP.UTF-8".to_owned()),
        ]);

        assert_eq!(selected, Some("ja_JP.UTF-8".to_owned()));
    }

    #[test]
    fn first_non_empty_returns_none_when_nothing_is_set() {
        assert_eq!(first_non_empty([None, None, None, None]), None);
        assert_eq!(
            first_non_empty([Some(String::new()), None, None, Some(String::new())]),
            None
        );
    }

    #[test]
    fn language_from_config_returns_none_when_the_key_is_absent() {
        let dir = TempDir::new("i18n-config-absent");
        init_repository(dir.path());
        let repository = open_isolated(dir.path());

        let value = language_from_config(Some(&repository)).expect("reading the snapshot succeeds");

        assert_eq!(value, None);
    }

    #[test]
    fn language_from_config_returns_the_configured_value() {
        let dir = TempDir::new("i18n-config-value");
        init_repository(dir.path());
        git_in(dir.path(), &["config", LANGUAGE_CONFIG_KEY, "ja"]);
        let repository = open_isolated(dir.path());

        let value = language_from_config(Some(&repository)).expect("reading the snapshot succeeds");

        assert_eq!(value, Some("ja".to_owned()));
    }

    #[test]
    fn language_from_config_reports_an_invalid_value_as_a_value() {
        // 妥当性の判定は resolve() の責務であり、取得層はそのまま値を返す
        // （「不在」と「不正値」が区別できることの確認）。
        let dir = TempDir::new("i18n-config-invalid");
        init_repository(dir.path());
        git_in(dir.path(), &["config", LANGUAGE_CONFIG_KEY, "klingon"]);
        let repository = open_isolated(dir.path());

        let value = language_from_config(Some(&repository)).expect("reading the snapshot succeeds");
        let err = resolve(&LanguageInputs {
            config: value.as_deref(),
            ..LanguageInputs::default()
        })
        .expect_err("`klingon` is not an accepted value");

        assert!(matches!(
            err,
            LanguageError::InvalidValue {
                origin: LanguageSource::Config,
                ..
            }
        ));
    }

    #[test]
    fn language_from_config_treats_an_empty_value_as_unset() {
        let dir = TempDir::new("i18n-config-empty");
        init_repository(dir.path());
        git_in(dir.path(), &["config", LANGUAGE_CONFIG_KEY, ""]);
        let repository = open_isolated(dir.path());

        let value = language_from_config(Some(&repository)).expect("reading the snapshot succeeds");

        assert_eq!(value, None);
    }

    #[test]
    fn language_from_config_reads_the_local_layer_of_the_repository() {
        // local 設定が効くことの確認。system / global の階層は開発者の環境に依存するため
        // ここでは検証せず、`gix` の設定スナップショットに委ねる。
        let dir = TempDir::new("i18n-config-local");
        init_repository(dir.path());
        git_in(
            dir.path(),
            &["config", "--local", LANGUAGE_CONFIG_KEY, "en"],
        );
        let repository = open_isolated(dir.path());

        let value = language_from_config(Some(&repository)).expect("reading the snapshot succeeds");

        assert_eq!(value, Some("en".to_owned()));
    }

    // 「読み取り失敗」（LanguageError::ConfigReadFailed）は、設定ファイルの I/O・パースが
    // 失敗したときに現れる。リポジトリ経由の経路ではその失敗はリポジトリを開いた時点で
    // 既に検出されており、値の取得時には起こらない。リポジトリ外の経路
    // （`File::from_globals()`）を失敗させるには HOME 等のプロセス全体の環境変数を
    // 差し替える必要があり、並列実行される単体テストからは安全に行えないため、
    // ここでは検証しない（型の上では `Result` で区別できている）。
}
