//! `gz worktree` — worktree の一覧・作成・削除・整理（FR-21）。
//!
//! 引数なしの `gz worktree` は一覧から 1 件選び、そのパスだけを標準出力へ出す
//! （`cd "$(gz worktree)"` のようなシェル連携用途。メッセージ類は標準エラーへ出す）。
//!
//! 一覧の読み取りは gix ではなく `git worktree list --porcelain -z` のキャプチャに倒している
//! （gix の `Repository::worktrees()` は main worktree を含まず、チェックアウト中のブランチも
//! 持たないため。design.md「追加機能の技術調査」）。
//!
//! move / lock / unlock / repair と `remove --force` は提供しない
//! （requirements.md「スコープ外」。いずれも素の git で 1 コマンドで済み、未コミット変更を
//! 破棄する強制削除は確認プロンプトだけでは担保しにくいため git に委ねる）。

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::cli::WorktreeCommand;
use crate::commands::confirmation::confirm;
use crate::commands::worktree_claude::copy_agent_config;
use crate::commands::worktree_install::{InstallMode, install_dependencies};
use crate::commands::{COLUMN_SEPARATOR, aligned_candidates, branch_manage, selection_header};
use crate::finder::{
    FinderItem, FinderOptions, PreviewSource, SelectionMode, select_one, select_one_with,
};
use crate::git::exec::{capture_git_stderr_in, run_git};
use crate::git::read::{
    BranchInfo, BranchScope, WorktreeInfo, branches, checked_out_branches, tags, worktrees,
};
use crate::git::repo::workdir;
use crate::i18n::{Language, Messages};

/// 整理対象を報告させる `git worktree prune` の引数（何も削除しない）。
///
/// `--verbose` を併せると、整理される worktree とその理由が 1 行ずつ報告される。
/// この報告は標準出力ではなく標準エラーへ出る（git 2.55 で実測。
/// [`capture_git_stderr_in`] を使うのはそのため）。
const PRUNE_DRY_RUN_ARGS: [&str; 4] = ["worktree", "prune", "--dry-run", "--verbose"];

/// プレビューに表示する最大コミット数。
const PREVIEW_COMMIT_COUNT: &str = "50";

/// main worktree（リポジトリ本体の作業ツリー）の表示。
const MAIN_LABEL: &str = "main";

/// linked worktree（`git worktree add` で追加した作業ツリー）の表示。
const LINKED_LABEL: &str = "linked";

/// 種別ラベルを揃える桁数（`linked` の 6 文字に合わせる）。
const KIND_WIDTH: usize = 6;

/// ブランチではなくコミットを直接チェックアウトしている worktree の表示。
const DETACHED_LABEL: &str = "detached";

/// 作業ツリーを持たない（bare）リポジトリ本体の表示。
const BARE_LABEL: &str = "bare";

/// ロックされている worktree に付ける印。
const LOCKED_MARK: &str = "locked";

/// `git worktree prune` の対象になり得る worktree に付ける印。
const PRUNABLE_MARK: &str = "prunable";

/// まだコミットが 1 件も無い worktree の `HEAD` 属性の値。
///
/// git はこの場合に全 0 のハッシュを報告する（man git-worktree「Porcelain Format」）。
/// 実在しないオブジェクトであるため、プレビューの `git log` は必ず失敗する。
const NULL_OBJECT_ID: &str = "0000000000000000000000000000000000000000";

/// worktree のサブコマンドを対応する処理へ振り分ける。
///
/// サブコマンドを省略した場合は一覧からの選択（パスの出力）を行う。
///
/// # Errors
///
/// 各操作が失敗した場合にエラーを返す。
pub fn run(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    command: Option<&WorktreeCommand>,
) -> Result<()> {
    match command {
        None => list(language, messages, repository),
        // `--no-install` の真偽値をここで型へ畳み、commands 層へ `bool` を持ち回さない
        // （既存の `PruneMode` / `FetchScope` と同方針）
        Some(WorktreeCommand::Add {
            name,
            branch,
            no_install,
        }) => add(
            language,
            messages,
            repository,
            name,
            branch.as_deref(),
            InstallMode::from_no_install(*no_install),
        ),
        Some(WorktreeCommand::Remove) => remove(language, messages, repository),
        Some(WorktreeCommand::Prune) => prune(language, messages, repository),
    }
}

/// worktree を 1 件選び、そのパスを標準出力へ書き出す。
///
/// # Errors
///
/// 一覧の取得、選択（中断を含む）、標準出力への書き込みに失敗した場合にエラーを返す。
fn list(language: Language, messages: &dyn Messages, repository: &gix::Repository) -> Result<()> {
    let worktrees = read_worktrees(messages, repository)?;
    let candidates: Vec<&WorktreeInfo> = worktrees.iter().collect();

    let selected = choose(language, messages, &candidates)?;

    // パイプ利用（`cd "$(gz worktree)"`）を想定し、stdout にはパス以外を混ぜない。
    // パイプ先が先に閉じた場合に panic しないよう、書き込みエラーは明示的に伝播する
    writeln!(std::io::stdout(), "{path}", path = selected.path)
        .context(messages.common().stdout_write_failed())?;

    Ok(())
}

/// ブランチを 1 件選び、新しい worktree を作成する。
///
/// 候補は他の worktree で使用中でないローカルブランチに限る。git は同じブランチを
/// 複数の worktree で同時にチェックアウトできないため、使用中のブランチを選べても
/// 必ず失敗する選択肢になるだけである。
///
/// 作成に成功したら、[`InstallMode::Run`] の場合に限り依存インストールを試みる
/// （FR-30。[`crate::commands::worktree_install`]）。**インストールの成否はこの関数の
/// 戻り値に影響しない。**worktree の作成そのものは成功しており、それを失敗として返すと
/// 呼び出し側（`&&` で繋いだシェル・スクリプト）に「worktree ができていない」と
/// 誤解させるためである。
///
/// # Errors
///
/// パスが UTF-8 でない場合、候補の取得・選択（中断を含む）、`git worktree add` の実行に
/// 失敗した場合にエラーを返す。作成後の worktree 一覧の読み直しに失敗した場合も
/// エラーを返す（この読み取りは fuzgit 自身の操作であり、インストールの失敗ではない）。
fn add(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    name: &str,
    new_branch: Option<&str>,
    install: InstallMode,
) -> Result<()> {
    // 置き場所は叩いた位置ではなくリポジトリルートの兄弟に固定する。worktree の中身は
    // 「`.git` のある階層まるごと」であり、その単位はリポジトリと並べたときに最も素直に
    // 読めるため（リポジトリの内側に作ると、本体からは未追跡の埋め込みリポジトリとして
    // 見え、`.gitignore` の手当てが要る）
    let path = resolve_new_name(messages, repository, name)?;
    let path = path.as_str();

    let arguments = match new_branch {
        // `-b` 無しは従来どおり「既存のローカルブランチを選ぶ」
        None => existing_branch_args(language, messages, repository, path)?,
        // `-b` 有りは「新しいブランチの作成元を選ぶ」。**選ばせる対象が入れ替わる**
        Some(new_branch) => new_branch_args(language, messages, repository, path, new_branch)?,
    };

    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).with_context(|| messages.worktree().creation_failed(path))?;

    finish_creation(messages, repository, path, install, PathReport::Needed)
}

