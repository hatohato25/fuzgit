//! `gz fetch` — リモートを選択して fetch する（FR-18）。
//!
//! fuzgit で初めてネットワークを伴うコマンドだが、ネットワークへ出るのは
//! ユーザーが決定したあとに継承 stdio で実行する `git fetch` の 1 回だけである。
//! 候補生成とプレビューはローカル情報（`.git/config` の URL と保存済みの
//! リモート追跡参照）のみを読む（design.md「候補生成・プレビューでネットワーク
//! アクセスを行わない」）。プレビューは選択項目ごとに都度実行されるため、
//! ここで往復遅延や認証プロンプトを挟むと描画がそのままブロックされる。
//!
//! タイムアウト・リトライ・認証情報の取り扱いは行わない。到達不能・認証拒否は
//! git の標準メッセージのまま非ゼロ終了する。

use anyhow::{Context as _, Result, anyhow, bail};

use crate::finder::{FinderItem, PreviewSource, select_one};
use crate::git::exec::run_git;
use crate::git::read::{remote_tracking_refs_args, remote_url_args, remotes};

/// 「すべてのリモート」を表す固定候補のキー。
///
/// リモート名は `refs/remotes/<name>/...` の一部になるため `git check-ref-format` の
/// 規則に従い、`*` を含むことができない。したがってこの識別子が実在のリモート名と
/// 衝突することはなく、選択結果の解決でリモートと取り違える余地もない
/// （FR-14 の復帰メニューと同じ固定キー方式）。
const ALL_REMOTES_KEY: &str = "*all*";

/// 「すべてのリモート」候補の表示。
const ALL_REMOTES_LABEL: &str = "すべてのリモート";

/// すべてのリモートから取得する `git fetch` のオプション。
const ALL_REMOTES_OPTION: &str = "--all";

/// リモートで削除されたブランチの追跡参照を掃除する `git fetch` のオプション。
const PRUNE_OPTION: &str = "--prune";

/// リモートが 1 つも登録されていない場合の案内（`gz push` と同じ扱い）。
const NO_REMOTE_MESSAGE: &str =
    "fetch 元のリモートが登録されていません。`git remote add <名前> <URL>` で追加してください";

/// プレビューでリモートの URL を示すセクションの見出し。
const URL_SECTION: &str = "リモート URL";

/// プレビューで既知のリモート追跡ブランチを示すセクションの見出し。
const TRACKING_SECTION: &str = "既知のリモート追跡ブランチ";

/// リモートで削除されたブランチの追跡参照を掃除するかどうか。
///
/// 掃除はローカルの参照を消す操作であるため、真偽値を持ち回さず
/// ユーザーの明示指定（`--prune`）だけで有効になることを型で表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneMode {
    /// 既存の追跡参照に触れない（既定）。
    Keep,
    /// リモートに存在しなくなった追跡参照を削除する（`--prune`）。
    Prune,
}

impl PruneMode {
    /// `git fetch` に付けるオプション。
    fn option(self) -> Option<&'static str> {
        match self {
            PruneMode::Keep => None,
            PruneMode::Prune => Some(PRUNE_OPTION),
        }
    }
}

/// fetch の対象。
///
/// 候補一覧はリモート名に固定候補「すべてのリモート」を加えたものであり、
/// 両者で `git fetch` へ渡す引数が異なるため、選択結果を型で区別して持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchTarget {
    /// 選択された 1 つのリモート。
    Remote(String),
    /// 登録されているすべてのリモート（`git fetch --all`）。
    All,
}

impl FetchTarget {
    /// エラーメッセージに示す対象の呼称。
    fn description(&self) -> String {
        match self {
            FetchTarget::Remote(name) => format!("リモート `{name}`"),
            FetchTarget::All => ALL_REMOTES_LABEL.to_owned(),
        }
    }
}

/// リモートを 1 件選び、`git fetch` を実行する。
///
/// 実行は継承 stdio で行い、更新された参照の一覧表示・認証プロンプト・進捗は git に委ねる。
///
/// # Errors
///
/// リモート一覧の取得、選択（中断を含む）、`git fetch` の実行に失敗した場合にエラーを返す。
/// リモートが 1 つも登録されていない場合は、追加方法を示して失敗する。
pub fn run(repository: &gix::Repository, prune: PruneMode) -> Result<()> {
    let remotes = remotes(repository).context("リモート一覧の取得に失敗しました")?;
    if remotes.is_empty() {
        bail!(NO_REMOTE_MESSAGE);
    }

    let selected = select_one(items(&remotes))?;

    // `git fetch` はパス以外の位置引数を取り `--` で保護できないため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let target = resolve(&remotes, &selected)?;

    let arguments = fetch_args(&target, prune);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(&arguments).with_context(|| {
        format!(
            "{target} からの取得に失敗しました",
            target = target.description()
        )
    })?;

    Ok(())
}

