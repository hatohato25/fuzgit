//! `gz merge` — merge 対象のブランチを選択して merge する（FR-12）。
//!
//! merge 方式の二段目メニューは提供せず、clap の排他フラグ（`--no-ff` / `--squash` /
//! `--ff-only`）で指定する（requirements.md「スコープ外」）。
//! 候補一覧への ahead/behind 等の事前表示も行わない（判断材料は選択中候補のプレビューで示す）。
//!
//! merge / rebase が進行中の場合は、ブランチ選択ではなく復帰メニュー（FR-14、
//! [`crate::commands::in_progress`]）へ委譲する。

use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::commands::command_display;
use crate::commands::confirmation::confirm;
use crate::commands::in_progress;
use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::{MergeTreeOutcome, capture_git_with_status_in, run_git};
use crate::git::read::{BranchInfo, commit_count, operation_in_progress, other_branches};
use crate::git::repo::workdir;

/// merge 先（現在の位置）を指すリビジョン。
///
/// ブランチ名ではなく `HEAD` を使うのは、detached HEAD でも同じ式で「今いる位置」を
/// 指せるため。merge されるコミットの範囲・コンフリクト予測・実際の merge のいずれも
/// HEAD を基準に行われるため、表示と実行の基準がずれない。
const CURRENT_REVISION: &str = "HEAD";

/// プレビューに表示する最大コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// merge 対象になるブランチが 1 件も無い場合の案内。
///
/// 一般の「選択できる候補がありません」では、候補から現在のブランチを除いた結果である
/// ことが分からないため、原因を明示する。
const NO_CANDIDATES_MESSAGE: &str = "merge 対象になるブランチがありません。\
現在のブランチ以外のローカルブランチ・リモート追跡ブランチが必要です";

/// コンフリクト予測が得られなかった場合の注記。
///
/// 予測は補助情報であり、得られなくても merge 自体は実行できる。原因を断定せず、
/// 必要な git のバージョンだけを伝える（Git 2.38 未満では `--write-tree` が未知の
/// オプションとして拒否される）。
const PREDICTION_UNAVAILABLE_NOTE: &str = "コンフリクト予測: 省略しました\
（`git merge-tree --write-tree` を実行できませんでした。予測には Git 2.38 以降が必要です）";

/// コンフリクトなく merge できる見込みであることを示す注記。
const PREDICTION_CLEAN_NOTE: &str = "コンフリクト予測: コンフリクトなく merge できる見込みです";

/// コンフリクトは予測されたが、対象のファイル名が得られなかった場合の注記。
const PREDICTION_UNNAMED_NOTE: &str = "コンフリクト予測: コンフリクトが発生する見込みです\
（対象のファイル名は取得できませんでした）";

/// merge の方式。
///
/// `--no-ff` / `--squash` / `--ff-only` は相互排他であるため、真偽値 3 つを持ち回さず
/// 4 通りの方式を型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    /// fast-forward できる場合は fast-forward する通常の merge（既定）。
    Default,
    /// fast-forward できる場合でもマージコミットを作成する（`--no-ff`）。
    NoFf,
    /// 結果を作業ツリー・index へ反映するだけでコミットしない（`--squash`）。
    Squash,
    /// fast-forward できる場合のみ merge する（`--ff-only`）。
    FfOnly,
}

impl MergeMode {
    /// `--no-ff` / `--squash` / `--ff-only` の指定から方式を決める。
    ///
    /// 排他性は `clap` の `conflicts_with_all` でも担保しているが、複数が立った状態を
    /// 暗黙にどれか 1 つへ倒すことがないよう、ここでも明示的に拒否する
    /// （[`crate::commands::tag::TagAction::from_flags`] と同方針）。
    ///
    /// # Errors
    ///
    /// 2 つ以上が同時に指定された場合にエラーを返す。
    pub fn from_flags(no_ff: bool, squash: bool, ff_only: bool) -> Result<Self> {
        match (no_ff, squash, ff_only) {
            (false, false, false) => Ok(Self::Default),
            (true, false, false) => Ok(Self::NoFf),
            (false, true, false) => Ok(Self::Squash),
            (false, false, true) => Ok(Self::FfOnly),
            _ => bail!("`--no-ff` / `--squash` / `--ff-only` は同時に指定できません"),
        }
    }

    /// `git merge` に付けるオプション。既定の方式ではオプションを付けない。
    fn option(self) -> Option<&'static str> {
        match self {
            MergeMode::Default => None,
            MergeMode::NoFf => Some("--no-ff"),
            MergeMode::Squash => Some("--squash"),
            MergeMode::FfOnly => Some("--ff-only"),
        }
    }
}