/// 既存のローカルブランチを選んで `git worktree add` の引数を組み立てる（`-b` 無し）。
fn existing_branch_args(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    path: &str,
) -> Result<Vec<String>> {
    let worktrees = read_worktrees(messages, repository)?;
    let in_use = checked_out_branches(&worktrees);
    let locals = branches(repository, BranchScope::Local)
        .context(messages.common().branch_list_read_failed())?;
    let candidates = available_branches(&locals, &in_use);
    if candidates.is_empty() {
        // 行き止まりのエラーを残さない。`-b` を付ければ先へ進めることを案内する。
        // **候補ゼロを検出して暗黙に `-b` の動作へ倒すことはしない**
        //（名前を受け取る手段が無く、暗黙のフォールバック禁止にも反する）
        bail!(messages.worktree().no_available_branch());
    }

    let items = candidates
        .iter()
        .map(|branch| to_branch_item(language, branch))
        .collect();
    // `-b` の有無で選ばせる対象が入れ替わるため、候補行だけでは区別が付かない。
    // 何を選ぶのか・Enter で何が起きるのかをヘッダーで示す
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.worktree().add_header_subject(),
        messages.worktree().add_header_outcome(),
    ));
    let selected = select_one_with(items, &options)?;

    // ブランチ名は `--` の後ろに置いてもパスと区別できない位置に来るため、
    // 選択結果が候補一覧に含まれることを確かめてから引数に渡す（design.md セキュリティ設計）
    let branch = candidates
        .iter()
        .find(|candidate| candidate.name == selected)
        .ok_or_else(|| anyhow!(messages.worktree().branch_selection_not_found(&selected)))?;

    Ok(add_args(path, &branch.name))
}

/// 新しいブランチの作成元を選んで `git worktree add -b` の引数を組み立てる（FR-31）。
///
/// 候補の生成と表示は `gz branch create` と**実装ごと共有する**
/// （[`branch_manage::base_candidates`] / [`branch_manage::to_base_item`]）。
/// 同じ規則を 2 か所へ書くと、片方だけ直したときに挙動だけがずれるためである。
fn new_branch_args(
    language: Language,
    messages: &dyn Messages,
    repository: &gix::Repository,
    path: &str,
    new_branch: &str,
) -> Result<Vec<String>> {
    let all = branches(repository, BranchScope::All)
        .context(messages.common().branch_list_read_failed())?;

    // **finder を開く前に**衝突を弾く。選ばせたあとで「その名前は使えません」と
    // 告げない（`gz pr --worktree` で確立した方針）。ブランチ一覧は作成元候補の
    // 生成にも使うため、この検査に追加の git プロセスは要らない
    if branch_manage::collides_with_local_branch(&all, new_branch) {
        bail!(messages.worktree().branch_already_exists(new_branch));
    }

    let tags = tags(repository).context(messages.common().tag_list_read_failed())?;
    let candidates = branch_manage::base_candidates(&all, &tags);
    if candidates.is_empty() {
        // 選ぶものが無い画面を出さない
        bail!(messages.worktree().no_base_candidate());
    }

    let items = candidates
        .iter()
        .map(|candidate| branch_manage::to_base_item(language, candidate))
        .collect();
    let options = FinderOptions::new(SelectionMode::Single).with_header(selection_header(
        messages.worktree().add_branch_header_subject(),
        &messages.worktree().add_branch_header_outcome(new_branch),
    ));
    let selected = select_one_with(items, &options)?;

    let base = candidates
        .iter()
        .find(|candidate| candidate.key == selected)
        .ok_or_else(|| anyhow!(messages.branch_manage().base_selection_not_found(&selected)))?;

    Ok(add_with_branch_args(path, new_branch, &base.revision))
}

/// worktree を作ったあとの後処理をまとめて行う。
///
/// **作成の手段に依らない共通部分**である。`gz worktree add` は `git worktree add` で、
/// `gz pr --worktree`（FR-35）は `gh pr checkout --worktree` で作るが、作られたあとに
/// 行うこと——どこへ作られたかを知らせ、登録内容と照合し、`.claude/` を複写し、
/// 依存をインストールする——は同一である。
///
/// # Errors
///
/// worktree 一覧の読み直し、標準エラーへの書き込み、依存インストールに失敗した場合。
pub fn finish_creation(
    messages: &dyn Messages,
    repository: &gix::Repository,
    path: &str,
    install: InstallMode,
    report: PathReport,
) -> Result<()> {
    // 叩いた場所ではなくリポジトリの兄弟へ作るため、どこへ作られたのかを必ず知らせる。
    // 標準出力は使わない（`git worktree add` 自身が `HEAD is now at ...` を標準出力へ
    // 書くため、パスだけを取り出せる状態にはならない。実測で確認済み）
    if report == PathReport::Needed {
        writeln!(
            std::io::stderr(),
            "{message}",
            message = messages.worktree().created_at(path)
        )
        .context(messages.common().stderr_write_failed())?;
    }

    // 作業ディレクトリには利用者が打った文字列（`../feature` のような相対パス）を使わない。
    // 一覧を読み直して照合した登録済みパスを使う（design.md セキュリティ設計）。
    // **どこへ作られたかを fuzgit の推測ではなく git の登録内容で確かめる**という意図は、
    // 作成を `gh` に任せる FR-35 でこそ重要になる
    let created = created_worktree(messages, repository, path)?;
    let Some(directory) = created else {
        // 照合できない以上、複写もインストールも実行できない
        if install == InstallMode::Run {
            report_install_directory_not_found(messages, path, &mut std::io::stderr())?;
        }
        return Ok(());
    };

    // エージェント設定（`.claude/`）は gitignore されていることが多く、`git worktree add`
    // では現れない。**`--no-install` では抑止しない**（依存インストールと違って外部
    // コマンドを起動せず、ファイルを複写するだけであり、抑止したい理由が別であるため）
    copy_agent_config(
        messages,
        current_worktree(repository),
        &directory,
        &mut std::io::stderr(),
    );

    if install == InstallMode::Run {
        install_where_the_command_was_run(messages, repository, &directory)?;
    }

    Ok(())
}

/// 作成先のパスを fuzgit が知らせる必要があるかどうか。
///
/// 作成の手段によって変わる。`git worktree add` は**作成先のパスを出さない**ため
/// fuzgit が知らせる必要があるが、`gh pr checkout --worktree` は
/// `✓ Checked out PR #<n> in worktree <path>` を自ら出す（実測確認済み・T-478）。
/// そこで fuzgit も出すと同じパスが 2 行並ぶ。
///
/// 「外部コマンドが既に見せたものを再掲しない」という既存方針
/// （`gz fetch --siblings` が git の失敗理由を再掲しないのと同じ）に従い、
/// **どちらなのかを呼び出し側が明示する**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathReport {
    /// fuzgit が作成先のパスを知らせる（`git worktree add` 経由）。
    Needed,
    /// 作成したコマンドが既に知らせている（`gh pr checkout --worktree` 経由）。
    AlreadyReported,
}