/// 候補（リモート一覧＋固定候補「すべてのリモート」）を組み立てる。
///
/// リモートが 1 つしか無い場合でも固定候補を出す。件数によって候補の並びが変わると、
/// 同じ位置に別の意味の項目が来てしまうため。
fn items(remotes: &[String]) -> Vec<FinderItem> {
    let mut items: Vec<FinderItem> = remotes.iter().map(|remote| to_item(remote)).collect();
    items.push(all_remotes_item(remotes));
    items
}

/// リモート 1 件を finder の候補へ変換する。
fn to_item(remote: &str) -> FinderItem {
    FinderItem::new(remote.to_owned(), remote.to_owned(), preview_source(remote))
}

/// リモート 1 件のプレビュー内容を組み立てる。
///
/// 参照するのは `.git/config` の URL と、前回までの fetch で保存済みのリモート追跡参照だけで、
/// いずれもネットワークを伴わない。生成は他コマンドと同じく選択項目ごとの遅延実行であり、
/// カーソルが当たっていない候補の分は実行されない。
fn preview_source(remote: &str) -> PreviewSource {
    PreviewSource::Composite(vec![
        (
            URL_SECTION.to_owned(),
            PreviewSource::Git(remote_url_args(remote)),
        ),
        (
            TRACKING_SECTION.to_owned(),
            PreviewSource::Git(remote_tracking_refs_args(remote)),
        ),
    ])
}

/// 「すべてのリモート」の固定候補を組み立てる。
fn all_remotes_item(remotes: &[String]) -> FinderItem {
    FinderItem::new(
        ALL_REMOTES_LABEL.to_owned(),
        ALL_REMOTES_KEY.to_owned(),
        all_remotes_preview(remotes),
    )
}

/// 「すべてのリモート」候補のプレビュー内容を組み立てる。
///
/// リモートごとの URL だけを並べる。追跡参照まで並べるとリモート数に比例して
/// プレビューの git 実行が増えるうえ、この候補で確かめたいこと（どのリモートへ
/// 問い合わせに行くのか）から離れるため。
fn all_remotes_preview(remotes: &[String]) -> PreviewSource {
    PreviewSource::Composite(
        remotes
            .iter()
            .map(|remote| (remote.clone(), PreviewSource::Git(remote_url_args(remote))))
            .collect(),
    )
}

/// 選択されたキーを fetch の対象へ解決する。
///
/// # Errors
///
/// キーが固定候補でも候補一覧のリモートでもない場合にエラーを返す
/// （対象を取り違えたまま git を実行しないよう、暗黙に読み飛ばさない）。
fn resolve(remotes: &[String], selected: &str) -> Result<FetchTarget> {
    if selected == ALL_REMOTES_KEY {
        return Ok(FetchTarget::All);
    }

    remotes
        .iter()
        .find(|remote| *remote == selected)
        .map(|remote| FetchTarget::Remote(remote.clone()))
        .ok_or_else(|| anyhow!("選択されたリモート `{selected}` が候補に見つかりません"))
}