/// `git merge-tree --write-tree` によるコンフリクト予測の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConflictPrediction {
    /// コンフリクトなく merge できる。
    Clean,
    /// コンフリクトが予測されるファイル（git が報告した順）。
    Conflicted(Vec<String>),
    /// 予測できなかった（Git 2.38 未満、または実行・解釈の失敗）。
    ///
    /// 予測は補助情報であるため、この場合もエラーで止めず merge の確認へ進む
    /// （requirements.md 共通要件）。
    Unavailable,
}

impl ConflictPrediction {
    /// 確認プロンプトに列挙する対象（予測されたコンフリクトファイル）。
    fn conflicted_files(&self) -> &[String] {
        match self {
            ConflictPrediction::Conflicted(paths) => paths,
            ConflictPrediction::Clean | ConflictPrediction::Unavailable => &[],
        }
    }
}

/// merge 対象のブランチを 1 件選び、確認のうえ `git merge` を実行する。
///
/// merge / rebase が進行中の場合はブランチ選択を行わず、復帰メニュー（FR-14）を表示する。
///
/// # Errors
///
/// ブランチ一覧の取得、選択（中断を含む）、コミット数の取得、`git merge` の実行に
/// 失敗した場合にエラーを返す。確認プロンプトで承認が得られなかった場合は
/// [`crate::error::Error::Cancelled`]。
pub fn run(repository: &gix::Repository, mode: MergeMode) -> Result<()> {
    // 進行中の merge / rebase を残したまま新しい merge は開始できないため、
    // 選択させる前に復帰メニューへ委譲する
    if let Some(operation) = operation_in_progress(repository) {
        return in_progress::run(repository, operation);
    }

    let candidates = other_branches(repository).context("ブランチ一覧の取得に失敗しました")?;
    if candidates.is_empty() {
        bail!("{NO_CANDIDATES_MESSAGE}");
    }

    let items = candidates.iter().map(to_item).collect();
    let selected = select_one(items)?;

    // `git merge` はブランチ名を位置引数に取り `--` で保護できないため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let branch = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!("選択されたブランチ `{selected}` が候補に見つかりません"))?;

    let workdir = workdir(repository)?;
    let count = commit_count(workdir, &merge_range(&branch.name)).with_context(|| {
        format!(
            "`{name}` から取り込まれるコミット数の取得に失敗しました",
            name = branch.name
        )
    })?;
    let prediction = predict_conflicts(workdir, &branch.name);

    let arguments = merge_args(mode, &branch.name);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let targets: Vec<&str> = prediction
        .conflicted_files()
        .iter()
        .map(String::as_str)
        .collect();
    confirm(
        &confirmation_header(&branch.name, count, &arguments, &prediction),
        &targets,
    )?;

    run_git(&arguments)
        .with_context(|| format!("`{name}` の merge に失敗しました", name = branch.name))?;

    Ok(())
}