/// 新しい worktree の名前を検査し、作成先の絶対パスを返す。
///
/// TODO: 検査の順序を見直す。現在は名前の妥当性 → 同名の存在の順だが、
/// 後者のほうが利用者にとって頻度が高い。
///
/// **選ばせる前に呼ぶ**ことを想定している。名前が使えない場合も、その名前の worktree が
/// 既にある場合も、finder を開いたあとに失敗させる理由が無い（FR-31 が同名ブランチを
/// finder の前で止めるのと同型）。
///
/// 受け取るのは**名前であってパスではない**。`gh pr checkout --worktree` はパスを取るが、
/// そこへ渡すのは利用者が打った文字列ではなく、ここで組み立てた値である
/// （自由入力パスで別のディレクトリを触らせないという既存方針を保つ）。
///
/// # Errors
///
/// 名前がディレクトリ名として使えない場合、worktree 一覧を読めない場合、
/// 同じ場所に worktree が既に登録されている場合にエラーを返す。
pub fn resolve_new_name(
    messages: &dyn Messages,
    repository: &gix::Repository,
    name: &str,
) -> Result<String> {
    let worktrees = read_worktrees(messages, repository)?;
    let destination = sibling_destination(messages, &worktrees, name)?;
    let path = destination
        .to_str()
        .ok_or_else(|| anyhow!(messages.worktree().path_not_utf8(&destination)))?
        .to_owned();

    if worktrees.iter().any(|worktree| worktree.path == path) {
        bail!(messages.worktree().already_exists(&path));
    }

    Ok(path)
}

/// `gz worktree add` を叩いた位置に対応するディレクトリで、依存インストールを実行する。
///
/// リポジトリルートから見た cwd の相対位置を、そのまま新しい worktree へ写す
/// （[`relative_prefix`]）。リポジトリルートで叩いた場合は相対位置が空になり、
/// worktree のルートを対象とする従来の挙動と完全に一致する。
///
/// 写した先が存在しない場合（選んだブランチにそのサブディレクトリがまだ無い等）は、
/// **worktree のルートへ暗黙に落とさず**、その旨を伝えて何もしない。ルートには利用者が
/// 意図したものと別の lockfile が置かれ得るためである。
///
/// # Errors
///
/// 標準エラーへの書き込みに失敗した場合にエラーを返す。
fn install_where_the_command_was_run(
    messages: &dyn Messages,
    repository: &gix::Repository,
    worktree: &Path,
) -> Result<()> {
    let Some(prefix) = relative_prefix(repository) else {
        // 相対位置を測れない（bare リポジトリから叩いた等）。worktree のルートを対象と
        // する従来の挙動をそのまま採る。写す基準が無い以上、これ以外の解釈が無いため
        install_dependencies(messages, worktree, &mut std::io::stderr());
        return Ok(());
    };

    let directory = worktree.join(&prefix);
    if !directory.is_dir() {
        writeln!(
            std::io::stderr(),
            "{message}",
            message = messages
                .worktree()
                .install_subdirectory_missing(&prefix.display().to_string())
        )
        .context(messages.common().stderr_write_failed())?;
        return Ok(());
    }

    install_dependencies(messages, &directory, &mut std::io::stderr());

    Ok(())
}

/// 現在の worktree のルートから見た cwd の相対位置。
///
/// `.git` がリポジトリの上位にある構成（例: `infrastructure/` がリポジトリルートで、
/// 実際に作業するのは `cdk/cyresource/she-cyresource/`）では、`git worktree add` が作るのは
/// **リポジトリ全体**であり、利用者が作業したいサブディレクトリは worktree のルートではなく
/// その内側に来る。インストールを走らせるべき位置もそこであるため、叩いた位置を写す。
///
/// 基準は主 worktree ではなく**cwd を含む worktree**である（linked worktree の中から
/// 別の worktree を作る場合、主 worktree ルートで測ると相対位置がずれる）。
///
/// パスは両側とも [`std::fs::canonicalize`] してから比較する。`git worktree list` が
/// シンボリックリンク解決済みの絶対パスを報告するのと同じ理由であり、`/tmp` と
/// `/private/tmp` のような差で strip に失敗することを避ける。
///
/// 相対位置を測れない場合（作業ツリーが無い・正規化に失敗した・cwd がルートの外）は
/// `None` を返す。推測で組み立てない。
fn relative_prefix(repository: &gix::Repository) -> Option<std::path::PathBuf> {
    let root = std::fs::canonicalize(repository.workdir()?).ok()?;
    let cwd = std::fs::canonicalize(std::env::current_dir().ok()?).ok()?;

    cwd.strip_prefix(&root).ok().map(Path::to_path_buf)
}

/// 作成する worktree の絶対パスを、リポジトリルートの兄弟として組み立てる。
///
/// `name` は**ディレクトリ名**であり、パスではない。区切りを含む名前を受け取ると
/// 「パスとして解釈されたが実際は別の場所に作られた」という取り違えが起きるため、
/// 受け付けずにエラーにする（暗黙に読み替えない）。
///
/// 基準は現在の worktree ではなく **main worktree**（`.git` の実体がある作業ツリー）である。
/// linked worktree の中から叩いた場合でも作成先が同じ場所になり、「どこで叩いても
/// リポジトリの隣に並ぶ」と説明できるため。
///
/// # Errors
///
/// 名前が空・パス区切りを含む・`.` や `..` である場合、main worktree が見つからない場合、
/// および main worktree に親ディレクトリが無い場合にエラーを返す。
fn sibling_destination(
    messages: &dyn Messages,
    worktrees: &[WorktreeInfo],
    name: &str,
) -> Result<std::path::PathBuf> {
    if !is_directory_name(name) {
        bail!(messages.worktree().name_is_not_a_directory_name(name));
    }

    let main = worktrees
        .iter()
        .find(|worktree| worktree.is_main)
        .ok_or_else(|| anyhow!(messages.worktree().main_worktree_not_found()))?;

    let root = Path::new(&main.path);
    let parent = root
        .parent()
        .ok_or_else(|| anyhow!(messages.worktree().no_parent_directory(&main.path)))?;

    Ok(parent.join(name))
}

/// `name` が単一のディレクトリ名として使えるかどうか。
///
/// パス区切りを含むもの、空文字、`.` / `..` を拒む。区切りの判定は `/` と
/// [`std::path::MAIN_SEPARATOR`] の両方で行う（Windows でも `/` が区切りとして通るため、
/// 片方だけでは擦り抜ける）。
fn is_directory_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains(std::path::MAIN_SEPARATOR)
}

/// 複写元とする、いま作業しているツリーのルート。
///
/// 主 worktree ではなく**現在の worktree**を複写元にする。利用者が見ている `.claude/` が
/// そのまま複写されるほうが結果を予測しやすく、linked worktree から
/// `gz worktree add` を実行した場合にも「手元にあるものが持って行かれる」で一貫するため。
fn current_worktree(repository: &gix::Repository) -> &Path {
    // bare リポジトリには作業ツリーが無く、そこには複写元となる `.claude/` も無い。
    // 存在しないパスを渡せば `copy_agent_config` が何もせずに戻る
    repository.workdir().unwrap_or(Path::new(""))
}

