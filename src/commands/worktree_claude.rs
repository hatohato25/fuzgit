//! `gz worktree add` が作った worktree へエージェント設定（`.claude/`）を複写する。
//!
//! `git worktree add` が新しい作業ツリーへ書き出すのは**追跡ファイルだけ**であり、
//! gitignore されたディレクトリは現れない。`.claude/` は丸ごと gitignore されている
//! ことが多く（このリポジトリ自身がそうである）、worktree を作った直後には
//! `CLAUDE.md` も `do_prompt.md` も `agents/` も無い状態になる。作業を始める前に
//! 手で複写する 1 手を fuzgit が引き受ける（[`crate::commands::worktree_install`] が
//! 依存インストールについて同じ役割を負っているのと同型）。
//!
//! # 複写であり共有ではない
//!
//! シンボリックリンクではなく**ファイルの複写**にする。リンクにすると
//! `requirements.md` / `design.md` / `tasks.md` が worktree 間で共有され、並行して
//! 別の作業をしている worktree 同士が同じファイルを書き換え合う。worktree を分ける
//! 目的は作業を分けることであり、設定と作業記録も分かれているほうが目的に合う。
//!
//! # 既にあるものは触らない
//!
//! 複写先に同名のファイルが既にある場合は**上書きせず飛ばす**。この機能は
//! 「無いものを補う」ためのものであり、worktree 側で行われた編集を巻き戻す権利は無い。
//!
//! # 失敗しても worktree の作成は成功である
//!
//! 複写の成否は `gz worktree add` の終了コードに影響しない。worktree はすでに
//! 作られており、それを失敗として返すと呼び出し側（`&&` で繋いだシェル）に
//! 「worktree ができていない」と誤解させる（`worktree_install` と同じ判断）。
//! ただし**黙らない**。補われなかったことは利用者の知るべき結果であるため、
//! 警告を標準エラーへ書く。

use std::io::Write;
use std::path::Path;

use crate::i18n::Messages;

/// 複写するエージェント設定ディレクトリの名前。
///
/// リポジトリ内のファイルや環境変数から受け取った文字列がここへ入る経路は無い
/// （`&'static str` の定数であることがその証拠になっている）。
const AGENT_CONFIG_DIRECTORY: &str = ".claude";

/// 複写の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Report {
    /// 新しく複写したファイルの数。
    copied: usize,
    /// 既に同名のファイルがあったため触れなかった数。
    skipped: usize,
    /// 複写できなかったファイルの数。
    failed: usize,
}

/// `source` の `.claude/` を `destination` へ複写する。
///
/// `source` に `.claude/` が無ければ**何も出力せずに戻る**（補うものが無い状態は
/// 異常ではないため。`worktree_install` が lockfile を 1 つも見つけなかった場合と同じ扱い）。
///
/// 戻り値を持たないのは、複写の成否が呼び出し側の制御に影響しないためである
/// （module doc comment 参照）。結果は `writer` へ書く。
pub fn copy_agent_config(
    messages: &dyn Messages,
    source: &Path,
    destination: &Path,
    writer: &mut impl Write,
) {
    let from = source.join(AGENT_CONFIG_DIRECTORY);
    if !from.is_dir() {
        return;
    }

    let mut report = Report::default();
    copy_directory(
        messages,
        &from,
        &destination.join(AGENT_CONFIG_DIRECTORY),
        &mut report,
        writer,
    );

    // 1 件も動かなかった場合（すべて既存）でも結果を示す。「コピーされたはず」と
    // 思い込んだまま中身が違う、という取り違えを避けるため
    if report.copied > 0 || report.skipped > 0 {
        let _ = writeln!(
            writer,
            "{summary}",
            summary = messages
                .worktree()
                .agent_config_copied(report.copied, report.skipped)
        );
    }
}

/// ディレクトリを再帰的に複写する。
///
/// 途中で失敗しても中断せず、残りの複写を続ける。1 つのファイルが読めないことと
/// 「設定が 1 つも入らない」ことは別であり、前者で後者を招く理由が無いため。
fn copy_directory(
    messages: &dyn Messages,
    from: &Path,
    to: &Path,
    report: &mut Report,
    writer: &mut impl Write,
) {
    if let Err(error) = std::fs::create_dir_all(to) {
        warn(messages, to, &error.to_string(), report, writer);
        return;
    }

    let entries = match std::fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) => {
            warn(messages, from, &error.to_string(), report, writer);
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn(messages, from, &error.to_string(), report, writer);
                continue;
            }
        };

        let source = entry.path();
        let destination = to.join(entry.file_name());

        // シンボリックリンクは辿らない。`symlink_metadata` で判定するのは、
        // リンク先が複写元の外を指していた場合に予期しない場所を読みに行かないため
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) => {
                warn(messages, &source, &error.to_string(), report, writer);
                continue;
            }
        };

        if metadata.is_dir() {
            copy_directory(messages, &source, &destination, report, writer);
        } else if metadata.is_file() {
            copy_file(messages, &source, &destination, report, writer);
        }
        // シンボリックリンク・その他の種別は複写の対象にしない（黙って飛ばす）
    }
}