/// merge されるコミットの範囲（`HEAD..<candidate>`）。
fn merge_range(branch: &str) -> String {
    format!("{CURRENT_REVISION}..{branch}")
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
///
/// 表示するのは `HEAD..<candidate>` = この merge で取り込まれるコミット。
fn preview_args(branch: &BranchInfo) -> Vec<String> {
    let range = merge_range(&branch.name);

    // 末尾の `--` により、リビジョンがパスとして解釈されることを防ぐ
    [
        "log",
        "--color=always",
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        &range,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// ブランチを finder の候補へ変換する。
///
/// 表示は名前だけとし、ahead/behind・diffstat 等を一覧へ事前表示しない
/// （全候補分の事前計算は初期表示の応答性を損なうため。requirements.md「スコープ外」。
/// 判断材料は選択中候補のプレビューで示す）。
fn to_item(branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        branch.name.clone(),
        branch.name.clone(),
        PreviewSource::Git(preview_args(branch)),
    )
}

/// `git merge [--no-ff|--squash|--ff-only] <branch>` の引数を組み立てる。
///
/// ブランチ名は gix が列挙した候補に由来する値だけを渡す
/// （`git merge` の位置引数は `--` で保護できないため、値の由来で担保する）。
fn merge_args(mode: MergeMode, branch: &str) -> Vec<String> {
    let mut args = vec!["merge".to_owned()];
    if let Some(option) = mode.option() {
        args.push(option.to_owned());
    }
    args.push(branch.to_owned());
    args
}

/// コンフリクト予測（merge のドライラン）用の `git merge-tree` の引数を組み立てる。
///
/// `--write-tree` は作業ツリー・index に触れず、結果のツリーオブジェクトを ODB へ
/// 書き込むだけ（不要なオブジェクトは git の gc が回収する）。
/// `--name-only -z` によりコンフリクトしたファイル名だけを NUL 区切りで得る。
fn merge_tree_args(branch: &str) -> Vec<String> {
    [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "-z",
        CURRENT_REVISION,
        branch,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// `git merge-tree --write-tree` でコンフリクトを予測する。
///
/// 予測は補助情報であるため、失敗しても [`ConflictPrediction::Unavailable`] を返して
/// 主要動作（merge の実行）を継続する（requirements.md 共通要件）。
///
/// Git 2.38 未満かどうかを `git --version` のパースで判定しないのは、
/// (a) 未対応の git では `--write-tree` が未知のオプションとして終了コード 129 で拒否され、
/// [`MergeTreeOutcome::from_exit_code`] が既に「エラー」と判定できること、
/// (b) 予測が得られない理由はバージョンだけではない（後から追加された `--name-only` の
/// 非対応、リポジトリ側の事情など）ため、バージョン文字列の解釈という別の失敗経路を
/// 増やしても網羅できないこと、による。
fn predict_conflicts(workdir: &Path, branch: &str) -> ConflictPrediction {
    let arguments = merge_tree_args(branch);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    let Ok((code, output)) = capture_git_with_status_in(workdir, &arguments) else {
        return ConflictPrediction::Unavailable;
    };

    match MergeTreeOutcome::from_exit_code(code) {
        MergeTreeOutcome::Clean => ConflictPrediction::Clean,
        MergeTreeOutcome::Conflicted => match parse_conflicted_files(&output) {
            Some(paths) => ConflictPrediction::Conflicted(paths),
            None => ConflictPrediction::Unavailable,
        },
        MergeTreeOutcome::Failed => ConflictPrediction::Unavailable,
    }
}

/// `git merge-tree --write-tree --name-only -z` の出力からコンフリクトファイル名を取り出す。
///
/// 出力は NUL 区切りで `<マージ結果のツリー OID>`、コンフリクトしたファイル名の並び、
/// 空レコード、情報メッセージ、の順に並ぶ（git 2.55 で実測）。ファイル名の区切りと
/// セクションの区切りが同じ NUL であるため、空レコードが現れた時点で打ち切る。
///
/// 得られた名前は確認プロンプトへ表示するだけで git へ渡さないため、UTF-8 でないパスは
/// ロッシー変換する（1 件のために予測全体を捨てない）。
/// 先頭の OID レコードが無い場合は想定と異なる出力として `None` を返す。
fn parse_conflicted_files(output: &[u8]) -> Option<Vec<String>> {
    let mut records = output.split(|byte| *byte == 0);

    let tree = records.next()?;
    if tree.is_empty() {
        return None;
    }

    Some(
        records
            .take_while(|record| !record.is_empty())
            .map(|record| String::from_utf8_lossy(record).into_owned())
            .collect(),
    )
}

/// 確認プロンプトの見出しを組み立てる。
///
/// 取り込まれるコミット数・実際に実行するコマンド・コンフリクト予測を示す。
/// コマンドは実行する引数配列そのものから作るため、説明と実際の操作が食い違わない。
fn confirmation_header(
    branch: &str,
    count: usize,
    arguments: &[&str],
    prediction: &ConflictPrediction,
) -> String {
    format!(
        "`{branch}` を merge します（取り込まれるコミット: {count} 件）\n\
         実行するコマンド: {command}\n\
         {note}",
        command = command_display(arguments),
        note = prediction_note(prediction)
    )
}

/// コンフリクト予測の結果を 1 行の注記にする。
fn prediction_note(prediction: &ConflictPrediction) -> String {
    match prediction {
        ConflictPrediction::Clean => PREDICTION_CLEAN_NOTE.to_owned(),
        // 終了コードはコンフリクトを示したが名前が 1 件も得られなかった場合、
        // 「コンフリクトしない」と誤読させないよう、発生の見込みだけは伝える
        ConflictPrediction::Conflicted(paths) if paths.is_empty() => {
            PREDICTION_UNNAMED_NOTE.to_owned()
        }
        ConflictPrediction::Conflicted(paths) => format!(
            "コンフリクト予測: 次の {count} 件のファイルで発生する見込みです",
            count = paths.len()
        ),
        ConflictPrediction::Unavailable => PREDICTION_UNAVAILABLE_NOTE.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::test_support::{TempDir, git_in, init_repository, write_file};

    fn local(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    fn conflicted(paths: &[&str]) -> ConflictPrediction {
        ConflictPrediction::Conflicted(paths.iter().map(|path| (*path).to_owned()).collect())
    }

    /// `shared.txt` の変更が衝突し、`only-main.txt` は衝突しない 2 ブランチを用意する。
    fn repository_with_diverged_branches(label: &str) -> TempDir {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        write_file(dir.path(), "shared.txt", "base\n");
        write_file(dir.path(), "other.txt", "base\n");
        git_in(dir.path(), &["add", "--all"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "base"]);

        git_in(dir.path(), &["switch", "--quiet", "-c", "feature"]);
        write_file(dir.path(), "shared.txt", "feature side\n");
        write_file(dir.path(), "other.txt", "feature side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "feature"]);

        git_in(dir.path(), &["switch", "--quiet", "main"]);
        write_file(dir.path(), "shared.txt", "main side\n");
        write_file(dir.path(), "other.txt", "main side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "main"]);

        dir
    }

    #[test]
    fn no_flag_merges_the_usual_way() {
        assert_eq!(
            MergeMode::from_flags(false, false, false).expect("no flag is always valid"),
            MergeMode::Default
        );
        assert_eq!(MergeMode::Default.option(), None);
        assert_eq!(
            merge_args(MergeMode::Default, "feature"),
            ["merge", "feature"]
        );
    }

    #[test]
    fn each_strategy_flag_adds_its_own_option() {
        for (no_ff, squash, ff_only, mode, option) in [
            (true, false, false, MergeMode::NoFf, "--no-ff"),
            (false, true, false, MergeMode::Squash, "--squash"),
            (false, false, true, MergeMode::FfOnly, "--ff-only"),
        ] {
            assert_eq!(
                MergeMode::from_flags(no_ff, squash, ff_only).expect("a single flag is valid"),
                mode
            );
            assert_eq!(mode.option(), Some(option));
            assert_eq!(
                merge_args(mode, "feature"),
                ["merge", option, "feature"],
                "the option must precede the branch"
            );
        }
    }

    #[test]
    fn combining_the_strategy_flags_is_rejected() {
        for (no_ff, squash, ff_only) in [
            (true, true, false),
            (true, false, true),
            (false, true, true),
            (true, true, true),
        ] {
            let err = MergeMode::from_flags(no_ff, squash, ff_only)
                .expect_err("the strategies are mutually exclusive");

            assert!(
                err.to_string().contains("同時に指定できません"),
                "unexpected error: {err:#}"
            );
        }
    }

    #[test]
    fn a_branch_containing_a_slash_keeps_its_full_name() {
        assert_eq!(
            merge_args(MergeMode::NoFf, "origin/feature/login"),
            ["merge", "--no-ff", "origin/feature/login"]
        );
    }

    #[test]
    fn the_preview_lists_the_commits_that_would_be_merged() {
        assert_eq!(
            preview_args(&local("feature")),
            [
                "log",
                "--color=always",
                "--oneline",
                "--decorate",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "HEAD..feature",
                "--"
            ],
            "the range starts at the current position"
        );
    }

    #[test]
    fn an_item_keeps_the_branch_name_as_its_key() {
        let item = to_item(&local("origin/main"));

        assert_eq!(item.key(), "origin/main");
    }

    #[test]
    fn the_dry_run_compares_the_current_position_with_the_candidate() {
        assert_eq!(
            merge_tree_args("feature"),
            [
                "merge-tree",
                "--write-tree",
                "--name-only",
                "-z",
                "HEAD",
                "feature"
            ]
        );
    }

    #[test]
    fn a_clean_dry_run_reports_only_the_resulting_tree() {
        // クリーンな merge の出力は「ツリー OID + NUL」だけ（git 2.55 で実測）
        let output = b"9b09a13822baf8df9427e5a197247eaa4790545d\0";

        assert_eq!(
            parse_conflicted_files(output).expect("the output has a tree record"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_conflicted_dry_run_lists_the_files_before_the_messages() {
        // OID・ファイル名の並び・空レコード・情報メッセージ、の順に NUL で区切られる
        let output = [
            &b"f7ab3c7f44322f7ce748c9ca53f63edb6ad56226\0"[..],
            &b"other.txt\0shared.txt\0"[..],
            &b"\0"[..],
            &b"1\0other.txt\0Auto-merging\0Auto-merging other.txt\n\0"[..],
        ]
        .concat();

        assert_eq!(
            parse_conflicted_files(&output).expect("the output has a tree record"),
            ["other.txt", "shared.txt"],
            "the informational messages must not be mistaken for file names"
        );
    }

    #[test]
    fn a_dry_run_output_without_a_tree_is_not_understood() {
        for output in [&b""[..], &b"\0other.txt\0"[..]] {
            assert_eq!(
                parse_conflicted_files(output),
                None,
                "an output without a tree record must not be reported as a prediction"
            );
        }
    }

    #[test]
    fn a_conflicting_path_that_is_not_utf8_is_still_listed() {
        // 予測の表示は git へ渡す値ではないため、1 件のために予測全体を捨てない
        let output = [
            &b"f7ab3c7f44322f7ce748c9ca53f63edb6ad56226\0"[..],
            &[0xff, 0xfe],
            &b"\0"[..],
        ]
        .concat();

        let paths = parse_conflicted_files(&output).expect("the output has a tree record");

        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn a_prediction_lists_the_files_that_are_expected_to_conflict() {
        let dir = repository_with_diverged_branches("merge-predict-conflict");

        let prediction = predict_conflicts(dir.path(), "feature");

        assert_eq!(
            prediction,
            conflicted(&["other.txt", "shared.txt"]),
            "both files were changed on either side"
        );
    }

    #[test]
    fn a_prediction_of_a_mergeable_branch_is_clean() {
        let dir = repository_with_diverged_branches("merge-predict-clean");
        git_in(dir.path(), &["switch", "--quiet", "-c", "clean", "feature"]);
        write_file(dir.path(), "only-clean.txt", "clean\n");
        git_in(dir.path(), &["add", "--all"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "clean"]);
        git_in(dir.path(), &["switch", "--quiet", "feature"]);

        assert_eq!(
            predict_conflicts(dir.path(), "clean"),
            ConflictPrediction::Clean
        );
    }

    #[test]
    fn only_the_conflicting_files_are_offered_as_confirmation_targets() {
        assert_eq!(
            conflicted(&["a.txt"]).conflicted_files(),
            ["a.txt".to_owned()]
        );
        assert!(ConflictPrediction::Clean.conflicted_files().is_empty());
        assert!(
            ConflictPrediction::Unavailable
                .conflicted_files()
                .is_empty()
        );
    }

    #[test]
    fn the_confirmation_names_the_branch_the_count_and_the_command() {
        let header = confirmation_header(
            "feature",
            3,
            &["merge", "--no-ff", "feature"],
            &ConflictPrediction::Clean,
        );

        assert!(header.contains("`feature`"), "unexpected header: {header}");
        assert!(header.contains("3 件"), "unexpected header: {header}");
        assert!(
            header.contains("git merge --no-ff feature"),
            "the command that runs must be shown as it is: {header}"
        );
    }

    #[test]
    fn the_confirmation_reports_a_clean_prediction() {
        assert_eq!(
            prediction_note(&ConflictPrediction::Clean),
            PREDICTION_CLEAN_NOTE
        );
    }

    #[test]
    fn the_confirmation_counts_the_files_that_are_expected_to_conflict() {
        assert_eq!(
            prediction_note(&conflicted(&["a.txt", "b.txt"])),
            "コンフリクト予測: 次の 2 件のファイルで発生する見込みです"
        );
    }

    #[test]
    fn the_confirmation_says_when_the_prediction_could_not_be_made() {
        let note = prediction_note(&ConflictPrediction::Unavailable);

        assert!(note.contains("2.38"), "unexpected note: {note}");
        assert!(
            !note.contains("コンフリクトなく"),
            "a missing prediction must not read as a clean one: {note}"
        );
    }

    #[test]
    fn the_confirmation_still_warns_when_no_file_name_was_reported() {
        assert_eq!(prediction_note(&conflicted(&[])), PREDICTION_UNNAMED_NOTE);
    }

    #[test]
    fn a_repository_with_no_other_branch_offers_nothing_to_merge() {
        let dir = TempDir::new("merge-no-candidates");
        init_repository(dir.path());
        write_file(dir.path(), "a.txt", "a\n");
        git_in(dir.path(), &["add", "--all"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "first"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err =
            run(&repository, MergeMode::Default).expect_err("the only branch is the current one");

        assert!(
            err.to_string()
                .contains("merge 対象になるブランチがありません"),
            "unexpected error: {err:#}"
        );
    }
}