/// 作成した worktree の登録済みパスを、利用者が打ったパスとの照合を経て求める。
///
/// 照合には [`std::fs::canonicalize`] した利用者入力を使う。`git worktree list --porcelain`
/// が報告するのはシンボリックリンクを解決した絶対パスであり（git 2.55 で実測。cwd が
/// `/tmp/…` でも `/private/tmp/…` が報告される）、相対パスの解釈がプロセスのカレント
/// ディレクトリに依存する曖昧さもこれで消える。
///
/// 照合できない場合（一覧に無い・正規化できない）は `None` を返す。**推測でパスを
/// 組み立てない**（列挙済み候補との照合検証を経てから実行する既存方針と同型）。
///
/// # Errors
///
/// worktree 一覧の読み直しに失敗した場合にエラーを返す。直前に `git worktree add` が
/// 成功している以上ほぼ起こり得ないが、起きたのであればインストールの問題ではなく
/// fuzgit の読み取りの問題であるため、警告へ倒さず明示する。
fn created_worktree(
    messages: &dyn Messages,
    repository: &gix::Repository,
    path: &str,
) -> Result<Option<std::path::PathBuf>> {
    let worktrees = read_worktrees(messages, repository)?;

    // 作成直後であるため正規化は成功するはずだが、失敗したなら照合の根拠が無い。
    // 検証できないまま実行しないという点で「一覧に無い」場合と扱いは同じ
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return Ok(None);
    };

    Ok(worktrees
        .into_iter()
        .map(|worktree| std::path::PathBuf::from(worktree.path))
        .find(|registered| *registered == canonical))
}

/// 作成した worktree を一覧に見つけられず、インストールを行わないことを伝える。
///
/// 標準出力はパス出力のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_install_directory_not_found(
    messages: &dyn Messages,
    path: &str,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = messages.worktree().install_directory_not_found(path)
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

/// linked worktree を 1 件選び、確認のうえ削除する。
///
/// main worktree は `git worktree remove` の対象外であるため候補に含めない。
/// locked / 未コミット変更ありの worktree は git が拒否するため、その理由は git の
/// メッセージのまま表示する（fuzgit 側で判定を二重に実装しない）。
///
/// # Errors
///
/// 一覧の取得、選択（中断を含む）、`git worktree remove` の実行に失敗した場合にエラーを返す。
/// 確認プロンプトで承認が得られなかった場合は [`crate::error::Error::Cancelled`]。
fn remove(language: Language, messages: &dyn Messages, repository: &gix::Repository) -> Result<()> {
    let worktrees = read_worktrees(messages, repository)?;
    let candidates = removable(&worktrees);
    if candidates.is_empty() {
        bail!(messages.worktree().no_removable());
    }

    let selected = choose(language, messages, &candidates)?;

    // 削除されるのは作業ツリーのディレクトリごとであるため、対象を示して同意を求める
    confirm(
        messages,
        messages.worktree().remove_confirmation(),
        &[&display_line(selected)],
    )?;

    let arguments = remove_args(&selected.path);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments)
        .with_context(|| messages.worktree().removal_failed(&selected.path))?;

    Ok(())
}

/// 実体を失った worktree の管理情報を、確認のうえ整理する。
///
/// 整理対象の判定は fuzgit では行わず、`git worktree prune --dry-run --verbose` の報告を
/// そのまま提示する（design.md「stale 判定は git に委ねる」）。
///
/// # Errors
///
/// ドライランの実行、`git worktree prune` の実行に失敗した場合にエラーを返す。
/// 確認プロンプトで承認が得られなかった場合は [`crate::error::Error::Cancelled`]。
fn prune(language: Language, messages: &dyn Messages, repository: &gix::Repository) -> Result<()> {
    let report = capture_git_stderr_in(language, workdir(repository)?, &PRUNE_DRY_RUN_ARGS)
        .context(messages.worktree().prune_targets_read_failed())?;
    let targets = prune_targets(&report);

    if targets.is_empty() {
        return report_nothing_to_prune(messages, &mut std::io::stderr());
    }

    let lines: Vec<&str> = targets.iter().map(String::as_str).collect();
    confirm(messages, messages.worktree().prune_confirmation(), &lines)?;

    let arguments = prune_args();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    run_git(language, &arguments).context(messages.worktree().prune_failed())?;

    Ok(())
}

/// worktree 一覧を取得する。
///
/// # Errors
///
/// 作業ツリーが無い場合、`git worktree list` の実行・パースに失敗した場合にエラーを返す。
fn read_worktrees(
    messages: &dyn Messages,
    repository: &gix::Repository,
) -> Result<Vec<WorktreeInfo>> {
    worktrees(workdir(repository)?).context(messages.common().worktree_list_read_failed())
}

/// worktree を 1 件選び、候補一覧との照合を経た 1 件を返す。
///
/// # Errors
///
/// 選択（中断を含む）に失敗した場合、選択結果が候補一覧に無い場合にエラーを返す。
fn choose<'a>(
    language: Language,
    messages: &dyn Messages,
    candidates: &[&'a WorktreeInfo],
) -> Result<&'a WorktreeInfo> {
    let items = aligned_candidates(candidates, |worktree| cells(worktree))
        .into_iter()
        .map(|(worktree, line)| to_item(language, worktree, line))
        .collect();
    let selected = select_one(items)?;

    resolve(messages, candidates, &selected)
}

/// 選択されたパスを候補一覧の 1 件へ解決する。
///
/// `git worktree remove` へ渡すパスは、ユーザーの自由入力ではなく fuzgit が
/// `git worktree list` の出力から取り出した値に限る（design.md セキュリティ設計）。
///
/// # Errors
///
/// パスが候補一覧に無い場合にエラーを返す（対象を取り違えたまま git を実行しないよう、
/// 暗黙に読み飛ばさない）。
fn resolve<'a>(
    messages: &dyn Messages,
    candidates: &[&'a WorktreeInfo],
    selected: &str,
) -> Result<&'a WorktreeInfo> {
    candidates
        .iter()
        .find(|worktree| worktree.path == selected)
        .copied()
        .ok_or_else(|| anyhow!(messages.worktree().selection_not_found(selected)))
}

/// 削除できる worktree（main を除く linked worktree）を候補順のまま抽出する。
fn removable(worktrees: &[WorktreeInfo]) -> Vec<&WorktreeInfo> {
    worktrees
        .iter()
        .filter(|worktree| !worktree.is_main)
        .collect()
}

/// 新しい worktree に割り当てられるローカルブランチを候補順のまま抽出する。
fn available_branches<'a>(
    locals: &'a [BranchInfo],
    in_use: &std::collections::HashSet<String>,
) -> Vec<&'a BranchInfo> {
    locals
        .iter()
        .filter(|branch| !in_use.contains(&branch.name))
        .collect()
}

/// 一覧に表示する 1 行を列へ分解する。連結した文字列がそのまま絞り込みの対象になる。
///
/// パスを先頭に置くのは、`cd` 先を探す用途では絞り込みの手掛かりがパスになるため。
/// パスの長さは worktree ごとにまちまちであるため、列として返して
/// [`aligned_candidates`] に幅を揃えさせる（種別の開始位置がずれないように）。
/// 種別は候補が main だけでも桁が動かないよう [`KIND_WIDTH`] で先に揃えておく。
/// locked / prunable は該当する場合のみ印を付ける（該当しない旨は並べない）。
fn cells(worktree: &WorktreeInfo) -> Vec<String> {
    let mut cells = vec![
        worktree.path.clone(),
        format!(
            "{kind:<width$}",
            kind = kind_label(worktree),
            width = KIND_WIDTH
        ),
        state_label(worktree),
    ];

    if worktree.is_locked {
        cells.push(LOCKED_MARK.to_owned());
    }
    if worktree.prunable {
        cells.push(PRUNABLE_MARK.to_owned());
    }

    cells
}

