//! `gz` コマンドのエントリポイント。
//!
//! CLI をパースして [`fuzgit::commands::dispatch`] へ振り分け、
//! 結果に応じた終了コードを返すだけの薄い層に留める。
//!
//! ただし i18n（FR-25）の導入により、**リポジトリを開く位置と表示言語の決定は
//! `main` の責務**になっている。言語解決が `git config fuzgit.lang`（リポジトリ設定の
//! 階層）を必要とし、かつリポジトリ外でも成立しなければならないため、discover の成否は
//! その場でエラーにせず値として `dispatch` まで持ち回る（design.md「起動シーケンス」）。

use std::ffi::OsString;
use std::process::ExitCode;

use clap::FromArgMatches as _;
use fuzgit::cli::{self, Cli};
use fuzgit::commands;
use fuzgit::error::is_cancelled;
use fuzgit::git::repo;
use fuzgit::i18n::{self, Messages};

/// fuzzy finder 中断時の終了コード（128 + SIGINT(2)）。
const EXIT_CANCELLED: u8 = 130;

/// エラー終了時の終了コード。
const EXIT_FAILURE: u8 = 1;

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();

    // リポジトリを開くのはここ 1 回だけ。開けなかった場合もリポジトリ外での言語解決
    // （層 3 は system / global の設定から引く）を成立させるため、ここではエラーにしない
    let repository = repo::discover_from_current_dir();

    // clap のヘルプ・パーサエラーより前に言語を決める必要があるため、`Cli::parse()` の前に置く
    let language = match i18n::resolve_from_environment(&argv, repository.as_ref().ok()) {
        Ok(language) => language,
        Err(error) => {
            // 表示言語がまだ決まっていないため、このメッセージだけは英語で固定する
            // （フォールバック言語が en である以上、これは規定動作）
            eprintln!("error: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let messages = language.messages();

    // `Cli::parse()` を使わず組み立て済みの `Command` を通すのは、`--help` とパーサエラーを
    // 解決済みの言語で出すため（`cli::localized_command` が文言を差し替える）。
    // 先読みと同じ `argv` を渡し、両者が別の引数列を見る余地を残さない
    let matches = cli::localized_command(messages).get_matches_from(&argv);
    // `get_matches_from` を通過した `ArgMatches` の取り出しに失敗するのは定義と derive の
    // 不整合だけであり、その場合も clap の書式でエラーを出して終了する（panic させない）
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());

    match commands::dispatch(language, messages, repository, &cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 中断はユーザーの意図した操作であり異常ではないため、
            // メッセージを出さずに専用の終了コードで抜ける
            if is_cancelled(&error) {
                return ExitCode::from(EXIT_CANCELLED);
            }

            eprintln!(
                "{prefix}{chain}",
                prefix = messages.errors().prefix(),
                chain = format_error_chain(messages, &error)
            );
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// エラー連鎖を表示用の 1 行へ組み立てる。
///
/// `anyhow` の `{:#}` に任せず自前で組み立てるのは、**言語の混在を防ぐため**
/// （design.md「`main` は連鎖を自前で組み立てる」）。`fuzgit::error::Error` の `Display` は
/// 英語の開発者向け表示であり、そのまま出すと解決済みの表示言語と食い違う。
///
/// 連鎖の各要素は次のいずれかとして扱う:
///
/// - `fuzgit::error::Error` へ downcast できる要素: `ErrorMessages::describe` で訳す
/// - それ以外（`.context(…)` で付けた文字列や、`#[source]` に保持している外部クレートの
///   エラー）: 既に解決済みの言語で組み立てられている前提でそのまま用いる
///
/// 区切りは `anyhow` の `{:#}` と同じ `": "` とし、表示の見た目を変えない。
fn format_error_chain(messages: &dyn Messages, error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| match cause.downcast_ref::<fuzgit::error::Error>() {
            Some(domain) => messages.errors().describe(domain),
            None => cause.to_string(),
        })
        .collect::<Vec<String>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;
    use fuzgit::error::Error;
    use fuzgit::i18n::Language;

    use super::format_error_chain;

    /// 文字列に日本語の文字が含まれるかどうかを判定する。
    ///
    /// 英語を選んだときに日本語が混ざっていないことを確かめるための検査であり、
    /// 逆（日本語に英語が混ざらないこと）は検査しない。日本語の文言にも
    /// `git commit` のような**翻訳しない語**が意図的に含まれるため。
    fn contains_japanese(text: &str) -> bool {
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

    #[test]
    fn contains_japanese_detects_japanese_text() {
        assert!(contains_japanese("選択できる候補がありません"));
        assert!(contains_japanese(
            "リポジトリ情報の読み取りに失敗しました（列挙）"
        ));
        assert!(!contains_japanese(
            "There are no candidates to select from."
        ));
    }

    #[test]
    fn a_domain_error_is_described_in_the_selected_language() {
        let error = anyhow::Error::from(Error::NoCandidates);

        assert_eq!(
            format_error_chain(Language::Japanese.messages(), &error),
            "選択できる候補がありません"
        );
        assert_eq!(
            format_error_chain(Language::English.messages(), &error),
            "There are no candidates to select from"
        );
    }

    #[test]
    fn the_context_and_the_description_are_joined_in_order() {
        let error = Err::<(), _>(Error::GitNotFound)
            .context("ブランチ一覧の取得に失敗しました")
            .expect_err("must be an error");

        assert_eq!(
            format_error_chain(Language::Japanese.messages(), &error),
            concat!(
                "ブランチ一覧の取得に失敗しました: ",
                "git コマンドが見つかりません。git をインストールして PATH を通してください"
            )
        );
    }

    #[test]
    fn an_english_chain_contains_no_japanese_characters() {
        // 文脈も `Messages` 由来である（＝解決済みの言語で組み立てられている）前提のため、
        // 文脈は英語で与える。連結後もすべて英語のままであることを確かめる
        for error in [
            Err::<(), _>(Error::GitNotFound)
                .context("could not list the branches")
                .expect_err("must be an error"),
            Err::<(), _>(Error::UnbornHead {
                branch: "main".to_string(),
            })
            .context("could not read the commit history")
            .expect_err("must be an error"),
            anyhow::Error::from(Error::GitRunFailed {
                command: "git commit".to_string(),
                code: Some(1),
            }),
        ] {
            let chain = format_error_chain(Language::English.messages(), &error);

            assert!(
                !contains_japanese(&chain),
                "the english chain must stay in english: {chain}"
            );
        }
    }

    #[test]
    fn every_layer_of_the_chain_is_kept() {
        let error = Err::<(), _>(Error::Cancelled)
            .context("ブランチの選択中")
            .context("`gz branch` の実行中")
            .expect_err("must be an error");

        let chain = format_error_chain(Language::Japanese.messages(), &error);

        assert_eq!(
            chain,
            "`gz branch` の実行中: ブランチの選択中: 選択が中断されました"
        );
    }
}