/// ファイルを 1 つ複写する。複写先に既にあれば触れない。
fn copy_file(
    messages: &dyn Messages,
    from: &Path,
    to: &Path,
    report: &mut Report,
    writer: &mut impl Write,
) {
    // `exists()` はリンク先を辿るため、リンクが壊れていると「無い」と判定され得る。
    // 壊れたリンクを上書きするのは触らない方針に反するので `symlink_metadata` で見る
    if std::fs::symlink_metadata(to).is_ok() {
        report.skipped += 1;
        return;
    }

    match std::fs::copy(from, to) {
        Ok(_) => report.copied += 1,
        Err(error) => warn(messages, from, &error.to_string(), report, writer),
    }
}

/// 複写できなかったことを警告として書き出す。
fn warn(
    messages: &dyn Messages,
    path: &Path,
    reason: &str,
    report: &mut Report,
    writer: &mut impl Write,
) {
    report.failed += 1;

    // 警告の書き出しに失敗しても複写を止めない（止めても利用者の得にならない）
    let _ = writeln!(
        writer,
        "{warning} ({reason})",
        warning = messages
            .worktree()
            .agent_config_copy_failed(&path.display().to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;
    use crate::test_support::{TempDir, write_file};

    /// 複写元となる `.claude/` を用意する。
    fn source_with_agent_config(directory: &Path) {
        write_file(directory, ".claude/CLAUDE.md", "project instructions");
        write_file(directory, ".claude/do_prompt.md", "task template");
        write_file(directory, ".claude/agents/planning_agent.md", "planner");
    }

    fn copy(source: &Path, destination: &Path) -> String {
        let mut written = Vec::new();
        copy_agent_config(
            Language::Japanese.messages(),
            source,
            destination,
            &mut written,
        );
        String::from_utf8(written).expect("the report is valid UTF-8")
    }

    #[test]
    fn the_whole_directory_is_copied_including_nested_files() {
        let source = TempDir::new("claude-copy-source");
        let destination = TempDir::new("claude-copy-destination");
        source_with_agent_config(source.path());

        let report = copy(source.path(), destination.path());

        for relative in [
            ".claude/CLAUDE.md",
            ".claude/do_prompt.md",
            ".claude/agents/planning_agent.md",
        ] {
            assert!(
                destination.path().join(relative).is_file(),
                "{relative} should have been copied"
            );
        }
        assert_eq!(
            std::fs::read_to_string(destination.path().join(".claude/CLAUDE.md"))
                .expect("the copy is readable"),
            "project instructions"
        );
        assert!(
            report.contains('3'),
            "the report should count them: {report}"
        );
    }

    #[test]
    fn an_existing_file_is_left_alone() {
        let source = TempDir::new("claude-copy-keep-source");
        let destination = TempDir::new("claude-copy-keep-destination");
        source_with_agent_config(source.path());
        write_file(
            destination.path(),
            ".claude/CLAUDE.md",
            "edited in the worktree",
        );

        let report = copy(source.path(), destination.path());

        // 上書きしないことがこの機能の前提。worktree 側の編集を巻き戻さない
        assert_eq!(
            std::fs::read_to_string(destination.path().join(".claude/CLAUDE.md"))
                .expect("the existing file is readable"),
            "edited in the worktree"
        );
        // 残りは複写される
        assert!(destination.path().join(".claude/do_prompt.md").is_file());
        assert!(!report.trim().is_empty(), "the skip should be reported");
    }

    #[test]
    fn nothing_is_written_when_there_is_no_agent_config() {
        let source = TempDir::new("claude-copy-absent-source");
        let destination = TempDir::new("claude-copy-absent-destination");

        let report = copy(source.path(), destination.path());

        // 補うものが無い状態は異常ではないため、何も出さない
        assert_eq!(report, "");
        assert!(!destination.path().join(".claude").exists());
    }

    #[test]
    fn a_symlink_is_not_followed() {
        let source = TempDir::new("claude-copy-symlink-source");
        let destination = TempDir::new("claude-copy-symlink-destination");
        source_with_agent_config(source.path());
        write_file(source.path(), "outside.txt", "must not be copied");
        std::os::unix::fs::symlink(
            source.path().join("outside.txt"),
            source.path().join(".claude/linked.md"),
        )
        .expect("the test can create a symlink");

        copy(source.path(), destination.path());

        assert!(
            !destination.path().join(".claude/linked.md").exists(),
            "a symlink must not be turned into a copy of what it points at"
        );
    }

    #[test]
    fn every_agent_config_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let worktree = language.messages().worktree();

            for text in [
                worktree.agent_config_copied(3, 0),
                worktree.agent_config_copied(3, 2),
            ] {
                assert!(
                    text.contains('3'),
                    "{language:?} must count the copies: {text}"
                );
            }
            assert!(
                worktree.agent_config_copied(3, 2).contains('2'),
                "{language:?} must count what was left alone"
            );
            assert!(
                worktree
                    .agent_config_copy_failed(".claude/CLAUDE.md")
                    .contains(".claude/CLAUDE.md"),
                "{language:?} must name the file"
            );
        }
    }

    #[test]
    fn the_agent_config_wording_is_translated() {
        let japanese = Language::Japanese.messages().worktree();
        let english = Language::English.messages().worktree();

        assert_ne!(
            japanese.agent_config_copied(1, 0),
            english.agent_config_copied(1, 0)
        );
        assert_ne!(
            japanese.agent_config_copied(1, 1),
            english.agent_config_copied(1, 1)
        );
        assert_ne!(
            japanese.agent_config_copy_failed("x"),
            english.agent_config_copy_failed("x")
        );
    }
}