/// 候補 1 件だけを示す 1 行を組み立てる（確認プロンプト用）。
///
/// 揃える相手が居ないため、列をそのまま連結する。
fn display_line(worktree: &WorktreeInfo) -> String {
    cells(worktree).join(COLUMN_SEPARATOR)
}

/// main worktree かどうかの表示。
fn kind_label(worktree: &WorktreeInfo) -> &'static str {
    if worktree.is_main {
        MAIN_LABEL
    } else {
        LINKED_LABEL
    }
}

/// 何をチェックアウトしているかの表示。
///
/// `git worktree list --porcelain` の `detached` / `bare` は、ブランチ・HEAD の有無で
/// 区別できるため [`WorktreeInfo`] が真偽値として持っていない（`git::read` の方針）。
/// 表示のためにここで読み替える。
fn state_label(worktree: &WorktreeInfo) -> String {
    match (&worktree.branch, &worktree.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(_)) => DETACHED_LABEL.to_owned(),
        // HEAD 属性そのものが無いのは bare なリポジトリ本体だけ
        (None, None) => BARE_LABEL.to_owned(),
    }
}

/// worktree を finder のアイテムへ変換する。
///
/// `line` は [`aligned_candidates`] が候補一覧全体で幅を揃えた表示行。
fn to_item(language: Language, worktree: &WorktreeInfo, line: String) -> FinderItem {
    FinderItem::new(
        line,
        worktree.path.clone(),
        preview_source(worktree),
        language.messages(),
    )
}

/// worktree のプレビュー内容（チェックアウト中の HEAD のコミットログ）を組み立てる。
///
/// linked worktree もリポジトリ本体とオブジェクトデータベースを共有するため、パスへ
/// 移動せずコミットのハッシュを指定するだけでログを読める。
/// 実体の無いコミット（bare・unborn HEAD）はログを引けないため、プレビューを出さない。
fn preview_source(worktree: &WorktreeInfo) -> PreviewSource {
    match &worktree.head {
        Some(head) if head != NULL_OBJECT_ID => PreviewSource::Git(log_preview_args(head)),
        Some(_) | None => PreviewSource::None,
    }
}

/// ブランチを finder のアイテムへ変換する（`gz worktree add` の候補）。
fn to_branch_item(language: Language, branch: &BranchInfo) -> FinderItem {
    FinderItem::new(
        branch.name.clone(),
        branch.name.clone(),
        PreviewSource::Git(log_preview_args(&branch.name)),
        language.messages(),
    )
}

