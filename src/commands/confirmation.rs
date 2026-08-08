//! 破壊的操作の実行前確認。
//!
//! `gz restore`（作業ツリーの変更破棄）と `gz stash drop`（stash の破棄）は
//! 元に戻せないため、対象を列挙したうえで明示的な同意を求める
//! （requirements.md「セキュリティ」/ design.md「セキュリティ設計」）。

use std::io::Write as _;

use anyhow::{Context as _, Result};

use crate::error::Error;

/// 確認応答が承認かどうかを判定する。
///
/// 既定は否認（`[y/N]`）とし、`y` / `yes`（大文字小文字を問わない）のみを承認とみなす。
fn is_affirmative(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// 標準入力から確認応答を 1 行読む。
///
/// EOF（パイプ入力など）の場合は空文字列になり、[`is_affirmative`] で否認と判定される。
/// 応答を得られない状況で破壊的操作を続行しないための挙動。
fn read_answer() -> Result<String> {
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("確認入力の読み取りに失敗しました")?;
    Ok(answer)
}

/// `header` と `targets` を示したうえで実行の同意を求める。
///
/// `header` には「何が失われるのか」を、`targets` には対象を 1 件ずつ渡す。
///
/// # Errors
///
/// 承認が得られなかった場合は [`Error::Cancelled`]（呼び出し側は git 操作を実行しないこと）、
/// 端末への出力・入力の読み取りに失敗した場合はそのエラーを返す。
pub fn confirm(header: &str, targets: &[&str]) -> Result<()> {
    // 標準出力はパイプ利用のために空けておき、対話用の出力は標準エラーへ出す
    let mut stderr = std::io::stderr();
    writeln!(stderr, "{header}").context("確認メッセージの出力に失敗しました")?;
    for target in targets {
        writeln!(stderr, "  {target}").context("確認メッセージの出力に失敗しました")?;
    }
    write!(stderr, "実行しますか? [y/N]: ").context("確認メッセージの出力に失敗しました")?;
    stderr
        .flush()
        .context("確認メッセージの出力に失敗しました")?;

    if !is_affirmative(&read_answer()?) {
        writeln!(stderr, "中止しました").context("確認メッセージの出力に失敗しました")?;
        return Err(Error::Cancelled.into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