/// `git fetch [--prune] <remote>` / `git fetch [--prune] --all` の引数を組み立てる。
///
/// リモート名は gix が列挙した候補に由来する値だけを渡す。
fn fetch_args(target: &FetchTarget, prune: PruneMode) -> Vec<String> {
    let mut args = vec!["fetch".to_owned()];
    if let Some(option) = prune.option() {
        args.push(option.to_owned());
    }

    match target {
        FetchTarget::Remote(name) => args.push(name.clone()),
        FetchTarget::All => args.push(ALL_REMOTES_OPTION.to_owned()),
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remotes() -> Vec<String> {
        ["origin", "upstream"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn fetching_a_remote_passes_its_name_as_it_was_listed() {
        assert_eq!(
            fetch_args(&FetchTarget::Remote("origin".to_owned()), PruneMode::Keep),
            ["fetch", "origin"]
        );
    }

    #[test]
    fn fetching_everything_uses_the_all_option_instead_of_a_name() {
        let arguments = fetch_args(&FetchTarget::All, PruneMode::Keep);

        assert_eq!(arguments, ["fetch", ALL_REMOTES_OPTION]);
        assert!(
            !arguments.iter().any(|argument| argument == ALL_REMOTES_KEY),
            "the finder key must never reach git: {arguments:?}"
        );
    }

    #[test]
    fn pruning_is_added_only_when_it_was_asked_for() {
        assert_eq!(
            fetch_args(&FetchTarget::Remote("origin".to_owned()), PruneMode::Prune),
            ["fetch", PRUNE_OPTION, "origin"]
        );
        assert_eq!(
            fetch_args(&FetchTarget::All, PruneMode::Prune),
            ["fetch", PRUNE_OPTION, ALL_REMOTES_OPTION]
        );
    }

    #[test]
    fn the_tracking_references_are_left_alone_unless_pruning_was_asked_for() {
        assert_eq!(PruneMode::Keep.option(), None);
        assert_eq!(PruneMode::Prune.option(), Some(PRUNE_OPTION));
    }

    #[test]
    fn a_remote_name_resolves_to_that_remote() {
        assert_eq!(
            resolve(&remotes(), "upstream").expect("a listed remote should resolve"),
            FetchTarget::Remote("upstream".to_owned())
        );
    }

    #[test]
    fn the_fixed_key_resolves_to_every_remote() {
        assert_eq!(
            resolve(&remotes(), ALL_REMOTES_KEY).expect("the fixed key should resolve"),
            FetchTarget::All
        );
    }

    #[test]
    fn a_name_outside_of_the_candidates_is_rejected() {
        let err = resolve(&remotes(), "elsewhere").expect_err("an unknown remote must be rejected");

        assert!(
            err.to_string().contains("elsewhere"),
            "the unknown remote should be named: {err:#}"
        );
    }

    #[test]
    fn the_label_of_the_fixed_candidate_is_not_mistaken_for_a_remote() {
        // 表示文字列と同じ名前のリモートがあっても、解決に使うのはキーだけ
        let remotes = vec![ALL_REMOTES_LABEL.to_owned()];

        assert_eq!(
            resolve(&remotes, ALL_REMOTES_LABEL).expect("the remote should resolve"),
            FetchTarget::Remote(ALL_REMOTES_LABEL.to_owned())
        );
        assert_eq!(
            resolve(&remotes, ALL_REMOTES_KEY).expect("the fixed key should resolve"),
            FetchTarget::All
        );
    }

    #[test]
    fn the_fixed_candidate_comes_after_the_remotes() {
        let items = items(&remotes());

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            ["origin", "upstream", ALL_REMOTES_KEY]
        );
    }

    #[test]
    fn a_single_remote_is_still_listed_next_to_the_fixed_candidate() {
        let items = items(&["origin".to_owned()]);

        assert_eq!(
            items.iter().map(FinderItem::key).collect::<Vec<_>>(),
            ["origin", ALL_REMOTES_KEY]
        );
    }

    /// プレビューのセクション（見出しと実行する git 引数）を取り出す。
    fn sections(source: &PreviewSource) -> Vec<(String, Vec<String>)> {
        let PreviewSource::Composite(sections) = source else {
            panic!("unexpected preview: {source:?}");
        };

        sections
            .iter()
            .map(|(label, source)| {
                let PreviewSource::Git(arguments) = source else {
                    panic!("`{label}` should run git: {source:?}");
                };
                (label.clone(), arguments.clone())
            })
            .collect()
    }

    #[test]
    fn a_preview_reads_local_information_only() {
        // ネットワークへ出るのは決定後の `git fetch` だけ（design.md の設計原則）
        let sections = sections(&preview_source("origin"));

        assert_eq!(
            sections
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            [URL_SECTION, TRACKING_SECTION]
        );
        assert_eq!(sections[0].1, remote_url_args("origin"));
        assert_eq!(sections[1].1, remote_tracking_refs_args("origin"));
    }

    #[test]
    fn no_preview_reaches_the_network() {
        let previews = [preview_source("origin"), all_remotes_preview(&remotes())];

        for preview in &previews {
            for (label, arguments) in sections(preview) {
                assert!(
                    matches!(arguments[0].as_str(), "remote" | "for-each-ref"),
                    "`{label}` must not reach the network: {arguments:?}"
                );
                assert!(
                    !arguments.iter().any(|argument| argument == "fetch"
                        || argument == "ls-remote"
                        || argument == "--dry-run"),
                    "`{label}` must not query the remote: {arguments:?}"
                );
            }
        }
    }

    #[test]
    fn the_preview_of_every_remote_lists_each_url_under_its_own_name() {
        let sections = sections(&all_remotes_preview(&remotes()));

        assert_eq!(
            sections
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["origin", "upstream"]
        );
        assert_eq!(sections[0].1, remote_url_args("origin"));
        assert_eq!(sections[1].1, remote_url_args("upstream"));
    }

    #[test]
    fn the_target_is_named_in_the_failure_message() {
        assert_eq!(
            FetchTarget::Remote("origin".to_owned()).description(),
            "リモート `origin`"
        );
        assert_eq!(FetchTarget::All.description(), ALL_REMOTES_LABEL);
    }
}