/// プレビュー用の `git log --oneline` の引数を組み立てる。
fn log_preview_args(revision: &str) -> Vec<String> {
    // 末尾の `--` により、リビジョンがパスとして解釈されることを防ぐ
    [
        "log",
        "--color=always",
        "--oneline",
        "--decorate",
        "-n",
        PREVIEW_COMMIT_COUNT,
        revision,
        "--",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// `git worktree add -- <path> <branch>` の引数を組み立てる。
///
/// パスはユーザー入力（位置引数）であるため `--` の後ろへ置き、オプションとして
/// 解釈される余地を排除する。`git worktree add` が `--` を受け付けること、`-` で始まる
/// パスもその後ろでは通常のパスとして扱われることは git 2.55 で実機確認済み。
fn add_args(path: &str, branch: &str) -> Vec<String> {
    ["worktree", "add", "--", path, branch]
        .into_iter()
        .map(str::to_owned)
        .collect()
}
/// `git worktree add -b <name> -- <path> <start-point>` の引数を組み立てる（FR-31）。
///
/// **`-b` はオプションなので `--` より前に置く。**`<path>` と `<start-point>` はこの順の
/// 位置引数である（man git-worktree の SYNOPSIS）。`<path>` の `--` による保護は
/// [`add_args`] と同じく維持する。
///
/// **`-B` は使わない。**`-B` は既存ブランチを黙って作り直すため、
/// 事前検査（[`branch_manage::collides_with_local_branch`]）を擦り抜けた場合の
/// 最後の砦として git 自身に拒否させる。
fn add_with_branch_args(path: &str, new_branch: &str, start_point: &str) -> Vec<String> {
    ["worktree", "add", "-b", new_branch, "--", path, start_point]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git worktree remove -- <path>` の引数を組み立てる。
///
/// パスは `git worktree list` の出力に由来する値だが、`-` で始まるディレクトリを
/// 作ることはできるため、`--` の後ろへ置く点は [`add_args`] と揃える。
fn remove_args(path: &str) -> Vec<String> {
    ["worktree", "remove", "--", path]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git worktree prune` の引数を組み立てる。
fn prune_args() -> Vec<String> {
    ["worktree", "prune"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// `git worktree prune --dry-run --verbose` の報告を 1 行ずつ取り出す。
///
/// 報告は `Removing worktrees/<名前>: <理由>` の形式で、対象が無ければ空になる。
/// 内容の解釈（どれが整理されるのか）は git に委ね、fuzgit は行として確認プロンプトへ
/// 転記するだけであるため、UTF-8 でない場合もロッシー変換して捨てない。
fn prune_targets(report: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(report)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 整理する worktree が無いことを伝えて正常終了する。
///
/// 標準出力はパス出力のために空けておく（書き出し先は標準エラー）。
///
/// # Errors
///
/// 書き込みに失敗した場合にエラーを返す。
fn report_nothing_to_prune(
    messages: &dyn Messages,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    writeln!(
        writer,
        "{message}",
        message = messages.worktree().nothing_to_prune()
    )
    .context(messages.common().stderr_write_failed())?;

    Ok(())
}

#[cfg(test)]
mod tests {

    /// worktree の作成先は、叩いた場所ではなくリポジトリルートの兄弟になる。
    #[test]
    fn a_worktree_is_created_next_to_the_repository_root() {
        let worktrees = vec![main_worktree("/product/infrastructure")];

        let destination = sibling_destination(Language::Japanese.messages(), &worktrees, "sample")
            .expect("a name next to the root should resolve");

        assert_eq!(destination, Path::new("/product/sample"));
    }

    /// linked worktree の中から叩いても、基準は main worktree のままである。
    #[test]
    fn the_destination_is_measured_from_the_main_worktree() {
        let worktrees = vec![
            main_worktree("/product/infrastructure"),
            linked_worktree("/somewhere/else/other"),
        ];

        let destination = sibling_destination(Language::Japanese.messages(), &worktrees, "sample")
            .expect("the main work tree decides the destination");

        // linked worktree の隣（/somewhere/else/sample）にはならない
        assert_eq!(destination, Path::new("/product/sample"));
    }

    /// パスを渡せたように見えて別の場所に作られる、という取り違えを起こさない。
    #[test]
    fn a_name_with_a_path_separator_is_refused() {
        let worktrees = vec![main_worktree("/product/infrastructure")];

        for name in ["../sample", "a/b", "/absolute", ".", "..", ""] {
            let error = sibling_destination(Language::Japanese.messages(), &worktrees, name)
                .expect_err("{name} must not be accepted as a directory name");

            assert!(
                !error.to_string().trim().is_empty(),
                "{name} must be refused with a reason"
            );
        }
    }

    #[test]
    fn a_plain_directory_name_is_accepted() {
        for name in ["sample", "feature-1", "a.b", "-dashy"] {
            assert!(is_directory_name(name), "{name} should be a directory name");
        }
    }

    #[test]
    fn the_created_path_is_announced_in_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let message = language.messages().worktree().created_at("/product/sample");

            assert!(
                message.contains("/product/sample"),
                "{language:?} must name the location: {message}"
            );
        }

        assert_ne!(
            Language::Japanese.messages().worktree().created_at("/x"),
            Language::English.messages().worktree().created_at("/x")
        );
    }

    #[test]
    fn the_placement_wording_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let worktree = language.messages().worktree();

            assert!(worktree.name_is_not_a_directory_name("a/b").contains("a/b"));
            assert!(worktree.no_parent_directory("/").contains('/'));
            assert!(!worktree.main_worktree_not_found().trim().is_empty());
        }

        let japanese = Language::Japanese.messages().worktree();
        let english = Language::English.messages().worktree();
        assert_ne!(
            japanese.name_is_not_a_directory_name("x"),
            english.name_is_not_a_directory_name("x")
        );
        assert_ne!(
            japanese.no_parent_directory("x"),
            english.no_parent_directory("x")
        );
        assert_ne!(
            japanese.main_worktree_not_found(),
            english.main_worktree_not_found()
        );
    }

    /// 作成先の算出に使う main worktree の記録を組み立てる。
    fn main_worktree(path: &str) -> WorktreeInfo {
        WorktreeInfo {
            path: path.to_owned(),
            head: None,
            branch: None,
            is_main: true,
            is_locked: false,
            prunable: false,
        }
    }

    /// linked worktree の記録を組み立てる。
    fn linked_worktree(path: &str) -> WorktreeInfo {
        WorktreeInfo {
            is_main: false,
            ..main_worktree(path)
        }
    }

    /// リポジトリルートから叩いた場合、相対位置は空になり従来どおり worktree のルートが対象になる。
    #[test]
    fn running_at_the_repository_root_keeps_targeting_the_worktree_root() {
        let worktree = Path::new("/tmp/created");

        // 相対位置が空なら join しても変わらない、という関係をここで固定する
        assert_eq!(worktree.join(Path::new("")), worktree);
    }

    /// サブディレクトリから叩いた場合、その位置が新しい worktree の内側へ写される。
    #[test]
    fn running_in_a_subdirectory_moves_the_target_inside_the_new_worktree() {
        let worktree = Path::new("/tmp/created");
        let prefix = Path::new("cdk/cyresource/she-cyresource");

        assert_eq!(
            worktree.join(prefix),
            Path::new("/tmp/created/cdk/cyresource/she-cyresource")
        );
    }

    /// 実際のリポジトリで、cwd の相対位置が測れることを確かめる。
    #[test]
    fn the_relative_prefix_is_measured_from_the_worktree_that_holds_the_cwd() {
        use crate::git::repo::discover;
        use crate::test_support::{TempDir, commit, init_repository, write_file};

        let dir = TempDir::new("worktree-relative-prefix");
        init_repository(dir.path());
        write_file(
            dir.path(),
            "cdk/cyresource/she-cyresource/package.json",
            "{}",
        );
        commit(dir.path(), "add the nested package");

        let repository = discover(dir.path()).expect("the repository should open");
        let root = std::fs::canonicalize(
            repository
                .workdir()
                .expect("the test repository has a work tree"),
        )
        .expect("the work tree can be canonicalized");
        let nested = root.join("cdk/cyresource/she-cyresource");

        // `relative_prefix` はプロセスの cwd を読むため、ここでは同じ計算を値で固定する
        // （cwd の差し替えはテストの並列実行と両立しない）
        let prefix = std::fs::canonicalize(&nested)
            .expect("the nested directory exists")
            .strip_prefix(&root)
            .expect("the nested directory is inside the work tree")
            .to_path_buf();

        assert_eq!(prefix, Path::new("cdk/cyresource/she-cyresource"));
    }

    #[test]
    fn a_missing_subdirectory_is_reported_in_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let message = language
                .messages()
                .worktree()
                .install_subdirectory_missing("cdk/cyresource/she-cyresource");

            assert!(
                message.contains("cdk/cyresource/she-cyresource"),
                "{language:?} must name the directory: {message}"
            );
        }

        assert_ne!(
            Language::Japanese
                .messages()
                .worktree()
                .install_subdirectory_missing("x"),
            Language::English
                .messages()
                .worktree()
                .install_subdirectory_missing("x")
        );
    }
    use super::*;
    use std::collections::HashSet;

    fn worktree(path: &str, branch: Option<&str>, is_main: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: path.to_owned(),
            head: Some("1111111111111111111111111111111111111111".to_owned()),
            branch: branch.map(str::to_owned),
            is_main,
            is_locked: false,
            prunable: false,
        }
    }

    fn local(name: &str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            is_current: false,
            is_remote: false,
        }
    }

    fn in_use(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn adding_with_a_new_branch_puts_the_flag_before_the_separator() {
        // `-b` は**オプション**なので `--` より前に置く。`<path>` と `<start-point>` は
        // この順の位置引数である（man git-worktree の SYNOPSIS）
        let arguments = add_with_branch_args("/repos/wt", "feature/new", "main");

        assert_eq!(
            arguments,
            [
                "worktree",
                "add",
                "-b",
                "feature/new",
                "--",
                "/repos/wt",
                "main"
            ]
        );

        let separator = arguments
            .iter()
            .position(|argument| argument == "--")
            .expect("`--` が含まれること");
        let flag = arguments
            .iter()
            .position(|argument| argument == "-b")
            .expect("`-b` が含まれること");
        assert!(flag < separator, "`-b` は `--` より前に来ること");
    }

    #[test]
    fn adding_with_a_new_branch_never_uses_the_forcing_flag() {
        // `-B` は既存ブランチを黙って作り直す。事前検査を擦り抜けた場合の最後の砦として
        // git 自身に拒否させるため、強制の綴りは決して使わない
        let arguments = add_with_branch_args("/repos/wt", "feature", "main");

        assert!(
            !arguments.iter().any(|argument| argument == "-B"),
            "既存ブランチを作り直す `-B` を使ってはならない: {arguments:?}"
        );
    }

    #[test]
    fn adding_with_a_new_branch_passes_the_resolved_start_point() {
        // タグを選んだ場合、`revision` は解決済みのオブジェクト ID である
        let arguments = add_with_branch_args("/repos/wt", "from-tag", "0123456789abcdef");

        assert_eq!(
            arguments.last().map(String::as_str),
            Some("0123456789abcdef")
        );
    }

    #[test]
    fn adding_puts_the_path_and_the_branch_after_the_separator() {
        assert_eq!(
            add_args("../feature", "feature"),
            ["worktree", "add", "--", "../feature", "feature"]
        );
    }

    #[test]
    fn a_path_starting_with_a_dash_stays_a_path() {
        // `--` の後ろではオプションとして解釈されない（git 2.55 で実機確認済み）
        let arguments = add_args("-dashy", "feature");

        assert_eq!(arguments[2], "--");
        assert_eq!(arguments[3], "-dashy");
    }

    #[test]
    fn a_path_containing_spaces_is_passed_as_a_single_argument() {
        let arguments = add_args("../with space", "feature");

        assert_eq!(arguments[3], "../with space");
    }

    #[test]
    fn removing_puts_the_path_after_the_separator() {
        assert_eq!(
            remove_args("/repo/../feature"),
            ["worktree", "remove", "--", "/repo/../feature"]
        );
    }

    #[test]
    fn pruning_takes_no_option_when_it_actually_runs() {
        // ドライランと本実行で引数を取り違えないことを固定する
        assert_eq!(prune_args(), ["worktree", "prune"]);
        assert_eq!(
            PRUNE_DRY_RUN_ARGS,
            ["worktree", "prune", "--dry-run", "--verbose"]
        );
    }

    #[test]
    fn a_dry_run_without_a_report_means_there_is_nothing_to_prune() {
        for report in [&b""[..], &b"\n"[..], &b"  \n\n"[..]] {
            assert!(
                prune_targets(report).is_empty(),
                "unexpected targets for {report:?}"
            );
        }
    }

    #[test]
    fn every_reported_line_becomes_a_confirmation_target() {
        let report = b"Removing worktrees/gone: gitdir file points to non-existent location\n\
Removing worktrees/old: gitdir file points to non-existent location\n";

        assert_eq!(
            prune_targets(report),
            [
                "Removing worktrees/gone: gitdir file points to non-existent location",
                "Removing worktrees/old: gitdir file points to non-existent location"
            ],
            "the reasons reported by git must be shown as they are"
        );
    }

    #[test]
    fn a_report_that_is_not_utf8_is_still_shown() {
        let report = [&b"Removing worktrees/"[..], &[0xff, 0xfe], &b": gone\n"[..]].concat();

        assert_eq!(
            prune_targets(&report).len(),
            1,
            "a single undecodable byte must not hide the whole report"
        );
    }

    #[test]
    fn the_main_worktree_is_never_a_removal_candidate() {
        let worktrees = [
            worktree("/repo", Some("main"), true),
            worktree("/repo/../feature", Some("feature"), false),
        ];

        let candidates = removable(&worktrees);

        assert_eq!(
            candidates
                .iter()
                .map(|worktree| worktree.path.as_str())
                .collect::<Vec<&str>>(),
            ["/repo/../feature"]
        );
    }

    #[test]
    fn a_repository_without_a_linked_worktree_offers_nothing_to_remove() {
        let worktrees = [worktree("/repo", Some("main"), true)];

        assert!(removable(&worktrees).is_empty());
    }

    #[test]
    fn the_branches_used_by_another_worktree_are_not_offered() {
        let locals = [local("feature"), local("main"), local("wip")];

        let candidates = available_branches(&locals, &in_use(&["main", "wip"]));

        assert_eq!(
            candidates
                .iter()
                .map(|branch| branch.name.as_str())
                .collect::<Vec<&str>>(),
            ["feature"],
            "a branch checked out elsewhere can never be added"
        );
    }

    #[test]
    fn every_branch_is_offered_when_no_worktree_holds_one() {
        let locals = [local("feature"), local("main")];

        assert_eq!(available_branches(&locals, &in_use(&[])).len(), 2);
    }

    #[test]
    fn a_selected_path_is_resolved_against_the_candidates() {
        let worktrees = [
            worktree("/repo", Some("main"), true),
            worktree("/repo/../feature", Some("feature"), false),
        ];
        let candidates: Vec<&WorktreeInfo> = worktrees.iter().collect();

        let resolved = resolve(
            Language::Japanese.messages(),
            &candidates,
            "/repo/../feature",
        )
        .expect("a listed path resolves");

        assert_eq!(resolved.branch.as_deref(), Some("feature"));
    }

    #[test]
    fn a_path_outside_of_the_candidates_is_rejected() {
        let worktrees = [worktree("/repo", Some("main"), true)];
        let candidates: Vec<&WorktreeInfo> = worktrees.iter().collect();

        let err = resolve(Language::Japanese.messages(), &candidates, "/elsewhere")
            .expect_err("an unknown path must be rejected");

        assert!(
            err.to_string().contains("/elsewhere"),
            "the unknown path should be named: {err:#}"
        );
    }

    #[test]
    fn the_main_worktree_cannot_be_resolved_from_the_removal_candidates() {
        // 候補から外した main worktree のパスが、後段の解決で拾われないことを確かめる
        let worktrees = [
            worktree("/repo", Some("main"), true),
            worktree("/repo/../feature", Some("feature"), false),
        ];

        let err = resolve(
            Language::Japanese.messages(),
            &removable(&worktrees),
            "/repo",
        )
        .expect_err("the main worktree is excluded");

        assert!(
            err.to_string().contains("/repo"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn a_line_shows_the_path_the_kind_and_the_checked_out_branch() {
        assert_eq!(
            display_line(&worktree("/repo", Some("main"), true)),
            "/repo  main    main"
        );
        assert_eq!(
            display_line(&worktree("/repo/../feature", Some("feature/login"), false)),
            "/repo/../feature  linked  feature/login"
        );
    }

    #[test]
    fn the_kind_starts_at_the_same_column_across_the_list() {
        let lines: Vec<String> = aligned_candidates(
            &[
                worktree("/repo", Some("main"), true),
                worktree("/repo/../feature-login", Some("feature/login"), false),
            ],
            cells,
        )
        .into_iter()
        .map(|(_, line)| line)
        .collect();

        assert_eq!(
            lines,
            [
                "/repo                   main    main",
                "/repo/../feature-login  linked  feature/login",
            ]
        );
    }

    #[test]
    fn a_detached_worktree_is_shown_as_such() {
        let mut detached = worktree("/repo/detached", None, false);
        detached.branch = None;

        assert!(
            display_line(&detached).contains(DETACHED_LABEL),
            "unexpected line: {line}",
            line = display_line(&detached)
        );
    }

    #[test]
    fn a_bare_repository_is_distinguished_from_a_detached_worktree() {
        let mut bare = worktree("/repo.git", None, true);
        bare.head = None;

        assert_eq!(state_label(&bare), BARE_LABEL);
    }

    #[test]
    fn the_locked_and_prunable_marks_are_added_only_when_they_apply() {
        let plain = worktree("/repo/../feature", Some("feature"), false);
        assert!(!display_line(&plain).contains(LOCKED_MARK));
        assert!(!display_line(&plain).contains(PRUNABLE_MARK));

        let mut marked = plain.clone();
        marked.is_locked = true;
        marked.prunable = true;
        let line = display_line(&marked);
        assert!(line.contains(LOCKED_MARK), "unexpected line: {line}");
        assert!(line.contains(PRUNABLE_MARK), "unexpected line: {line}");
    }

    #[test]
    fn an_item_keeps_the_path_as_its_key() {
        // 決定時に標準出力へ出すのは表示行ではなくパスそのもの
        let candidate = worktree("/repo/../feature", Some("feature"), false);
        let item = to_item(Language::Japanese, &candidate, display_line(&candidate));

        assert_eq!(item.key(), "/repo/../feature");
    }

    #[test]
    fn the_preview_reads_the_log_of_the_checked_out_commit() {
        let worktree = worktree("/repo", Some("main"), true);

        assert_eq!(
            preview_source(&worktree),
            PreviewSource::Git(log_preview_args("1111111111111111111111111111111111111111"))
        );
    }

    #[test]
    fn a_worktree_without_a_reachable_commit_has_no_preview() {
        let mut bare = worktree("/repo.git", None, true);
        bare.head = None;
        assert_eq!(preview_source(&bare), PreviewSource::None);

        let mut unborn = worktree("/repo", Some("main"), true);
        unborn.head = Some(NULL_OBJECT_ID.to_owned());
        assert_eq!(
            preview_source(&unborn),
            PreviewSource::None,
            "an unborn HEAD points at no commit"
        );
    }

    #[test]
    fn the_preview_ends_with_a_path_separator() {
        assert_eq!(
            log_preview_args("main"),
            [
                "log",
                "--color=always",
                "--oneline",
                "--decorate",
                "-n",
                PREVIEW_COMMIT_COUNT,
                "main",
                "--"
            ]
        );
    }

    #[test]
    fn a_branch_item_keeps_its_name_as_the_key() {
        assert_eq!(
            to_branch_item(Language::Japanese, &local("feature/login")).key(),
            "feature/login"
        );
    }

    #[test]
    fn nothing_to_prune_is_reported_without_an_error() {
        let messages = Language::Japanese.messages();
        let mut output = Vec::new();

        report_nothing_to_prune(messages, &mut output).expect("writing to a buffer cannot fail");

        assert_eq!(
            String::from_utf8(output).expect("the message should be utf-8"),
            format!(
                "{message}\n",
                message = messages.worktree().nothing_to_prune()
            )
        );
    }

    #[test]
    fn every_worktree_message_is_filled_in_for_both_languages() {
        for language in [Language::Japanese, Language::English] {
            let worktree = language.messages().worktree();

            for text in [
                worktree.no_available_branch(),
                worktree.no_removable(),
                worktree.remove_confirmation(),
                worktree.prune_targets_read_failed(),
                worktree.prune_confirmation(),
                worktree.prune_failed(),
                worktree.nothing_to_prune(),
            ] {
                assert!(!text.trim().is_empty(), "{language:?} left a message empty");
            }

            assert!(
                worktree.selection_not_found("/repo").contains("/repo"),
                "{language:?} must name the selection"
            );
            assert!(
                worktree
                    .path_not_utf8(std::path::Path::new("/repo/../feature"))
                    .contains("feature"),
                "{language:?} must name the path"
            );
            assert!(
                worktree
                    .branch_selection_not_found("feature")
                    .contains("feature"),
                "{language:?} must name the selection"
            );
            assert!(
                worktree
                    .creation_failed("/repo/../feature")
                    .contains("/repo"),
                "{language:?} must name the worktree"
            );
            assert!(
                worktree
                    .removal_failed("/repo/../feature")
                    .contains("/repo"),
                "{language:?} must name the worktree"
            );
        }
    }

    #[test]
    fn the_worktree_wording_is_translated() {
        let japanese = Language::Japanese.messages().worktree();
        let english = Language::English.messages().worktree();
        let path = std::path::Path::new("/repo/../feature");

        assert_ne!(
            japanese.selection_not_found("/repo"),
            english.selection_not_found("/repo")
        );
        assert_ne!(japanese.path_not_utf8(path), english.path_not_utf8(path));
        assert_ne!(
            japanese.no_available_branch(),
            english.no_available_branch()
        );
        assert_ne!(
            japanese.branch_selection_not_found("feature"),
            english.branch_selection_not_found("feature")
        );
        assert_ne!(
            japanese.creation_failed("/repo"),
            english.creation_failed("/repo")
        );
        assert_ne!(japanese.no_removable(), english.no_removable());
        assert_ne!(
            japanese.remove_confirmation(),
            english.remove_confirmation()
        );
        assert_ne!(
            japanese.removal_failed("/repo"),
            english.removal_failed("/repo")
        );
        assert_ne!(
            japanese.prune_targets_read_failed(),
            english.prune_targets_read_failed()
        );
        assert_ne!(japanese.prune_confirmation(), english.prune_confirmation());
        assert_ne!(japanese.prune_failed(), english.prune_failed());
        assert_ne!(japanese.nothing_to_prune(), english.nothing_to_prune());
        assert_ne!(
            japanese.install_directory_not_found("/repo/../feature"),
            english.install_directory_not_found("/repo/../feature")
        );
    }

    #[test]
    fn the_created_worktree_is_resolved_to_the_path_git_registered() {
        let (_dir, repository, root) = repository_with_worktree("worktree-created");
        let created = root.join("feature");

        let resolved = created_worktree(
            Language::English.messages(),
            &repository,
            created.to_str().expect("the temp path must be UTF-8"),
        )
        .expect("the list must be readable right after the worktree was created");

        // 一時ディレクトリの親（macOS の `/var` 等）がシンボリックリンクである環境では、
        // git が報告するのは解決済みの絶対パスであり、利用者入力を正規化して初めて一致する
        assert_eq!(
            resolved,
            Some(
                std::fs::canonicalize(&created)
                    .expect("the created worktree must exist on the filesystem")
            )
        );
    }

    #[test]
    fn a_path_that_git_never_registered_resolves_to_nothing() {
        let (_dir, repository, root) = repository_with_worktree("worktree-unregistered");

        for candidate in [root.join("feature").join("src"), root.join("absent")] {
            let resolved = created_worktree(
                Language::English.messages(),
                &repository,
                candidate.to_str().expect("the temp path must be UTF-8"),
            )
            .expect("the list must stay readable");

            assert_eq!(
                resolved, None,
                "only a registered worktree may become the install directory: {candidate:?}"
            );
        }
    }

    /// linked worktree を 1 つ持つリポジトリと、その親ディレクトリを用意する。
    fn repository_with_worktree(
        label: &str,
    ) -> (
        crate::test_support::TempDir,
        gix::Repository,
        std::path::PathBuf,
    ) {
        use crate::test_support::{TempDir, commit, create_branch, git_in, init_repository};

        let dir = TempDir::new(label);
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).expect("the main worktree must be creatable");
        init_repository(&main);
        commit(&main, "initial");
        create_branch(&main, "feature");
        git_in(&main, &["worktree", "add", "--", "../feature", "feature"]);

        let repository = gix::open_opts(&main, gix::open::Options::isolated())
            .expect("the initialized repository must be openable");
        let root = dir.path().to_path_buf();

        (dir, repository, root)
    }

    #[test]
    fn the_report_of_the_dry_run_is_never_translated() {
        // 報告は git の出力そのものであり、fuzgit は行に分割するだけで解釈も翻訳もしない
        // （design.md「`capture_git_stderr_in` を (B) とする根拠」）
        let report = b"Removing worktrees/gone: gitdir file points to non-existent location\n";

        assert_eq!(
            prune_targets(report),
            ["Removing worktrees/gone: gitdir file points to non-existent location"]
        );
    }
}
