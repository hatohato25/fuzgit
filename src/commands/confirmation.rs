//! 破壊的操作の実行前確認。
//!
//! `gz restore`（作業ツリーの変更破棄）と `gz stash drop`（stash の破棄）は
//! 元に戻せないため、対象を列挙したうえで明示的な同意を求める
//! （requirements.md「セキュリティ」/ design.md「セキュリティ設計」）。

use std::io::Write as _;

use anyhow::{Context as _, Result};

use crate::error::Error;
use crate::i18n::Messages;

/// 確認応答が承認かどうかを判定する。
///
/// 既定は否認（`[y/N]`）とし、`y` / `yes`（大文字小文字を問わない）のみを承認とみなす。
///
/// **受理集合は表示言語に依らず固定**であり、翻訳の対象にしない（design.md
/// 「commands（i18n 導入により更新）」）。`は` / `はい` まで受理すると、英語環境の利用者が
/// 意図せず破壊的操作を承認できてしまう入力が増えるため。
fn is_affirmative(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// 標準入力から確認応答を 1 行読む。
///
/// EOF（パイプ入力など）の場合は空文字列になり、[`is_affirmative`] で否認と判定される。
/// 応答を得られない状況で破壊的操作を続行しないための挙動。
fn read_answer(messages: &dyn Messages) -> Result<String> {
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context(messages.confirm().input_failed())?;
    Ok(answer)
}

/// `header` と `targets` を示したうえで実行の同意を求める。
///
/// `header` には「何が失われるのか」を、`targets` には対象を 1 件ずつ渡す。両者は対象の
/// 内容（ブランチ名・パス等）を含むため呼び出し側のコマンドが組み立て、この関数が出すのは
/// プロンプトと中止の通知だけである。
///
/// # Errors
///
/// 承認が得られなかった場合は [`Error::Cancelled`]（呼び出し側は git 操作を実行しないこと）、
/// 端末への出力・入力の読み取りに失敗した場合はそのエラーを返す。
pub fn confirm(messages: &dyn Messages, header: &str, targets: &[&str]) -> Result<()> {
    let confirm = messages.confirm();

    // 標準出力はパイプ利用のために空けておき、対話用の出力は標準エラーへ出す
    let mut stderr = std::io::stderr();
    writeln!(stderr, "{header}").context(confirm.output_failed())?;
    for target in targets {
        writeln!(stderr, "  {target}").context(confirm.output_failed())?;
    }
    write!(stderr, "{prompt}", prompt = confirm.prompt()).context(confirm.output_failed())?;
    stderr.flush().context(confirm.output_failed())?;

    if !is_affirmative(&read_answer(messages)?) {
        writeln!(stderr, "{cancelled}", cancelled = confirm.cancelled())
            .context(confirm.output_failed())?;
        return Err(Error::Cancelled.into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    #[test]
    fn only_an_explicit_yes_confirms_the_operation() {
        for answer in ["y\n", "Y\n", "yes\n", "YES\r\n", " y \n"] {
            assert!(is_affirmative(answer), "{answer:?} should be affirmative");
        }
    }

    #[test]
    fn anything_else_declines_the_operation() {
        for answer in ["", "\n", "n\n", "N\n", "no\n", "yeah\n", "ｙ\n", "1\n"] {
            assert!(!is_affirmative(answer), "{answer:?} should decline");
        }
    }

    #[test]
    fn the_accepted_answers_do_not_depend_on_the_display_language() {
        // 受理集合は `y` / `yes` のみで固定する（design.md「言語に依らず固定」）。
        // 日本語の肯定語まで受理すると、英語環境の利用者が意図せず破壊的操作を
        // 承認できてしまう入力が増える
        for answer in ["は\n", "はい\n", "ハイ\n", "うん\n", "j\n", "o\n", "oui\n"] {
            assert!(!is_affirmative(answer), "{answer:?} should decline");
        }
    }

    #[test]
    fn every_language_prompts_with_the_same_accepted_answers() {
        // 受理集合が固定である以上、どの言語のプロンプトも `[y/N]` を示す
        for language in [Language::Japanese, Language::English] {
            let prompt = language.messages().confirm().prompt();

            assert!(prompt.contains("[y/N]"), "{language:?}: {prompt}");
            assert!(
                !prompt.ends_with('\n'),
                "{language:?} must keep the cursor on the prompt line: {prompt:?}"
            );
        }
    }

    #[test]
    fn the_confirmation_wording_is_translated() {
        let japanese = Language::Japanese.messages().confirm();
        let english = Language::English.messages().confirm();

        assert_ne!(japanese.prompt(), english.prompt());
        assert_ne!(japanese.cancelled(), english.cancelled());
        assert_ne!(japanese.output_failed(), english.output_failed());
        assert_ne!(japanese.input_failed(), english.input_failed());
    }

    #[test]
    fn no_confirmation_wording_is_empty() {
        for language in [Language::Japanese, Language::English] {
            let confirm = language.messages().confirm();

            for text in [
                confirm.prompt(),
                confirm.cancelled(),
                confirm.output_failed(),
                confirm.input_failed(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }
        }
    }
}
