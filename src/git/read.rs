//! リポジトリ情報の読み取り。
//!
//! ブランチ・コミット・タグ・reflog・変更ファイルの一覧を、`gix` の型を
//! `commands` 層へ漏らさないプレーンな構造体として返す。
//!
//! 基本は `gix` で読むが、作業ツリーの状態（`git status`）とツリーのファイル一覧
//! （`git ls-tree`）は `git` コマンドのキャプチャで取得する。gix の status 実装は
//! 設定（`status.renames` 等）の解釈まで含めると挙動互換の担保が難しく、
//! ここでは git 本体の判定をそのまま利用するほうが確実なため。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use gix::bstr::{BStr, ByteSlice as _};

use crate::error::{Error, Result};
use crate::git::exec::{capture_git_in, capture_git_with_status_in};
use crate::git::repo::workdir;

/// 候補に含めるブランチの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchScope {
    /// ローカルブランチ（`refs/heads/`）のみを対象にする。
    Local,
    /// ローカルブランチに加えてリモート追跡ブランチ（`refs/remotes/`）も対象にする。
    All,
}

/// ブランチ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// 短縮名。ローカルは `main`、リモート追跡は `origin/main` の形式。
    pub name: String,
    /// HEAD が指しているブランチかどうか。
    pub is_current: bool,
    /// リモート追跡ブランチかどうか。
    pub is_remote: bool,
}

/// 候補に含めるコミットの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitScope<'a> {
    /// HEAD から辿れるコミットのみを対象にする（`gz log`）。
    Head,
    /// すべてのブランチ（ローカル・リモート追跡）の先端から辿れるコミットを対象にする。
    AllBranches,
    /// 指定したブランチ（リビジョン）から辿れるコミットを対象にする。
    Branch(&'a str),
}

/// コミット 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// フルハッシュ。
    pub id: String,
    /// `core.abbrev` に従って短縮したハッシュ。
    pub short_id: String,
    /// コミットメッセージの 1 行目。
    pub summary: String,
    /// 作者名。
    pub author: String,
    /// 作者日時。コミットに記録されたタイムゾーンでの `YYYY-MM-DD` 表記。
    pub time: String,
}

/// リポジトリ情報の読み取り操作。
///
/// [`Error::RepositoryReadFailed`] / [`Error::GitOutputMalformed`] が「何の読み取りに
/// 失敗したか」を**表示済みの文字列ではなく値として**保持するための型（design.md
/// 「`Error` は表示済みの文字列を保持しない」）。表示は
/// [`crate::i18n::messages::ErrorMessages::describe`] が担う。
///
/// バリアントを追加すると [`crate::i18n::ja`] / [`crate::i18n::en`] の網羅的な `match` が
/// 双方ともコンパイルエラーになる。翻訳漏れを実行時ではなくコンパイル時に検出するための
/// 設計であり、翻訳側にワイルドカードの腕を足してはならない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOperation {
    /// HEAD の読み取り。
    HeadRead,
    /// HEAD が指すブランチ名の解釈。
    HeadBranchNameDecode,
    /// HEAD が指すコミットの解決。
    HeadResolve,
    /// 参照の列挙。
    ReferenceList,
    /// ローカルブランチの列挙。
    LocalBranchList,
    /// ローカルブランチ 1 件の読み取り。
    LocalBranchRead,
    /// ローカルブランチ名の解釈。
    LocalBranchNameDecode,
    /// リモート追跡ブランチの列挙。
    RemoteBranchList,
    /// リモート追跡ブランチ 1 件の読み取り。
    RemoteBranchRead,
    /// リモート追跡ブランチ名の解釈。
    RemoteBranchNameDecode,
    /// リモート名の解釈。
    RemoteNameDecode,
    /// ブランチ名を参照名として解釈すること。
    BranchNameParse {
        /// 対象のブランチ名。
        branch: String,
    },
    /// リモートの URL の解釈。
    RemoteUrlDecode,
    /// ブランチの upstream の解決。
    UpstreamResolve {
        /// 対象のブランチ名。
        branch: String,
    },
    /// upstream の参照名の解釈。
    UpstreamRefNameDecode,
    /// `git rev-list` の出力の解釈。
    RevListOutputParse,
    /// ブランチ 1 件の読み取り（ローカル・リモート追跡を区別しない走査）。
    BranchRead,
    /// ブランチ先端のコミットの解決。
    BranchTipResolve,
    /// ブランチの解決。
    BranchResolve {
        /// 対象のブランチ名。
        branch: String,
    },
    /// コミット履歴の走査。
    CommitHistoryWalk,
    /// コミットオブジェクトの取得。
    CommitObjectFetch,
    /// 短縮ハッシュの算出。
    ShortIdCompute,
    /// コミットメッセージの解釈。
    CommitMessageDecode,
    /// コミット作者の解釈。
    CommitAuthorDecode,
    /// コミット日時の解釈。
    CommitTimeDecode,
    /// コミット日時の整形。
    CommitTimeFormat,
    /// コミットサマリの解釈。
    CommitSummaryDecode,
    /// コミット作者名の解釈。
    CommitAuthorNameDecode,
    /// `git status` の出力の解釈。
    StatusOutputParse,
    /// パスの解釈。
    PathDecode,
    /// リネーム元のパスの解釈。
    RenameOriginPathDecode,
    /// リビジョンの解決。
    RevisionResolve {
        /// 対象のリビジョン。
        revision: String,
    },
    /// タグの列挙。
    TagList,
    /// タグ 1 件の読み取り。
    TagRead,
    /// タグ名の解釈。
    TagNameDecode,
    /// タグが指すオブジェクトの解決。
    TagResolve {
        /// 対象のタグ名。
        tag: String,
    },
    /// タグの対象オブジェクトの取得。
    TagTargetFetch {
        /// 対象のタグ名。
        tag: String,
    },
    /// タグオブジェクトとしての解釈。
    TagParse {
        /// 対象のタグ名。
        tag: String,
    },
    /// タグオブジェクトの復号。
    TagDecode {
        /// 対象のタグ名。
        tag: String,
    },
    /// タグメッセージの解釈。
    TagMessageDecode,
    /// HEAD の reflog の読み取り。
    HeadReflogRead,
    /// HEAD の reflog の解釈。
    HeadReflogParse,
    /// reflog のメッセージの解釈。
    ReflogMessageDecode,
    /// `git stash list` の出力の解釈。
    StashListOutputParse,
    /// stash の参照（`stash@{n}`）の解釈。
    StashSelectorDecode,
    /// stash のメッセージの解釈。
    StashMessageDecode,
    /// `git branch --merged` の出力の解釈。
    MergedBranchOutputParse,
    /// `git for-each-ref` の出力の解釈。
    ForEachRefOutputParse,
    /// `git worktree list` の出力の解釈。
    WorktreeListOutputParse,
}

/// `git` の出力が想定した形式と異なる場合の、具体的な食い違いの内容。
///
/// [`ReadOperation`] が「何をしていたか」を表すのに対し、こちらは「何が想定と違ったか」を
/// 表す。読み取れなかった値そのもの（レコード・参照名など）を**整形前の値として**保持し、
/// 文へ組み立てるのは [`crate::i18n::messages::ErrorMessages::describe`] に委ねる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MalformedOutput {
    /// `git rev-list --left-right --count` の出力が ahead/behind の 2 フィールドでない。
    AheadBehind {
        /// 受け取った出力（UTF-8 でないバイトは置換した表現）。
        output: String,
    },
    /// `git rev-list --count` の出力がコミット数として解釈できない。
    CommitCount {
        /// 受け取った出力（UTF-8 でないバイトは置換した表現）。
        output: String,
    },
    /// `git status --porcelain -z` のエントリが `XY <path>` の形式でない。
    StatusEntry {
        /// 受け取ったレコード（UTF-8 でないバイトは置換した表現）。
        record: String,
    },
    /// リネーム・コピーのエントリに続くはずの変更元のレコードが無い。
    StatusRenameOriginMissing {
        /// 変更元を欠いていた変更後のパス。
        path: String,
    },
    /// `git stash list -z` の出力が参照とメッセージの組になっていない。
    StashRecordPairing {
        /// 受け取ったレコード数。
        records: usize,
    },
    /// stash の参照が `stash@{n}` の形式でない。
    StashSelectorFormat {
        /// 受け取った参照。
        selector: String,
    },
    /// `git for-each-ref` の出力が参照名と日時の組になっていない。
    BranchActivityPair {
        /// 受け取った行。
        line: String,
    },
    /// `git worktree list --porcelain` の属性が値を欠いている。
    WorktreeAttributeValueMissing {
        /// 値を欠いていた属性のラベル。
        label: String,
    },
    /// worktree の `branch` 属性が [`BRANCH_REF_PREFIX`] 配下の参照名でない。
    WorktreeBranchReference {
        /// 受け取った参照名。
        reference: String,
    },
    /// worktree のレコードが [`WORKTREE_LABEL`] 属性で始まっていない。
    WorktreeRecordStart {
        /// 受け取った行。
        line: String,
    },
    /// worktree のレコードが空行で終端されていない。
    WorktreeRecordUnterminated {
        /// 終端を欠いていたレコードの worktree のパス。
        path: String,
    },
}

/// `gix` 側のエラーを [`Error::RepositoryReadFailed`] へ変換する。
fn read_error(
    operation: ReadOperation,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> Error {
    Error::RepositoryReadFailed {
        operation,
        source: source.into(),
    }
}

/// `git` の出力の食い違いを [`Error::GitOutputMalformed`] へ変換する。
///
/// `gix` のエラーと違い原因となる `source` が存在しない（食い違いを見つけたのは fuzgit 自身で
/// ある）ため、[`read_error`] とは別のバリアントで表す。
fn malformed_output(operation: ReadOperation, detail: MalformedOutput) -> Error {
    Error::GitOutputMalformed { operation, detail }
}

/// git の参照名・署名は任意のバイト列を取り得るため、UTF-8 でない場合は明示的にエラーとする。
fn to_utf8(bytes: &BStr, operation: ReadOperation) -> Result<String> {
    bytes
        .to_str()
        .map(str::to_owned)
        .map_err(|source| read_error(operation, source))
}

/// HEAD がまだ 1 件もコミットを持たないブランチ（unborn HEAD）を指している場合、その短縮名を返す。
fn unborn_branch(repository: &gix::Repository) -> Result<Option<String>> {
    let head = repository
        .head()
        .map_err(|source| read_error(ReadOperation::HeadRead, source))?;

    match &head.kind {
        gix::head::Kind::Unborn(name) => {
            to_utf8(name.shorten(), ReadOperation::HeadBranchNameDecode).map(Some)
        }
        gix::head::Kind::Symbolic(_) | gix::head::Kind::Detached { .. } => Ok(None),
    }
}

/// 候補が 0 件になった原因が unborn HEAD であれば [`Error::UnbornHead`] を返す。
///
/// `git init` 直後のリポジトリでは「候補がありません」だけでは原因が分からないため、
/// 単なる候補ゼロと区別して原因と次に取れる操作を伝える。
fn reject_unborn_head(repository: &gix::Repository) -> Result<()> {
    match unborn_branch(repository)? {
        Some(branch) => Err(Error::UnbornHead { branch }),
        None => Ok(()),
    }
}

/// HEAD が指しているローカルブランチの短縮名を返す。detached HEAD の場合は `None`。
///
/// まだコミットの無いブランチ（unborn HEAD）でも、HEAD が指す名前を返す。
///
/// # Errors
///
/// HEAD の読み取りに失敗した場合、またはブランチ名が UTF-8 でない場合は
/// [`Error::RepositoryReadFailed`] を返す。
pub fn current_branch(repository: &gix::Repository) -> Result<Option<String>> {
    let Some(name) = repository
        .head_name()
        .map_err(|source| read_error(ReadOperation::HeadRead, source))?
    else {
        return Ok(None);
    };

    to_utf8(name.shorten(), ReadOperation::HeadBranchNameDecode).map(Some)
}

/// ブランチ一覧を名前順で取得する。
///
/// [`BranchScope::All`] の場合はローカルブランチの後ろにリモート追跡ブランチを並べる。
///
/// # Errors
///
/// - 参照の列挙・読み取りに失敗した場合、または参照名が UTF-8 でない場合は
///   [`Error::RepositoryReadFailed`]
/// - ブランチが 1 件も無く、その原因が unborn HEAD である場合は [`Error::UnbornHead`]
pub fn branches(repository: &gix::Repository, scope: BranchScope) -> Result<Vec<BranchInfo>> {
    let current = current_branch(repository)?;
    let platform = repository
        .references()
        .map_err(|source| read_error(ReadOperation::ReferenceList, source))?;

    let mut locals = Vec::new();
    for reference in platform
        .local_branches()
        .map_err(|source| read_error(ReadOperation::LocalBranchList, source))?
    {
        let reference =
            reference.map_err(|source| read_error(ReadOperation::LocalBranchRead, source))?;
        let name = to_utf8(
            reference.name().shorten(),
            ReadOperation::LocalBranchNameDecode,
        )?;
        locals.push(BranchInfo {
            is_current: current.as_deref() == Some(name.as_str()),
            name,
            is_remote: false,
        });
    }
    locals.sort_by(|left, right| left.name.cmp(&right.name));

    if scope == BranchScope::Local {
        if locals.is_empty() {
            reject_unborn_head(repository)?;
        }
        return Ok(locals);
    }

    let mut remotes = Vec::new();
    for reference in platform
        .remote_branches()
        .map_err(|source| read_error(ReadOperation::RemoteBranchList, source))?
    {
        let reference =
            reference.map_err(|source| read_error(ReadOperation::RemoteBranchRead, source))?;

        // `origin/HEAD` はリモートの既定ブランチへのシンボリック参照であり、
        // 切り替え先としては実体のブランチと重複するため候補から外す
        if matches!(reference.target(), gix::refs::TargetRef::Symbolic(_)) {
            continue;
        }

        let name = to_utf8(
            reference.name().shorten(),
            ReadOperation::RemoteBranchNameDecode,
        )?;
        remotes.push(BranchInfo {
            name,
            is_current: false,
            is_remote: true,
        });
    }
    remotes.sort_by(|left, right| left.name.cmp(&right.name));

    locals.extend(remotes);
    if locals.is_empty() {
        reject_unborn_head(repository)?;
    }
    Ok(locals)
}

/// 現在のブランチを除いたブランチ一覧（ローカル・リモート追跡）を取得する。
///
/// `gz merge` の merge 対象、`gz rebase` の base の候補に用いる。自分自身への merge /
/// rebase は「取り込むコミットが 1 件も無い」操作であり、選んでも意味を持たないため候補から外す。
/// detached HEAD では現在のブランチが無いため、すべてのブランチが候補になる。
///
/// # Errors
///
/// [`branches`] と同じ（unborn HEAD の場合は [`Error::UnbornHead`]）。
pub fn other_branches(repository: &gix::Repository) -> Result<Vec<BranchInfo>> {
    Ok(branches(repository, BranchScope::All)?
        .into_iter()
        .filter(|branch| !branch.is_current)
        .collect())
}

/// リモートの名前一覧を名前順で取得する。
///
/// `gz push` の候補（リモート × 現在ブランチ）を組み立てるために用いる。
///
/// # Errors
///
/// リモート名が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn remotes(repository: &gix::Repository) -> Result<Vec<String>> {
    // `remote_names()` は BTreeSet であり既に名前順に並んでいる
    repository
        .remote_names()
        .iter()
        .map(|name| to_utf8(name.as_bstr(), ReadOperation::RemoteNameDecode))
        .collect()
}

/// 参照名だけを短縮名で列挙させる `git for-each-ref` の書式。
///
/// 既定の出力はオブジェクト ID と種別も並べるため、必要な参照名だけを出させる。
/// 参照名は制御文字を含めない規則（`git check-ref-format`）であり、改行で区切っても曖昧にならない。
const SHORT_REF_FORMAT: &str = "--format=%(refname:short)";

/// リモートの URL を表示する `git remote get-url <remote>` の実行引数を組み立てる。
///
/// `.git/config` に記録された URL を読むだけで、ネットワークへは接続しない。
/// `gz fetch` のプレビューは選択項目ごとに都度実行されるため、往復遅延・認証プロンプトで
/// 描画がブロックされないよう、プレビューへ出すのはこのようなローカル情報に限る
/// （design.md「候補生成・プレビューでネットワークアクセスを行わない」）。
///
/// `remote` には [`remotes`] が列挙した名前だけを渡すこと（`git remote get-url` は
/// リモート名を `--` で保護できないため、値の由来で担保する）。
#[must_use]
pub fn remote_url_args(remote: &str) -> Vec<String> {
    ["remote", "get-url", remote]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// 既知のリモート追跡ブランチを列挙する `git for-each-ref` の実行引数を組み立てる。
///
/// [`remote_url_args`] と同じく、ローカルに保存済みの参照を読むだけでネットワークへは接続しない
/// （前回の fetch までに取得できている参照が並ぶ）。
#[must_use]
pub fn remote_tracking_refs_args(remote: &str) -> Vec<String> {
    vec![
        "for-each-ref".to_owned(),
        format!("refs/remotes/{remote}"),
        SHORT_REF_FORMAT.to_owned(),
    ]
}

/// ローカルブランチの追跡状況を 1 行ずつ並べる `git for-each-ref` の書式。
///
/// `%(HEAD)` は HEAD が指すブランチにだけ `*` を、それ以外には空白を出す（`git branch` と同じ体裁）。
/// upstream が設定されていないブランチと、upstream に対して差が無いブランチでは
/// `%(if)` で該当部分ごと省き、行末に空の欄が残らないようにする。
/// ahead/behind の文言（`[ahead 2, behind 1]` / `[gone]`）は git の表記をそのまま用いる。
const BRANCH_TRACKING_FORMAT: &str = "--format=%(HEAD) %(refname:short)\
%(if)%(upstream)%(then) → %(upstream:short)%(end)\
%(if)%(upstream:track)%(then) %(upstream:track)%(end)";

/// ローカルブランチとその追跡状況を列挙する `git for-each-ref` の実行引数を組み立てる。
///
/// 読むのは `refs/heads` と `refs/remotes` の参照および `.git/config` の upstream 設定だけで、
/// **作業ツリーを走査しない**。`git status --branch` でも同じ追跡状況を得られるが、
/// あちらは追跡状況の前に index の refresh（全ファイルの stat、必要なら再ハッシュ）を伴い、
/// 規模の大きいリポジトリでは秒単位を要する（tasks.md の実測メモ）。プレビューは
/// カーソル移動のたびに同期実行されるため、走査を伴わないこちらを用いる。
///
/// [`remote_url_args`] と同じくネットワークへは接続しない
/// （追跡状況は前回の fetch までに取得済みの参照との比較で求まる）。
#[must_use]
pub fn branch_tracking_args() -> Vec<String> {
    vec![
        "for-each-ref".to_owned(),
        "refs/heads".to_owned(),
        BRANCH_TRACKING_FORMAT.to_owned(),
    ]
}

/// ブランチに設定された upstream（`branch.<name>.remote` / `.merge`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// `branch.<name>.remote` の値。
    ///
    /// 通常は `origin` のようなリモート名だが、git の設定では URL を直接書くこともできるため
    /// その場合は URL がそのまま入る（[`remotes`] の一覧とは一致しない）。
    pub remote: String,
    /// `branch.<name>.merge` の値（リモート側の完全な参照名。例: `refs/heads/main`）。
    pub merge_ref: String,
}

impl Upstream {
    /// ローカルのリモート追跡参照（`refs/remotes/<remote>/<branch>`）を組み立てる。
    ///
    /// upstream との比較（ahead/behind の算出、`gz diff --upstream`）はネットワークを
    /// 使わずローカルの追跡参照に対して行うため、リモート側の参照名
    /// （`refs/heads/<branch>`）ではなく追跡参照へ読み替える。
    /// `branch.<name>.remote` に URL を直接設定している場合など、追跡参照を
    /// 組み立てられない設定では推測せずに `None` を返す。
    #[must_use]
    pub fn tracking_ref(&self) -> Option<String> {
        let branch = self.merge_ref.strip_prefix("refs/heads/")?;

        Some(format!(
            "refs/remotes/{remote}/{branch}",
            remote = self.remote
        ))
    }
}

/// ブランチに設定された upstream を取得する。設定が無い場合は `None`。
///
/// # Errors
///
/// ブランチ名が参照名として不正な場合、upstream の参照名を解決できない場合、
/// または値が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn upstream(repository: &gix::Repository, branch: &str) -> Result<Option<Upstream>> {
    let full_name = gix::refs::Category::LocalBranch
        .to_full_name(branch)
        .map_err(|source| {
            read_error(
                ReadOperation::BranchNameParse {
                    branch: branch.to_owned(),
                },
                source,
            )
        })?;

    // fetch 方向を見る。push 方向は pushRemote / push.default の解決を伴い、
    // 「現在の追跡先」を知りたいここでの用途とは意味が異なる
    let direction = gix::remote::Direction::Fetch;

    let Some(remote) = repository.branch_remote_name(branch, direction) else {
        return Ok(None);
    };
    let remote = match remote {
        gix::remote::Name::Symbol(name) => name.into_owned(),
        gix::remote::Name::Url(url) => to_utf8(url.as_ref(), ReadOperation::RemoteUrlDecode)?,
    };

    let Some(merge_ref) = repository.branch_remote_ref_name(full_name.as_ref(), direction) else {
        return Ok(None);
    };
    let merge_ref = merge_ref.map_err(|source| {
        read_error(
            ReadOperation::UpstreamResolve {
                branch: branch.to_owned(),
            },
            source,
        )
    })?;
    let merge_ref = to_utf8(merge_ref.as_bstr(), ReadOperation::UpstreamRefNameDecode)?;

    Ok(Some(Upstream { remote, merge_ref }))
}

/// `git rev-list --left-right --count` の出力をパースする。
///
/// 出力は `<左だけに含まれる件数>\t<右だけに含まれる件数>\n` の 1 行
/// （末尾の改行は無い場合もある）。`<local>...<remote>` の順で渡すため、
/// 左が ahead、右が behind に対応する。
///
/// # Errors
///
/// フィールド数が 2 でない場合、数値として解釈できない場合、
/// UTF-8 でない場合は [`Error::GitOutputMalformed`] を返す。
fn parse_ahead_behind(output: &[u8]) -> Result<(usize, usize)> {
    let malformed = || {
        malformed_output(
            ReadOperation::RevListOutputParse,
            MalformedOutput::AheadBehind {
                output: output.as_bstr().to_str_lossy().into_owned(),
            },
        )
    };

    let text = output
        .to_str()
        .map_err(|_| malformed())?
        .trim_end_matches(['\n', '\r']);

    let mut fields = text.split('\t');
    let (Some(ahead), Some(behind), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(malformed());
    };

    let ahead = ahead.parse::<usize>().map_err(|_| malformed())?;
    let behind = behind.parse::<usize>().map_err(|_| malformed())?;
    Ok((ahead, behind))
}

/// `revision` が解決できるかどうかを判定する。
///
/// `git rev-parse --verify --quiet` は解決できない場合にメッセージを出さず非ゼロ終了するため、
/// 終了コードだけで「存在するか」を判定できる。
fn revision_exists(workdir: &Path, revision: &str) -> Result<bool> {
    // `^{commit}` を付けて、同名のファイルやタグではなくコミットとして解決できることを確かめる
    let specification = format!("{revision}^{{commit}}");
    let (code, _) = capture_git_with_status_in(
        workdir,
        &["rev-parse", "--verify", "--quiet", &specification],
    )?;

    Ok(code == 0)
}

/// `local` が `remote_ref` に対して何コミット進んでいる / 遅れているかを返す。
///
/// `remote_ref` が存在しない場合（まだ push していないブランチなど）は `None` を返す。
///
/// # Errors
///
/// `git` の実行に失敗した場合、または出力をパースできない場合にエラーを返す。
pub fn ahead_behind(
    workdir: &Path,
    local: &str,
    remote_ref: &str,
) -> Result<Option<(usize, usize)>> {
    if !revision_exists(workdir, remote_ref)? {
        return Ok(None);
    }

    // `<左>...<右>` の対称差を左右別に数える。左＝ローカルにしかないコミット数＝ahead
    let range = format!("{local}...{remote_ref}");
    // 末尾の `--` はリビジョンとパスの境界。同名のファイルがあってもパスとして解釈させない
    let output = capture_git_in(
        workdir,
        &["rev-list", "--left-right", "--count", &range, "--"],
    )?;

    parse_ahead_behind(&output).map(Some)
}

/// `git rev-list --count` の出力をパースする。
///
/// 出力は件数だけの 1 行（末尾の改行は無い場合もある）。
///
/// # Errors
///
/// 数値として解釈できない場合、UTF-8 でない場合は [`Error::GitOutputMalformed`] を返す。
fn parse_commit_count(output: &[u8]) -> Result<usize> {
    let malformed = || {
        malformed_output(
            ReadOperation::RevListOutputParse,
            MalformedOutput::CommitCount {
                output: output.as_bstr().to_str_lossy().into_owned(),
            },
        )
    };

    output
        .to_str()
        .map_err(|_| malformed())?
        .trim_end_matches(['\n', '\r'])
        .parse::<usize>()
        .map_err(|_| malformed())
}

/// `range`（`<from>..<to>` 形式）に含まれるコミット数を数える。
///
/// `gz merge` で取り込まれるコミット数、`gz rebase` で replay されるコミット数を
/// 確認プロンプトに提示するために用いる。
///
/// # Errors
///
/// `git` の実行に失敗した場合、または出力をパースできない場合にエラーを返す。
pub fn commit_count(workdir: &Path, range: &str) -> Result<usize> {
    // 末尾の `--` はリビジョンとパスの境界。同名のファイルがあってもパスとして解釈させない
    let output = capture_git_in(workdir, &["rev-list", "--count", range, "--"])?;

    parse_commit_count(&output)
}

/// push 先の候補 1 件分の情報（リモート × 現在のブランチ）。
///
/// `remote` / `branch` はいずれも gix が列挙した値であり、ユーザーの自由入力ではない。
/// `git push` はパス以外の位置引数を取り `--` で保護できないため、この由来を保つことが
/// そのままインジェクション対策になる（design.md セキュリティ設計）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    /// リモート名（`origin` 等）。
    pub remote: String,
    /// push するブランチ名（現在のブランチ）。
    pub branch: String,
    /// 現在のブランチの upstream に対応する候補かどうか。
    pub is_upstream: bool,
    /// リモート追跡参照に対する (ahead, behind)。追跡参照が無い場合は `None`。
    pub ahead_behind: Option<(usize, usize)>,
}

impl PushTarget {
    /// リモート追跡参照の短縮名（`origin/main`）。表示と選択結果の照合に用いる。
    #[must_use]
    pub fn tracking_name(&self) -> String {
        format!(
            "{remote}/{branch}",
            remote = self.remote,
            branch = self.branch
        )
    }

    /// リモート追跡参照の完全な参照名（`refs/remotes/origin/main`）。
    ///
    /// git へ渡す際は短縮名ではなくこちらを使う。同名のローカルブランチ・タグがあっても
    /// リモート追跡参照として解決されることを保証するため。
    #[must_use]
    pub fn tracking_ref(&self) -> String {
        format!("refs/remotes/{name}", name = self.tracking_name())
    }
}

/// `upstream` が「リモート `remote` の `refs/heads/<branch>`」を指しているかどうか。
///
/// `gz push` の候補は `git push <remote> <branch>`（= リモート側の `refs/heads/<branch>` を更新）
/// であるため、リモート名だけでなく更新先の参照名まで一致して初めて upstream と同じ宛先といえる。
fn targets_upstream(upstream: Option<&Upstream>, remote: &str, branch: &str) -> bool {
    upstream.is_some_and(|upstream| {
        upstream.remote == remote && upstream.merge_ref == format!("refs/heads/{branch}")
    })
}

/// push 先の候補（リモート × 現在のブランチ）をリモート名順で取得する。
///
/// # Errors
///
/// - HEAD がブランチを指していない場合は [`Error::DetachedHead`]
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - リモート・upstream の読み取り、ahead/behind の算出に失敗した場合はそれぞれのエラー
pub fn push_targets(repository: &gix::Repository) -> Result<Vec<PushTarget>> {
    let Some(branch) = current_branch(repository)? else {
        return Err(Error::DetachedHead);
    };

    let upstream = upstream(repository, &branch)?;
    let workdir = workdir(repository)?;
    // まだコミットが 1 件も無いブランチはローカル側を解決できず、比較そのものが成立しない
    let branch_exists = revision_exists(workdir, &branch)?;

    let mut targets = Vec::new();
    for remote in remotes(repository)? {
        let mut target = PushTarget {
            remote,
            branch: branch.clone(),
            is_upstream: false,
            ahead_behind: None,
        };
        target.is_upstream = targets_upstream(upstream.as_ref(), &target.remote, &branch);
        if branch_exists {
            target.ahead_behind = ahead_behind(workdir, &branch, &target.tracking_ref())?;
        }
        targets.push(target);
    }

    Ok(targets)
}

/// upstream へ追随させられるローカルブランチ 1 件分の情報（`gz pull`）。
///
/// `remote` は [`remotes`] が列挙した名前と一致することを確認済みの値、`tracking_ref` は
/// [`Upstream::tracking_ref`] が組み立てたローカルの追跡参照であり、いずれもユーザーの
/// 自由入力ではない。`git fetch` / `git merge` の位置引数は `--` で保護できないため、
/// この由来を保つことがそのままインジェクション対策になる（design.md セキュリティ設計）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullTarget {
    /// ローカルブランチの短縮名。
    pub branch: String,
    /// upstream のリモート名（`origin` 等）。
    pub remote: String,
    /// upstream のローカル追跡参照（`refs/remotes/<remote>/<branch>`）。
    pub tracking_ref: String,
    /// HEAD が指しているブランチかどうか。
    ///
    /// チェックアウト中のブランチへの ref 更新は git が拒否するため、取り込みの経路が
    /// 他のブランチ（`git fetch .`）と異なる（`git merge --ff-only`）。
    pub is_current: bool,
}

/// [`pull_targets`] の走査結果。
///
/// 除外件数を finder のヘッダーへ出せるよう、候補と併せて返す（[`crate::git::siblings::SiblingScan`]
/// と同型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullScan {
    /// 取り込み対象にできるブランチ。現在のブランチが先頭、以降はブランチ名の昇順。
    pub targets: Vec<PullTarget>,
    /// 取り込み先を決められない・更新できないため候補から除外したブランチの件数。
    ///
    /// 黙って消さずに件数を示せるよう、候補と併せて返す。
    pub excluded: usize,
}

/// 1 件のローカルブランチから取り込み対象を組み立てる。候補にできない場合は `None`。
///
/// `remotes` / `checked_out` は呼び出し側が一度だけ集めたものを受け取る
/// （ブランチごとに git を起動しないため）。
fn pull_target(
    repository: &gix::Repository,
    branch: &BranchInfo,
    remotes: &HashSet<String>,
    checked_out: &HashSet<String>,
) -> Result<Option<PullTarget>> {
    // 他の worktree でチェックアウト中のブランチは、git が ref の更新を拒否する
    // （`refusing to fetch into branch '%s' checked out at '%s'`）。
    // 現在のブランチだけは作業ツリーごと更新する経路（`git merge --ff-only`）を持つため残す
    if !branch.is_current && checked_out.contains(&branch.name) {
        return Ok(None);
    }

    let Some(upstream) = upstream(repository, &branch.name)? else {
        return Ok(None);
    };

    // `branch.<name>.merge` がリモート側のブランチを指していない設定では追跡参照を
    // 組み立てられない。取り込み元を推測せずに候補から外す
    let Some(tracking_ref) = upstream.tracking_ref() else {
        return Ok(None);
    };

    // `branch.<name>.remote` には URL を直接書くこともできる。その値をそのまま git へ渡すと
    // fuzgit が列挙していない対象へ通信することになるため、登録済みのリモート名である
    // ことを確かめる（`gz sync` の resolve_target と同じ検査。design.md セキュリティ設計）
    if !remotes.contains(&upstream.remote) {
        return Ok(None);
    }

    Ok(Some(PullTarget {
        branch: branch.name.clone(),
        remote: upstream.remote,
        tracking_ref,
        is_current: branch.is_current,
    }))
}

/// upstream へ追随させられるローカルブランチを列挙する（現在のブランチが先頭、以降は名前順）。
///
/// 候補は upstream が設定され、そこからリモート追跡参照を組み立てられ、かつその
/// リモートが登録済みであるローカルブランチに限る。それ以外と、他の worktree で
/// チェックアウト中のブランチは候補から除外し、件数を [`PullScan::excluded`] として返す。
///
/// detached HEAD はエラーにしない（[`push_targets`] が [`Error::DetachedHead`] を返すのとの
/// 違い）。`gz pull` は複数のローカルブランチを対象にする一括処理であり、現在のブランチが
/// 無くても他のブランチは通常どおり取り込めるため。
///
/// 起動する `git` プロセスは [`worktrees`] の `git worktree list` 1 回だけで、upstream と
/// リモートの解決は `gix` のプロセス内読み取りで行う。ahead/behind は求めない
/// （全ブランチ分の `rev-list` は 1 ブランチあたり 2 プロセスを要し、候補一覧の
/// 応答性（非機能要件 200ms）を圧迫するため。値は取り込み後に git 自身が示す）。
///
/// # Errors
///
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - ローカルブランチが 1 件も無く、その原因が unborn HEAD である場合は [`Error::UnbornHead`]
/// - worktree 一覧の取得・パースに失敗した場合、ブランチ・リモート・upstream の読み取りに
///   失敗した場合はそれぞれのエラー
pub fn pull_targets(repository: &gix::Repository) -> Result<PullScan> {
    let checked_out = checked_out_branches(&worktrees(workdir(repository)?)?);
    let remotes: HashSet<String> = remotes(repository)?.into_iter().collect();

    let mut targets = Vec::new();
    let mut excluded = 0;
    for branch in branches(repository, BranchScope::Local)? {
        match pull_target(repository, &branch, &remotes, &checked_out)? {
            Some(target) => targets.push(target),
            None => excluded += 1,
        }
    }

    // `branches` は名前順で返すため、現在のブランチを先頭へ引き上げるだけで
    // 「現在のブランチが先頭、以降は名前順」が確定する（`siblings::discover` と同じ規則）
    targets.sort_by_key(|target| std::cmp::Reverse(target.is_current));

    Ok(PullScan { targets, excluded })
}

/// 進行中の git 操作のうち、fuzgit が復帰メニューを提供するもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// merge が進行中（`.git/MERGE_HEAD` がある）。
    Merge,
    /// rebase が進行中（`.git/rebase-merge` または `.git/rebase-apply` がある）。
    Rebase,
}

/// merge / rebase が進行中かどうかを返す。
///
/// cherry-pick / revert / bisect / `git am` も「進行中の操作」ではあるが、
/// fuzgit が continue / abort の復帰メニューを提供するのは merge と rebase だけであるため
/// `None`（進行中ではない扱い）を返す。
#[must_use]
pub fn operation_in_progress(repository: &gix::Repository) -> Option<Operation> {
    use gix::state::InProgress;

    match repository.state()? {
        InProgress::Merge => Some(Operation::Merge),
        // 非対話の `git rebase` でも merge バックエンドは `rebase-merge/interactive` を作るため、
        // RebaseInteractive も通常の rebase として扱う。ApplyMailboxRebase は
        // `rebase-apply` があり `applying` が無い状態で、git 自身も rebase 進行中と判定する
        InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase => {
            Some(Operation::Rebase)
        }
        InProgress::ApplyMailbox
        | InProgress::Bisect
        | InProgress::CherryPick
        | InProgress::CherryPickSequence
        | InProgress::Revert
        | InProgress::RevertSequence => None,
    }
}

/// 走査の起点となるコミット（tip）を [`CommitScope`] に応じて集める。
///
/// 起点が存在しない場合（unborn HEAD、ブランチが 1 件も無いリポジトリ）は空の `Vec` を返す。
fn commit_tips(repository: &gix::Repository, scope: CommitScope<'_>) -> Result<Vec<gix::ObjectId>> {
    match scope {
        CommitScope::Head => {
            let mut head = repository
                .head()
                .map_err(|source| read_error(ReadOperation::HeadRead, source))?;
            let head_id = head
                .try_peel_to_id()
                .map_err(|source| read_error(ReadOperation::HeadResolve, source))?;
            Ok(head_id.map(|id| id.detach()).into_iter().collect())
        }
        CommitScope::AllBranches => {
            let platform = repository
                .references()
                .map_err(|source| read_error(ReadOperation::ReferenceList, source))?;
            let locals = platform
                .local_branches()
                .map_err(|source| read_error(ReadOperation::LocalBranchList, source))?;
            let remotes = platform
                .remote_branches()
                .map_err(|source| read_error(ReadOperation::RemoteBranchList, source))?;

            let mut tips = Vec::new();
            for reference in locals.chain(remotes) {
                let reference =
                    reference.map_err(|source| read_error(ReadOperation::BranchRead, source))?;
                let id = reference
                    .into_fully_peeled_id()
                    .map_err(|source| read_error(ReadOperation::BranchTipResolve, source))?;
                tips.push(id.detach());
            }
            // 複数ブランチが同じコミットを指していても rev_walk 側で重複は除かれる
            Ok(tips)
        }
        CommitScope::Branch(name) => {
            let id = repository.rev_parse_single(name).map_err(|source| {
                read_error(
                    ReadOperation::BranchResolve {
                        branch: name.to_owned(),
                    },
                    source,
                )
            })?;
            Ok(vec![id.detach()])
        }
    }
}

/// 走査の並び順を [`CommitScope`] に応じて決める。
fn sorting(scope: CommitScope<'_>) -> gix::revision::walk::Sorting {
    match scope {
        // 起点が 1 つの場合、グラフ順はそのまま HEAD からの祖先順になる（P1 の `log` と同じ順序）
        CommitScope::Head | CommitScope::Branch(_) => gix::revision::walk::Sorting::BreadthFirst,
        // 複数の起点をグラフ順で辿るとブランチごとにコミットが偏り、limit で打ち切った際に
        // 一部のブランチの新しいコミットが候補から漏れるため、コミット日時の新しい順に揃える
        CommitScope::AllBranches => gix::revision::walk::Sorting::ByCommitTime(
            gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
        ),
    }
}

/// `scope` の範囲のコミットを新しい順に最大 `limit` 件取得する。
///
/// # Errors
///
/// - 起点の解決・履歴の走査・コミットの復号に失敗した場合は [`Error::RepositoryReadFailed`]
/// - コミットが 1 件も無く、その原因が unborn HEAD である場合は [`Error::UnbornHead`]
pub fn commits(
    repository: &gix::Repository,
    scope: CommitScope<'_>,
    limit: usize,
) -> Result<Vec<CommitInfo>> {
    let tips = commit_tips(repository, scope)?;

    let walk = repository
        .rev_walk(tips)
        .sorting(sorting(scope))
        .all()
        .map_err(|source| read_error(ReadOperation::CommitHistoryWalk, source))?;

    // 大規模リポジトリでも初期表示を保つため、rev_walk は limit 件で打ち切る
    let mut commits = Vec::new();
    for info in walk.take(limit) {
        let info = info.map_err(|source| read_error(ReadOperation::CommitHistoryWalk, source))?;
        let commit = info
            .object()
            .map_err(|source| read_error(ReadOperation::CommitObjectFetch, source))?;
        commits.push(commit_info(&commit)?);
    }

    if commits.is_empty() {
        reject_unborn_head(repository)?;
    }

    Ok(commits)
}

/// `gix` のコミットオブジェクトを [`CommitInfo`] へ変換する。
fn commit_info(commit: &gix::Commit<'_>) -> Result<CommitInfo> {
    let short_id = commit
        .short_id()
        .map_err(|source| read_error(ReadOperation::ShortIdCompute, source))?;
    let message = commit
        .message()
        .map_err(|source| read_error(ReadOperation::CommitMessageDecode, source))?;
    let author = commit
        .author()
        .map_err(|source| read_error(ReadOperation::CommitAuthorDecode, source))?
        .trim();
    let time = author
        .time()
        .map_err(|source| read_error(ReadOperation::CommitTimeDecode, source))?
        .format(gix::date::time::format::SHORT)
        .map_err(|source| read_error(ReadOperation::CommitTimeFormat, source))?;

    Ok(CommitInfo {
        id: commit.id().to_string(),
        short_id: short_id.to_string(),
        summary: to_utf8(&message.summary(), ReadOperation::CommitSummaryDecode)?,
        author: to_utf8(author.name, ReadOperation::CommitAuthorNameDecode)?,
        time,
    })
}

/// `git status` の実行引数。
///
/// - `-z`: パスを NUL 区切り・エスケープなしで出力させる。既定の出力は空白や非 ASCII を含むパスを
///   ダブルクォートとバックスラッシュでエスケープする（`core.quotepath`）ため、解析側でその復元が
///   必要になるうえ、パス中の空白と区切りの空白を区別できない。NUL 区切りならどちらの問題も起きない
/// - `--untracked-files=all`: 未追跡ディレクトリを 1 エントリにまとめず、ファイル単位で列挙させる
///   （`gz add` でファイル単位に選択・プレビューできるようにするため）
const STATUS_ARGS: [&str; 4] = ["status", "--porcelain", "-z", "--untracked-files=all"];

/// `git status --porcelain` の 1 エントリのうち、状態コードと区切り空白が占める幅（`XY `）。
const STATUS_FIELD_WIDTH: usize = 3;

/// 未追跡を表す状態コード（`??`）。
const UNTRACKED_CODE: char = '?';

/// 変更なしを表す状態コード。
const UNMODIFIED_CODE: char = ' ';

/// マージ未解決を表す状態コード（`UU` / `AU` / `UD` など）。
const UNMERGED_CODE: char = 'U';

/// 追加を表す状態コード。両側が `A` の場合（`AA`）はマージ未解決を意味する。
const ADDED_CODE: char = 'A';

/// 削除を表す状態コード。両側が `D` の場合（`DD`）はマージ未解決を意味する。
const DELETED_CODE: char = 'D';

/// 候補に含める変更ファイルの範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeScope {
    /// ステージ済みの変更（`gz restore --staged` の対象）。
    Staged,
    /// 作業ツリーの変更（`gz restore` の対象）。
    Worktree,
    /// 未ステージの変更と未追跡ファイル（`gz add` の対象）。
    Stageable,
    /// 追跡済みファイルの変更（`gz stash push` の対象）。
    ///
    /// `git stash push -- <paths>` はステージ済みの変更も含めて退避するため、
    /// [`ChangeScope::Worktree`] と違い index 側だけに変更があるファイルも対象に含める。
    Tracked,
    /// 追跡済みファイルの変更と未追跡ファイル（`gz stash push --include-untracked` の対象）。
    ///
    /// 未追跡ファイルは `git stash push` に `--include-untracked` を付けたときだけ退避できるため、
    /// [`ChangeScope::Tracked`] と分けて指定する。
    TrackedOrUntracked,
    /// マージ未解決（コンフリクト中）のファイル。
    ///
    /// merge / rebase の復帰メニューで、解決したファイルを stage する対象に用いる。
    Unmerged,
}

impl ChangeScope {
    /// 変更がこの範囲に含まれるかどうかを判定する。
    fn includes(self, change: &FileChange) -> bool {
        match self {
            ChangeScope::Staged => change.has_staged_change(),
            ChangeScope::Worktree => change.has_worktree_change(),
            ChangeScope::Stageable => change.is_untracked() || change.has_worktree_change(),
            ChangeScope::Tracked => change.has_staged_change() || change.has_worktree_change(),
            ChangeScope::TrackedOrUntracked => {
                change.is_untracked() || change.has_staged_change() || change.has_worktree_change()
            }
            ChangeScope::Unmerged => change.is_unmerged(),
        }
    }
}

/// 変更ファイル 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// 作業ツリールートからの相対パス。リネーム・コピーの場合は変更後のパス。
    pub path: String,
    /// リネーム・コピー元のパス（`R` / `C` のエントリのみ）。
    pub original_path: Option<String>,
    /// index 側の状態コード（`git status --porcelain` の 1 文字目。未追跡は `?`）。
    pub index_status: char,
    /// 作業ツリー側の状態コード（2 文字目。未追跡は `?`）。
    pub worktree_status: char,
}

impl FileChange {
    /// 未追跡ファイル（`??`）かどうか。
    #[must_use]
    pub fn is_untracked(&self) -> bool {
        self.index_status == UNTRACKED_CODE
    }

    /// HEAD と index の間に差分がある（ステージ済みの変更を持つ）かどうか。
    #[must_use]
    pub fn has_staged_change(&self) -> bool {
        !self.is_untracked() && self.index_status != UNMODIFIED_CODE
    }

    /// index と作業ツリーの間に差分がある（未ステージの変更を持つ）かどうか。
    #[must_use]
    pub fn has_worktree_change(&self) -> bool {
        !self.is_untracked() && self.worktree_status != UNMODIFIED_CODE
    }

    /// マージが未解決（コンフリクト中）かどうか。
    ///
    /// `git status --porcelain` がマージ未解決として扱う状態コードは
    /// `DD` / `AU` / `UD` / `UA` / `DU` / `AA` / `UU` の 7 種類。
    /// どちらかが `U` の 5 種類に加えて、両側が `A`（双方が追加）と両側が `D`（双方が削除）が含まれる。
    #[must_use]
    pub fn is_unmerged(&self) -> bool {
        let (index, worktree) = (self.index_status, self.worktree_status);

        index == UNMERGED_CODE
            || worktree == UNMERGED_CODE
            || (index == ADDED_CODE && worktree == ADDED_CODE)
            || (index == DELETED_CODE && worktree == DELETED_CODE)
    }

    /// `git status --porcelain` と同じ 2 文字の状態コード（`MM` / ` M` / `??` など）。
    #[must_use]
    pub fn status_code(&self) -> String {
        [self.index_status, self.worktree_status].iter().collect()
    }
}

/// タグ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInfo {
    /// 短縮名（`refs/tags/` を除いた `v1.0` の形式）。
    pub name: String,
    /// タグ参照が直接指すオブジェクトの ID。annotated tag ではタグオブジェクト自身。
    ///
    /// `git show` へ渡すとタグのメッセージと対象コミットの両方が表示され、
    /// `git switch --detach` / `git diff` へ渡した場合は git 側でコミットまで peel される。
    /// タグ名ではなくこの ID を git へ渡すことで、名前がオプションとして解釈される余地を排除する。
    pub id: String,
    /// annotated tag のメッセージ 1 行目。lightweight tag では `None`。
    pub message: Option<String>,
}

/// reflog エントリ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// 新しいものから数えた位置。`HEAD@{n}` の `n` に対応する。
    pub index: usize,
    /// エントリの適用後に HEAD が指していたオブジェクトのフルハッシュ。
    pub id: String,
    /// reflog のメッセージ（`checkout: moving from main to feature` 等）。
    pub message: String,
}

impl ReflogEntry {
    /// エントリを一意に指す表記（`HEAD@{n}`）。
    ///
    /// 同じコミット・同じメッセージのエントリが並び得るため、選択結果の照合に用いる。
    #[must_use]
    pub fn selector(&self) -> String {
        format!("HEAD@{{{index}}}", index = self.index)
    }
}

/// stash エントリ 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// `stash@{n}` の `n`。
    pub index: usize,
    /// stash のメッセージ（`WIP on main: 5d21a8c first` / `On main: 作業中` 等）。
    pub message: String,
}

impl StashEntry {
    /// git へ渡す stash の参照（`stash@{n}`）。
    ///
    /// git の出力をそのまま持ち回らず、パース済みの添字から組み立て直すことで、
    /// 想定外の文字列が git の引数として渡らないことを保証する。
    #[must_use]
    pub fn selector(&self) -> String {
        format!("stash@{{{index}}}", index = self.index)
    }
}

/// リビジョンが指すツリーのファイル一覧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionFiles {
    /// 解決済みのフルハッシュ。
    ///
    /// `git` へ渡す際は指定された文字列ではなくこちらを使う。ユーザー入力が
    /// オプションとして解釈される余地を排除し、存在しないリビジョンを早期に弾くため。
    pub id: String,
    /// 作業ツリールートからの相対パス（git が返す辞書順）。
    pub paths: Vec<String>,
}

/// NUL 区切りの出力をレコード列へ分割する。
///
/// 末尾の区切り文字が生む空レコードは取り除く。git が返すパスは空文字になり得ないため、
/// 空レコードを捨てても情報は失われない。
fn nul_records(output: &[u8]) -> impl Iterator<Item = &[u8]> {
    output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
}

/// `git status --porcelain -z` の出力をパースする。
///
/// 1 エントリは `XY <path>` で、リネーム・コピー（`R` / `C`）の場合のみ変更元のパスが
/// 次のレコードとして続く（`-z` では `<変更後>` `<変更元>` の順になる）。
///
/// # Errors
///
/// エントリの形式が想定と異なる場合、変更元のレコードが欠けている場合は
/// [`Error::GitOutputMalformed`]、パスが UTF-8 でない場合は
/// [`Error::RepositoryReadFailed`] を返す。
fn parse_status(output: &[u8]) -> Result<Vec<FileChange>> {
    let mut records = nul_records(output);
    let mut changes = Vec::new();

    while let Some(record) = records.next() {
        if record.len() <= STATUS_FIELD_WIDTH || record[2] != b' ' {
            return Err(malformed_output(
                ReadOperation::StatusOutputParse,
                MalformedOutput::StatusEntry {
                    record: record.as_bstr().to_str_lossy().into_owned(),
                },
            ));
        }

        let index_status = char::from(record[0]);
        let worktree_status = char::from(record[1]);
        let path = to_utf8(
            record[STATUS_FIELD_WIDTH..].as_bstr(),
            ReadOperation::PathDecode,
        )?;

        let original_path = if is_rename_or_copy(index_status) || is_rename_or_copy(worktree_status)
        {
            let original = records.next().ok_or_else(|| {
                malformed_output(
                    ReadOperation::StatusOutputParse,
                    MalformedOutput::StatusRenameOriginMissing { path: path.clone() },
                )
            })?;
            Some(to_utf8(
                original.as_bstr(),
                ReadOperation::RenameOriginPathDecode,
            )?)
        } else {
            None
        };

        changes.push(FileChange {
            path,
            original_path,
            index_status,
            worktree_status,
        });
    }

    Ok(changes)
}

/// 状態コードがリネーム（`R`）・コピー（`C`）かどうか。
fn is_rename_or_copy(status: char) -> bool {
    status == 'R' || status == 'C'
}

/// 作業ツリーの変更ファイルを `scope` で絞り込んで取得する。
///
/// # Errors
///
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - `git status` の実行に失敗した場合は [`Error::GitCommandFailed`] 等
/// - 出力の形式が想定と異なる場合は [`Error::GitOutputMalformed`]
/// - 出力が UTF-8 でない場合は [`Error::RepositoryReadFailed`]
pub fn changes(repository: &gix::Repository, scope: ChangeScope) -> Result<Vec<FileChange>> {
    let output = capture_git_in(workdir(repository)?, &STATUS_ARGS)?;

    Ok(parse_status(&output)?
        .into_iter()
        .filter(|change| scope.includes(change))
        .collect())
}

/// `git diff --name-only -z` の出力をパースする。
///
/// 1 レコードが 1 ファイルのパスで、各レコードは NUL で終端される
/// （`parse_status` / `parse_worktree_list` と同じ規則）。差分が無い場合は出力自体が空になる。
///
/// # Errors
///
/// パスが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_changed_files(output: &[u8]) -> Result<Vec<String>> {
    nul_records(output)
        .map(|record| to_utf8(record.as_bstr(), ReadOperation::PathDecode))
        .collect()
}

/// 比較範囲に変更のあるファイルのパス一覧を、git が返す順で取得する。
///
/// `range` には `git diff` のサブコマンド名の後ろ・`--` の前に置く引数
/// （`--staged` や比較するリビジョン 2 つ）をそのまま渡す。組み立ては
/// 比較モードを持つ呼び出し側（`gz diff`）の責務であり、ここでは
/// 一覧取得のオプションと `--` の付与だけを担う（[`commit_count`] と同方針）。
///
/// # Errors
///
/// - `git diff` の実行に失敗した場合は [`Error::GitCommandFailed`] 等
/// - 出力のパースに失敗した場合は [`Error::RepositoryReadFailed`]
pub fn changed_files(workdir: &Path, range: &[String]) -> Result<Vec<String>> {
    let mut arguments = vec!["diff", "--name-only", "-z"];
    arguments.extend(range.iter().map(String::as_str));
    // 末尾の `--` はリビジョンとパスの境界。同名のファイルがあってもパスとして解釈させない
    arguments.push("--");

    let output = capture_git_in(workdir, &arguments)?;

    parse_changed_files(&output)
}

/// `revision` が指すツリーに含まれるファイルの一覧を取得する。
///
/// # Errors
///
/// - リビジョンを解決できない場合は [`Error::RepositoryReadFailed`]
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - `git ls-tree` の実行・出力のパースに失敗した場合はそれぞれのエラー
pub fn revision_files(repository: &gix::Repository, revision: &str) -> Result<RevisionFiles> {
    let id = repository
        .rev_parse_single(revision)
        .map_err(|source| {
            read_error(
                ReadOperation::RevisionResolve {
                    revision: revision.to_owned(),
                },
                source,
            )
        })?
        .to_string();

    // `--full-tree` を付けないと ls-tree はカレントディレクトリ基準のパスを返すため、
    // `git status` と同じく作業ツリールート基準へ揃える
    let output = capture_git_in(
        workdir(repository)?,
        &["ls-tree", "-r", "--name-only", "-z", "--full-tree", &id],
    )?;

    let mut paths = Vec::new();
    for record in nul_records(&output) {
        paths.push(to_utf8(record.as_bstr(), ReadOperation::PathDecode)?);
    }

    Ok(RevisionFiles { id, paths })
}

/// 複数行のメッセージから 1 行目を取り出す。
fn first_line(message: &BStr) -> &BStr {
    let end = message
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(message.len());
    message[..end].as_bstr()
}

/// タグ一覧を名前順で取得する。
///
/// annotated tag（タグオブジェクトを伴うタグ）はそのメッセージの 1 行目も併せて返す。
///
/// # Errors
///
/// 参照の列挙・解決、タグオブジェクトの復号に失敗した場合、
/// またはタグ名・メッセージが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn tags(repository: &gix::Repository) -> Result<Vec<TagInfo>> {
    let platform = repository
        .references()
        .map_err(|source| read_error(ReadOperation::ReferenceList, source))?;

    let mut tags = Vec::new();
    for reference in platform
        .tags()
        .map_err(|source| read_error(ReadOperation::TagList, source))?
    {
        let mut reference =
            reference.map_err(|source| read_error(ReadOperation::TagRead, source))?;
        let name = to_utf8(reference.name().shorten(), ReadOperation::TagNameDecode)?;

        // シンボリック参照は辿るが、annotated tag のタグオブジェクトは peel せずに保持する。
        // タグのメッセージを読むにはタグオブジェクト自体が必要なため
        let id = reference.follow_to_object().map_err(|source| {
            read_error(ReadOperation::TagResolve { tag: name.clone() }, source)
        })?;
        let object = id.object().map_err(|source| {
            read_error(ReadOperation::TagTargetFetch { tag: name.clone() }, source)
        })?;

        let message = match object.kind {
            gix::object::Kind::Tag => {
                let tag = object.try_into_tag().map_err(|source| {
                    read_error(ReadOperation::TagParse { tag: name.clone() }, source)
                })?;
                let decoded = tag.decode().map_err(|source| {
                    read_error(ReadOperation::TagDecode { tag: name.clone() }, source)
                })?;
                Some(to_utf8(
                    first_line(decoded.message),
                    ReadOperation::TagMessageDecode,
                )?)
            }
            // lightweight tag は参照が直接コミット等を指すためメッセージを持たない
            _ => None,
        };

        tags.push(TagInfo {
            name,
            id: id.to_string(),
            message,
        });
    }

    tags.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tags)
}

/// HEAD の reflog を新しい順に取得する。
///
/// 削除されたブランチの調査が用途であり、gc 済みで実体を失ったオブジェクトを指すエントリも
/// そのまま返す（オブジェクトデータベースは参照しない）。
///
/// # Errors
///
/// HEAD の参照を開けない場合、reflog の読み取り・解釈に失敗した場合、
/// またはメッセージが UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn head_reflog(repository: &gix::Repository) -> Result<Vec<ReflogEntry>> {
    let reference = repository
        .find_reference("HEAD")
        .map_err(|source| read_error(ReadOperation::HeadRead, source))?;

    let mut platform = reference.log_iter();
    // reflog が 1 度も記録されていないリポジトリでは reflog ファイル自体が存在しない
    let Some(lines) = platform
        .rev()
        .map_err(|source| read_error(ReadOperation::HeadReflogRead, source))?
    else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    for (index, line) in lines.enumerate() {
        let line = line.map_err(|source| read_error(ReadOperation::HeadReflogParse, source))?;
        entries.push(ReflogEntry {
            index,
            id: line.new_oid.to_string(),
            message: to_utf8(line.message.as_bstr(), ReadOperation::ReflogMessageDecode)?,
        });
    }

    Ok(entries)
}

/// `git stash list` の実行引数。
///
/// - `--format=%gd%x00%gs`: 参照（`stash@{n}`）とメッセージを NUL で区切って出力させる。
///   既定の出力形式（`stash@{0}: WIP on main: 5d21a8c first`）はメッセージ自体がコロンを
///   含み得るため、コロンでの分割では参照とメッセージを確実に切り分けられない
/// - `-z`: エントリ同士も NUL で区切らせる。メッセージ（reflog subject）は改行を含まないが、
///   区切りを NUL に統一することでフィールド区切りと同じ規則で解析できる
///   （`git status` を `-z` で読むのと同じ方針）
const STASH_LIST_ARGS: [&str; 4] = ["stash", "list", "-z", "--format=%gd%x00%gs"];

/// stash の参照（`stash@{n}`）の前置き。
const STASH_SELECTOR_PREFIX: &str = "stash@{";

/// stash の参照（`stash@{n}`）の後置き。
const STASH_SELECTOR_SUFFIX: &str = "}";

/// `stash@{n}` 形式の参照から添字を取り出す。
fn parse_stash_selector(selector: &str) -> Option<usize> {
    selector
        .strip_prefix(STASH_SELECTOR_PREFIX)?
        .strip_suffix(STASH_SELECTOR_SUFFIX)?
        .parse()
        .ok()
}

/// [`STASH_LIST_ARGS`] の出力をパースする。
///
/// 出力は `<参照>\0<メッセージ>\0` の繰り返しで、末尾にも区切りの NUL が付く。
/// メッセージは空になり得るため空レコードは読み飛ばさず、2 件ずつの組として厳密に扱う。
///
/// # Errors
///
/// レコード数が奇数の場合、参照が `stash@{n}` 形式でない場合は
/// [`Error::GitOutputMalformed`]、メッセージが UTF-8 でない場合は
/// [`Error::RepositoryReadFailed`] を返す。
fn parse_stash_list(output: &[u8]) -> Result<Vec<StashEntry>> {
    if output.is_empty() {
        return Ok(Vec::new());
    }

    let mut records: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    // 最後のエントリの後ろにも区切りが置かれるため、その分の空レコードだけを取り除く
    if records.last().is_some_and(|record| record.is_empty()) {
        records.pop();
    }

    if !records.len().is_multiple_of(2) {
        return Err(malformed_output(
            ReadOperation::StashListOutputParse,
            MalformedOutput::StashRecordPairing {
                records: records.len(),
            },
        ));
    }

    let mut entries = Vec::with_capacity(records.len() / 2);
    for pair in records.chunks_exact(2) {
        let selector = to_utf8(pair[0].as_bstr(), ReadOperation::StashSelectorDecode)?;
        let index = parse_stash_selector(&selector).ok_or_else(|| {
            malformed_output(
                ReadOperation::StashListOutputParse,
                MalformedOutput::StashSelectorFormat {
                    selector: selector.clone(),
                },
            )
        })?;

        entries.push(StashEntry {
            index,
            message: to_utf8(pair[1].as_bstr(), ReadOperation::StashMessageDecode)?,
        });
    }

    Ok(entries)
}

/// stash 一覧を新しい順（`stash@{0}` から）に取得する。
///
/// # Errors
///
/// - 作業ツリーを持たない bare リポジトリの場合は [`Error::NoWorktree`]
/// - `git stash list` の実行に失敗した場合は [`Error::GitCommandFailed`] 等
/// - 出力の形式が想定と異なる場合は [`Error::GitOutputMalformed`]
/// - 出力が UTF-8 でない場合は [`Error::RepositoryReadFailed`]
pub fn stashes(repository: &gix::Repository) -> Result<Vec<StashEntry>> {
    let output = capture_git_in(workdir(repository)?, &STASH_LIST_ARGS)?;
    parse_stash_list(&output)
}

/// `git branch --merged` の出力形式。
///
/// 既定の出力は現在のブランチに `* ` を付け、worktree で使用中のブランチに `+ ` を付けるため、
/// 参照名だけを出させて印の解釈を不要にする。参照名は制御文字を含めない規則
/// （`git check-ref-format`）であり、改行で区切っても曖昧にならない。
const MERGED_BRANCH_FORMAT: &str = "--format=%(refname:short)";

/// `base` から到達可能な（＝ merged な）ローカルブランチの名前を集めて返す。
///
/// `base` には既定の基準として `HEAD` を、`gz branch delete --into` の指定時は
/// 検証済みのブランチ名を渡す。ブランチごとに merge-base を計算する代わりに
/// git へ一括で判定させる（候補生成での git 実行を 1 回に抑えるため）。
///
/// # Errors
///
/// `git branch --merged` の実行に失敗した場合は [`Error::GitCommandFailed`] 等、
/// 出力が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn merged_branches(workdir: &Path, base: &str) -> Result<HashSet<String>> {
    // 値は `--merged=<base>` の形で渡す。`-` で始まる値が別のオプションとして
    // 解釈される余地を残さないため
    let merged = format!("--merged={base}");
    let output = capture_git_in(workdir, &["branch", &merged, MERGED_BRANCH_FORMAT])?;

    parse_merged_branches(&output)
}

/// [`MERGED_BRANCH_FORMAT`] を伴う `git branch --merged` の出力をパースする。
///
/// 出力は 1 行 1 ブランチ名。空行は生じないが、末尾の改行だけが残ることはある。
///
/// # Errors
///
/// 出力が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_merged_branches(output: &[u8]) -> Result<HashSet<String>> {
    let text = to_utf8(output.as_bstr(), ReadOperation::MergedBranchOutputParse)?;

    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// ブランチごとの最終更新日時を取得する `git for-each-ref` の実行引数。
///
/// - `refs/heads`: ローカルブランチのみを対象にする
/// - `%00`: 参照名と日時をフィールドとして NUL で区切る。相対日時（`3 days ago`）は
///   空白を含むため、空白区切りでは切り分けられない（`git status` を `-z` で読むのと同じ方針）
const BRANCH_ACTIVITY_ARGS: [&str; 3] = [
    "for-each-ref",
    "refs/heads",
    "--format=%(refname:short)%00%(committerdate:relative)",
];

/// ローカルブランチごとの最終更新日時（`3 days ago` 形式の相対表記）を取得する。
///
/// `gz branch delete` の候補一覧に添える情報であり、全ブランチ分を 1 回のキャプチャで得る。
/// 相対表記の文言は git に委ね、fuzgit 側では整形しない。
///
/// # Errors
///
/// `git for-each-ref` の実行に失敗した場合は [`Error::GitCommandFailed`] 等、
/// 出力の形式が想定と異なる場合は [`Error::GitOutputMalformed`]、
/// 出力が UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
pub fn branch_activity(workdir: &Path) -> Result<HashMap<String, String>> {
    let output = capture_git_in(workdir, &BRANCH_ACTIVITY_ARGS)?;

    parse_branch_activity(&output)
}

/// [`BRANCH_ACTIVITY_ARGS`] の出力をパースする。
///
/// 1 行が 1 ブランチで、`<参照名>\0<相対日時>` の 2 フィールドからなる。
///
/// # Errors
///
/// フィールドが 2 つでない場合は [`Error::GitOutputMalformed`]、
/// UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_branch_activity(output: &[u8]) -> Result<HashMap<String, String>> {
    let text = to_utf8(output.as_bstr(), ReadOperation::ForEachRefOutputParse)?;

    let mut activity = HashMap::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let Some((name, relative_date)) = line.split_once('\0') else {
            return Err(malformed_output(
                ReadOperation::ForEachRefOutputParse,
                MalformedOutput::BranchActivityPair {
                    line: line.to_owned(),
                },
            ));
        };

        activity.insert(name.to_owned(), relative_date.to_owned());
    }

    Ok(activity)
}

/// worktree 一覧の実行引数。
///
/// gix の `Repository::worktrees()` は linked worktree しか返さず、チェックアウト中の
/// ブランチも持たないため、main を含む全件を 1 回で得られる git のキャプチャを使う。
/// `-z` を付けると各属性行が改行ではなく NUL で終端され、空白を含むパスも曖昧にならない。
const WORKTREE_LIST_ARGS: [&str; 4] = ["worktree", "list", "--porcelain", "-z"];

/// 各レコードの先頭に必ず現れる属性（パス）。
///
/// [`MalformedOutput::WorktreeRecordStart`] の説明にも現れるため、
/// 文言側（`i18n`）が値を重複して持たずに済むよう公開する。
pub(crate) const WORKTREE_LABEL: &str = "worktree";

/// チェックアウト中のコミットを示す属性。
const WORKTREE_HEAD_LABEL: &str = "HEAD";

/// チェックアウト中のブランチを示す属性。
const WORKTREE_BRANCH_LABEL: &str = "branch";

/// ロックされていることを示す属性（理由が無い場合はラベルのみ）。
const WORKTREE_LOCKED_LABEL: &str = "locked";

/// `git worktree prune` の対象になり得ることを示す属性。
const WORKTREE_PRUNABLE_LABEL: &str = "prunable";

/// worktree の `branch` 属性が持つ参照名の前置き。
///
/// [`MalformedOutput::WorktreeBranchReference`] の説明にも現れるため、
/// 文言側（`i18n`）が値を重複して持たずに済むよう公開する。
pub(crate) const BRANCH_REF_PREFIX: &str = "refs/heads/";

/// worktree 1 件分の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// worktree のパス（git が返す絶対パス）。
    pub path: String,
    /// チェックアウト中のコミットのフルハッシュ。
    ///
    /// bare な worktree には `HEAD` 属性が無いため `None` になる。まだコミットが無い
    /// ブランチ（unborn HEAD）では全 0 のハッシュが返る。
    pub head: Option<String>,
    /// チェックアウト中のブランチの短縮名。detached HEAD の場合は `None`。
    pub branch: Option<String>,
    /// main worktree（リポジトリ本体の作業ツリー）かどうか。
    ///
    /// `git worktree list` は main worktree を必ず先頭に出力する（man git-worktree
    /// 「list」の記載と git 2.55 の実機出力で確認）ため、先頭のレコードを main とする。
    pub is_main: bool,
    /// `git worktree lock` でロックされているかどうか。
    pub is_locked: bool,
    /// `git worktree prune` の対象になり得るか（実体を失っている等）どうか。
    pub prunable: bool,
}

/// worktree 一覧を取得する（main worktree が先頭）。
///
/// # Errors
///
/// - `git worktree list` の実行に失敗した場合は [`Error::GitCommandFailed`] 等
/// - 出力の形式が想定と異なる場合は [`Error::GitOutputMalformed`]
/// - 出力が UTF-8 でない場合は [`Error::RepositoryReadFailed`]
pub fn worktrees(workdir: &Path) -> Result<Vec<WorktreeInfo>> {
    let output = capture_git_in(workdir, &WORKTREE_LIST_ARGS)?;

    parse_worktree_list(&output)
}

/// worktree でチェックアウト中のブランチの短縮名を集める。
///
/// [`worktrees`] の結果を受け取るだけのプロセス内処理であり、追加の git 実行は行わない
/// （`gz branch delete` の候補生成で git の実行回数を増やさないため）。
/// detached HEAD の worktree はブランチを占有しないため含まれない。
///
/// git は同じブランチを複数の worktree で同時にチェックアウトできず、使用中のブランチの
/// 削除も拒否する。そのため呼び出し側は、この集合に含まれるブランチを候補から除外する。
#[must_use]
pub fn checked_out_branches(worktrees: &[WorktreeInfo]) -> HashSet<String> {
    worktrees
        .iter()
        .filter_map(|worktree| worktree.branch.clone())
        .collect()
}

/// 値を伴うはずの属性から値を取り出す。
///
/// ラベルのみの行になるのは真偽値の属性（`detached` / `bare` / 理由の無い `locked`）だけで、
/// パス・ハッシュ・参照名を持つ属性が値を欠くことは無い。欠けている場合は出力を
/// 読み違えているため、推測せずエラーにする。
fn required_value<'a>(label: &str, value: Option<&'a str>) -> Result<&'a str> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        malformed_output(
            ReadOperation::WorktreeListOutputParse,
            MalformedOutput::WorktreeAttributeValueMissing {
                label: label.to_owned(),
            },
        )
    })
}

/// `branch` 属性の参照名を短縮名へ変換する。
///
/// worktree がチェックアウトできるのはブランチ（`refs/heads/` 配下）だけであり、
/// それ以外の参照名は解釈できないため、短縮名を推測せずエラーにする。
fn short_branch_name(reference: &str) -> Result<String> {
    reference
        .strip_prefix(BRANCH_REF_PREFIX)
        .map(str::to_owned)
        .ok_or_else(|| {
            malformed_output(
                ReadOperation::WorktreeListOutputParse,
                MalformedOutput::WorktreeBranchReference {
                    reference: reference.to_owned(),
                },
            )
        })
}

/// [`WORKTREE_LIST_ARGS`] の出力をパースする。
///
/// 1 レコードは `worktree <path>` で始まり、`HEAD <hash>` / `branch <ref>` /
/// `detached` / `bare` / `locked [<reason>]` / `prunable [<reason>]` が続く。
/// 属性はラベルと値を 1 つの空白で区切った形式で、真偽値の属性（`detached` / `bare` /
/// 理由の無い `locked`）はラベルのみの行になる。レコードの終端は空行（`-z` では
/// 空のレコード）で、最後のレコードの後ろにも必ず置かれる。
///
/// 未知の属性は無視する。将来の git が属性を増やしても一覧が読めなくならないようにするため
/// （fuzgit が解釈する属性の意味を変えるわけではない）。
///
/// # Errors
///
/// レコードが `worktree` 属性で始まっていない場合、終端されていない場合、
/// ブランチの参照名が `refs/heads/` 配下でない場合は [`Error::GitOutputMalformed`]、
/// UTF-8 でない場合は [`Error::RepositoryReadFailed`] を返す。
fn parse_worktree_list(output: &[u8]) -> Result<Vec<WorktreeInfo>> {
    let mut worktrees: Vec<WorktreeInfo> = Vec::new();
    let mut current: Option<WorktreeInfo> = None;

    // `-z` の NUL は区切りではなく行の終端であるため、出力全体の末尾にも必ず 1 つ置かれる。
    // 分割で生じるその分の空要素だけを取り除き、残りをそのまま行として扱う
    // （レコード終端の空行と、終端の名残を取り違えないようにするため）
    let mut lines: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    for record in lines {
        if record.is_empty() {
            // 空行はレコードの終端
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }

        let line = to_utf8(record.as_bstr(), ReadOperation::WorktreeListOutputParse)?;
        let (label, value) = match line.split_once(' ') {
            Some((label, value)) => (label, Some(value)),
            None => (line.as_str(), None),
        };

        if label == WORKTREE_LABEL {
            let path = required_value(WORKTREE_LABEL, value)?;

            if let Some(previous) = &current {
                return Err(unterminated_record(&previous.path));
            }

            current = Some(WorktreeInfo {
                path: path.to_owned(),
                head: None,
                branch: None,
                // main worktree は必ず先頭に出力される
                is_main: worktrees.is_empty(),
                is_locked: false,
                prunable: false,
            });
            continue;
        }

        let Some(worktree) = current.as_mut() else {
            return Err(malformed_output(
                ReadOperation::WorktreeListOutputParse,
                MalformedOutput::WorktreeRecordStart { line: line.clone() },
            ));
        };

        match label {
            WORKTREE_HEAD_LABEL => {
                worktree.head = Some(required_value(WORKTREE_HEAD_LABEL, value)?.to_owned());
            }
            WORKTREE_BRANCH_LABEL => {
                worktree.branch = Some(short_branch_name(required_value(
                    WORKTREE_BRANCH_LABEL,
                    value,
                )?)?);
            }
            WORKTREE_LOCKED_LABEL => worktree.is_locked = true,
            WORKTREE_PRUNABLE_LABEL => worktree.prunable = true,
            // detached / bare は head・branch の有無で表せるため、真偽値としては保持しない。
            // それ以外の未知の属性とあわせて読み飛ばす
            _ => {}
        }
    }

    if let Some(worktree) = &current {
        return Err(unterminated_record(&worktree.path));
    }

    Ok(worktrees)
}

/// 空行で終端されていないレコードを検出した際のエラーを作る。
///
/// `git worktree list --porcelain` は最後のレコードの後ろにも空行を置く（man git-worktree
/// 「Porcelain Format」）。終端が無い出力は途中で切れているため、読めた分だけを返さない。
fn unterminated_record(path: &str) -> Error {
    malformed_output(
        ReadOperation::WorktreeListOutputParse,
        MalformedOutput::WorktreeRecordUnterminated {
            path: path.to_owned(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::discover;
    use crate::i18n::Language;
    use crate::test_support::{
        COMMIT_DATE_SHORT, TempDir, commit, commit_at, create_annotated_tag, create_branch,
        create_lightweight_tag, create_remote_branch, create_remote_symbolic_ref, git_in,
        init_repository, stash_changes, try_git_in, write_file,
    };

    /// `main` に 1 コミットだけ持つテストリポジトリを用意する。
    fn repository_with_one_commit(label: &str) -> (TempDir, gix::Repository, String) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository, head)
    }

    fn names(branches: &[BranchInfo]) -> Vec<&str> {
        branches.iter().map(|branch| branch.name.as_str()).collect()
    }

    /// 組み立てた引数を [`capture_git_in`] へ渡せる形へ変換する。
    fn to_str(arguments: &[String]) -> Vec<&str> {
        arguments.iter().map(String::as_str).collect()
    }

    fn summaries(commits: &[CommitInfo]) -> Vec<&str> {
        commits
            .iter()
            .map(|commit| commit.summary.as_str())
            .collect()
    }

    #[test]
    fn local_branches_are_listed_by_name_with_the_current_one_marked() {
        let (dir, repository, _head) = repository_with_one_commit("read-local-branches");
        create_branch(dir.path(), "feature");
        create_branch(dir.path(), "another");

        let branches = branches(&repository, BranchScope::Local).expect("branches should be read");

        assert_eq!(names(&branches), ["another", "feature", "main"]);
        assert!(branches.iter().all(|branch| !branch.is_remote));
        let current: Vec<&str> = branches
            .iter()
            .filter(|branch| branch.is_current)
            .map(|branch| branch.name.as_str())
            .collect();
        assert_eq!(current, ["main"]);
    }

    #[test]
    fn a_repository_without_commits_reports_the_unborn_branch_instead_of_an_empty_list() {
        let dir = TempDir::new("read-unborn-branches");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        for scope in [BranchScope::Local, BranchScope::All] {
            let err = branches(&repository, scope)
                .expect_err("an unborn HEAD should be reported as its own error");

            match err {
                Error::UnbornHead { branch } => assert_eq!(branch, "main"),
                other => panic!("unexpected error for {scope:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn an_orphan_branch_does_not_hide_the_existing_branches() {
        let (dir, _repository, _head) = repository_with_one_commit("read-orphan-branch");
        git_in(dir.path(), &["checkout", "--quiet", "--orphan", "orphan"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let branches = branches(&repository, BranchScope::Local).expect("branches should be read");

        assert_eq!(names(&branches), ["main"]);
    }

    #[test]
    fn a_detached_head_marks_no_branch_as_current() {
        let (dir, _repository, head) = repository_with_one_commit("read-detached-head");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let branches = branches(&repository, BranchScope::Local).expect("branches should be read");

        assert_eq!(names(&branches), ["main"]);
        assert!(branches.iter().all(|branch| !branch.is_current));
    }

    #[test]
    fn remote_branches_are_excluded_unless_the_scope_is_all() {
        let (dir, repository, head) = repository_with_one_commit("read-remote-scope");
        create_remote_branch(dir.path(), "origin/main", &head);

        let local = branches(&repository, BranchScope::Local).expect("branches should be read");
        assert_eq!(names(&local), ["main"]);

        let all = branches(&repository, BranchScope::All).expect("branches should be read");
        assert_eq!(names(&all), ["main", "origin/main"]);
        assert_eq!(
            all.iter()
                .map(|branch| branch.is_remote)
                .collect::<Vec<_>>(),
            [false, true]
        );
    }

    #[test]
    fn symbolic_remote_references_are_not_offered_as_candidates() {
        let (dir, repository, head) = repository_with_one_commit("read-remote-symbolic");
        create_remote_branch(dir.path(), "origin/main", &head);
        create_remote_symbolic_ref(dir.path(), "origin/HEAD", "origin/main");

        let all = branches(&repository, BranchScope::All).expect("branches should be read");

        assert_eq!(names(&all), ["main", "origin/main"]);
    }

    #[test]
    fn commits_are_returned_from_head_backwards() {
        let dir = TempDir::new("read-commits-order");
        init_repository(dir.path());
        commit(dir.path(), "first");
        commit(dir.path(), "second");
        let third = commit(dir.path(), "third");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits = commits(&repository, CommitScope::Head, 10).expect("commits should be read");

        assert_eq!(summaries(&commits), ["third", "second", "first"]);
        assert_eq!(commits[0].id, third);
    }

    #[test]
    fn commits_are_truncated_to_the_limit() {
        let dir = TempDir::new("read-commits-limit");
        init_repository(dir.path());
        commit(dir.path(), "first");
        commit(dir.path(), "second");
        commit(dir.path(), "third");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits = commits(&repository, CommitScope::Head, 2).expect("commits should be read");

        assert_eq!(summaries(&commits), ["third", "second"]);
    }

    #[test]
    fn a_limit_of_zero_yields_no_commits() {
        let (_dir, repository, _head) = repository_with_one_commit("read-commits-zero");

        let commits = commits(&repository, CommitScope::Head, 0).expect("commits should be read");

        assert!(commits.is_empty(), "unexpected commits: {commits:?}");
    }

    #[test]
    fn a_repository_without_commits_reports_the_unborn_branch() {
        let dir = TempDir::new("read-commits-unborn");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = commits(&repository, CommitScope::Head, 10)
            .expect_err("an unborn HEAD should be reported as its own error");

        match err {
            Error::UnbornHead { branch } => assert_eq!(branch, "main"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn an_orphan_branch_is_reported_as_unborn_even_when_other_commits_exist() {
        let (dir, _repository, _head) = repository_with_one_commit("read-commits-orphan");
        git_in(dir.path(), &["checkout", "--quiet", "--orphan", "orphan"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = commits(&repository, CommitScope::Head, 10)
            .expect_err("an unborn HEAD should be reported as its own error");

        match err {
            Error::UnbornHead { branch } => assert_eq!(branch, "orphan"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 2 本のブランチに分岐した履歴を持つテストリポジトリを用意する。
    ///
    /// ```text
    /// first (01-01) ─┬─ main only    (01-02)  <- main（HEAD）
    ///                └─ feature only (01-03)  <- feature
    /// ```
    ///
    /// コミット日時をずらしてあるため、日時順とブランチのまとまりが一致しない。
    fn repository_with_two_branches(label: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        commit_at(dir.path(), "first", "2024-01-01T00:00:00+00:00");
        git_in(dir.path(), &["switch", "--quiet", "-c", "feature"]);
        commit_at(dir.path(), "feature only", "2024-01-03T00:00:00+00:00");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        commit_at(dir.path(), "main only", "2024-01-02T00:00:00+00:00");

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn the_head_scope_ignores_commits_of_other_branches() {
        let (_dir, repository) = repository_with_two_branches("read-commits-head-scope");

        let commits = commits(&repository, CommitScope::Head, 10).expect("commits should be read");

        assert_eq!(summaries(&commits), ["main only", "first"]);
    }

    #[test]
    fn the_all_branches_scope_merges_every_branch_by_commit_time() {
        let (_dir, repository) = repository_with_two_branches("read-commits-all-scope");

        let commits =
            commits(&repository, CommitScope::AllBranches, 10).expect("commits should be read");

        assert_eq!(
            summaries(&commits),
            ["feature only", "main only", "first"],
            "commits of all branches should be interleaved by commit time"
        );
    }

    #[test]
    fn a_commit_shared_by_several_branches_is_listed_once() {
        let (_dir, repository) = repository_with_two_branches("read-commits-shared");

        let commits =
            commits(&repository, CommitScope::AllBranches, 10).expect("commits should be read");

        let shared = commits
            .iter()
            .filter(|commit| commit.summary == "first")
            .count();
        assert_eq!(shared, 1, "the shared ancestor should not be duplicated");
    }

    #[test]
    fn the_all_branches_scope_includes_remote_tracking_branches() {
        let (dir, _repository) = repository_with_two_branches("read-commits-remote-scope");
        git_in(dir.path(), &["switch", "--quiet", "-c", "temporary"]);
        let head = commit_at(dir.path(), "remote only", "2024-01-04T00:00:00+00:00");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        // ローカルブランチを消し、リモート追跡ブランチからのみ辿れるコミットにする
        git_in(dir.path(), &["branch", "--delete", "--force", "temporary"]);
        create_remote_branch(dir.path(), "origin/temporary", &head);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let commits =
            commits(&repository, CommitScope::AllBranches, 10).expect("commits should be read");

        assert_eq!(
            summaries(&commits),
            ["remote only", "feature only", "main only", "first"]
        );
    }

    #[test]
    fn the_all_branches_scope_is_truncated_to_the_limit_across_branches() {
        let (_dir, repository) = repository_with_two_branches("read-commits-all-limit");

        let commits =
            commits(&repository, CommitScope::AllBranches, 2).expect("commits should be read");

        assert_eq!(summaries(&commits), ["feature only", "main only"]);
    }

    #[test]
    fn a_branch_scope_is_limited_to_that_branch() {
        let (_dir, repository) = repository_with_two_branches("read-commits-branch-scope");

        let commits = commits(&repository, CommitScope::Branch("feature"), 10)
            .expect("commits should be read");

        assert_eq!(summaries(&commits), ["feature only", "first"]);
    }

    #[test]
    fn an_unknown_branch_is_reported_with_its_name() {
        let (_dir, repository) = repository_with_two_branches("read-commits-unknown-branch");

        let err = commits(&repository, CommitScope::Branch("no-such-branch"), 10)
            .expect_err("an unknown branch must not be silently ignored");

        match &err {
            Error::RepositoryReadFailed { operation, .. } => {
                assert_eq!(
                    *operation,
                    ReadOperation::BranchResolve {
                        branch: "no-such-branch".to_owned()
                    }
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // 表示は言語ごとの `describe` が組み立てるため、言語を明示して検証する
        // （enum 化する前はここでエラーの `operation` が持つ日本語を直接アサートしていた）
        for language in [Language::Japanese, Language::English] {
            let described = language.messages().errors().describe(&err);
            assert!(
                described.contains("no-such-branch"),
                "{language:?} must name the branch: {described}"
            );
        }
    }

    /// `git status --porcelain -z` の出力を組み立てる（各レコードの末尾に NUL を置く）。
    fn status_output(records: &[&str]) -> Vec<u8> {
        let mut output = Vec::new();
        for record in records {
            output.extend_from_slice(record.as_bytes());
            output.push(0);
        }
        output
    }

    fn change(path: &str, code: &str) -> FileChange {
        let mut codes = code.chars();
        FileChange {
            path: path.to_owned(),
            original_path: None,
            index_status: codes.next().expect("a status code has two characters"),
            worktree_status: codes.next().expect("a status code has two characters"),
        }
    }

    fn paths(changes: &[FileChange]) -> Vec<&str> {
        changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>()
    }

    #[test]
    fn an_empty_status_yields_no_changes() {
        let changes = parse_status(&[]).expect("empty output should parse");

        assert!(changes.is_empty(), "unexpected changes: {changes:?}");
    }

    #[test]
    fn staged_unstaged_and_untracked_entries_are_distinguished() {
        let output = status_output(&["M  staged.txt", " M unstaged.txt", "?? new.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(paths(&changes), ["staged.txt", "unstaged.txt", "new.txt"]);
        assert_eq!(
            changes
                .iter()
                .map(FileChange::status_code)
                .collect::<Vec<_>>(),
            ["M ", " M", "??"]
        );
        assert_eq!(
            changes
                .iter()
                .map(FileChange::has_staged_change)
                .collect::<Vec<_>>(),
            [true, false, false]
        );
        assert_eq!(
            changes
                .iter()
                .map(FileChange::has_worktree_change)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(
            changes
                .iter()
                .map(FileChange::is_untracked)
                .collect::<Vec<_>>(),
            [false, false, true]
        );
    }

    #[test]
    fn an_entry_can_be_staged_and_unstaged_at_the_same_time() {
        let output = status_output(&["MM both.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let change = changes.first().expect("one entry should be parsed");
        assert!(change.has_staged_change());
        assert!(change.has_worktree_change());
        assert!(!change.is_untracked());
    }

    #[test]
    fn a_path_containing_spaces_is_kept_verbatim() {
        let output = status_output(&[" M dir/with space.txt", "?? another one.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(
            paths(&changes),
            ["dir/with space.txt", "another one.txt"],
            "NUL separated records must not be split on spaces"
        );
    }

    #[test]
    fn a_path_starting_with_a_dash_is_kept_verbatim() {
        let output = status_output(&["?? --looks-like-an-option"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(paths(&changes), ["--looks-like-an-option"]);
    }

    #[test]
    fn a_rename_keeps_both_the_new_and_the_original_path() {
        // `-z` では変更後のパスが先、変更元が次のレコードになる
        let output = status_output(&["R  new.txt", "old.txt", "?? other.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(paths(&changes), ["new.txt", "other.txt"]);
        let rename = changes.first().expect("the rename should be parsed");
        assert_eq!(rename.original_path.as_deref(), Some("old.txt"));
        assert_eq!(rename.status_code(), "R ");
    }

    #[test]
    fn a_rename_with_a_worktree_change_is_reported_once() {
        let output = status_output(&["RM new name.txt", "old name.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert_eq!(changes.len(), 1);
        let rename = changes.first().expect("the rename should be parsed");
        assert_eq!(rename.path, "new name.txt");
        assert_eq!(rename.original_path.as_deref(), Some("old name.txt"));
        assert!(rename.has_staged_change());
        assert!(rename.has_worktree_change());
    }

    #[test]
    fn a_copy_also_carries_the_original_path() {
        let output = status_output(&["C  copy.txt", "source.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let copy = changes.first().expect("the copy should be parsed");
        assert_eq!(copy.original_path.as_deref(), Some("source.txt"));
    }

    #[test]
    fn a_rename_detected_in_the_work_tree_also_carries_the_original_path() {
        let output = status_output(&[" R new.txt", "old.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let rename = changes.first().expect("the rename should be parsed");
        assert_eq!(rename.original_path.as_deref(), Some("old.txt"));
        assert!(rename.has_worktree_change());
        assert!(!rename.has_staged_change());
    }

    #[test]
    fn an_unmerged_entry_is_reported_for_both_sides() {
        let output = status_output(&["UU conflict.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        let conflict = changes.first().expect("the conflict should be parsed");
        assert!(conflict.has_staged_change());
        assert!(conflict.has_worktree_change());
        assert!(!conflict.is_untracked());
    }

    #[test]
    fn a_deletion_is_reported_on_the_side_it_happened() {
        let output = status_output(&["D  staged-delete.txt", " D worktree-delete.txt"]);

        let changes = parse_status(&output).expect("status output should parse");

        assert!(changes[0].has_staged_change());
        assert!(!changes[0].has_worktree_change());
        assert!(changes[1].has_worktree_change());
        assert!(!changes[1].has_staged_change());
    }

    #[test]
    fn a_malformed_entry_is_rejected_instead_of_being_skipped() {
        for record in ["M", "M  ", "M-path.txt"] {
            let output = status_output(&[record]);

            let err =
                parse_status(&output).expect_err("a malformed entry must not be silently ignored");

            assert!(
                matches!(
                    err,
                    Error::GitOutputMalformed {
                        detail: MalformedOutput::StatusEntry { .. },
                        ..
                    }
                ),
                "unexpected error for {record:?}: {err:?}"
            );
        }
    }

    #[test]
    fn a_rename_without_its_original_path_is_rejected() {
        let output = status_output(&["R  new.txt"]);

        let err = parse_status(&output).expect_err("a truncated rename entry must not be accepted");

        match &err {
            Error::GitOutputMalformed { operation, detail } => {
                assert_eq!(*operation, ReadOperation::StatusOutputParse);
                assert_eq!(
                    *detail,
                    MalformedOutput::StatusRenameOriginMissing {
                        path: "new.txt".to_owned()
                    }
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // ja を明示した表示の検証。enum 化する前にここへ直接書いていた日本語
        // （`git status 出力の解釈` と対象パス）が `describe` 経由で出ることを確かめる
        let described = Language::Japanese.messages().errors().describe(&err);
        assert!(
            described.contains("git status 出力の解釈"),
            "the Japanese description must name the operation: {described}"
        );
        assert!(
            described.contains("new.txt"),
            "the Japanese description must name the affected path: {described}"
        );
    }

    #[test]
    fn a_non_utf8_path_is_rejected_instead_of_being_converted_lossily() {
        let mut output = b"?? invalid-".to_vec();
        output.push(0xff);
        output.push(0);

        let err = parse_status(&output).expect_err("a non utf-8 path must not be accepted");

        assert!(
            matches!(err, Error::RepositoryReadFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn a_scope_selects_the_matching_side_of_a_change() {
        let staged = change("a.txt", "M ");
        let unstaged = change("b.txt", " M");
        let both = change("c.txt", "MM");
        let untracked = change("d.txt", "??");

        for (scope, expected) in [
            (ChangeScope::Staged, [true, false, true, false]),
            (ChangeScope::Worktree, [false, true, true, false]),
            (ChangeScope::Stageable, [false, true, true, true]),
            (ChangeScope::Tracked, [true, true, true, false]),
            (ChangeScope::TrackedOrUntracked, [true, true, true, true]),
        ] {
            let actual =
                [&staged, &unstaged, &both, &untracked].map(|change| scope.includes(change));
            assert_eq!(actual, expected, "unexpected selection for {scope:?}");
        }
    }

    /// 各種の変更を持つテストリポジトリを用意する。
    ///
    /// ```text
    /// staged.txt        M    ステージ済みの変更のみ
    /// unstaged.txt       M   未ステージの変更のみ
    /// both.txt          MM   両方
    /// dir/new file.txt  ??   未追跡（空白入りのパス）
    /// renamed.txt       R    ステージ済みのリネーム（元: history.txt）
    /// ```
    fn repository_with_changes(label: &str) -> (TempDir, gix::Repository) {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        for name in ["staged.txt", "unstaged.txt", "both.txt"] {
            write_file(dir.path(), name, "original\n");
        }
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");

        write_file(dir.path(), "staged.txt", "staged\n");
        write_file(dir.path(), "both.txt", "staged\n");
        git_in(dir.path(), &["add", "--", "staged.txt", "both.txt"]);
        write_file(dir.path(), "unstaged.txt", "unstaged\n");
        write_file(dir.path(), "both.txt", "unstaged\n");
        write_file(dir.path(), "dir/new file.txt", "untracked\n");
        git_in(dir.path(), &["mv", "history.txt", "renamed.txt"]);

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        (dir, repository)
    }

    #[test]
    fn the_staged_scope_lists_the_index_side_of_the_changes() {
        let (_dir, repository) = repository_with_changes("read-status-staged");

        let changes = changes(&repository, ChangeScope::Staged).expect("status should be read");

        assert_eq!(paths(&changes), ["both.txt", "renamed.txt", "staged.txt"]);
    }

    #[test]
    fn the_worktree_scope_lists_the_unstaged_changes_only() {
        let (_dir, repository) = repository_with_changes("read-status-worktree");

        let changes = changes(&repository, ChangeScope::Worktree).expect("status should be read");

        assert_eq!(paths(&changes), ["both.txt", "unstaged.txt"]);
    }

    #[test]
    fn the_stageable_scope_lists_unstaged_changes_together_with_untracked_files() {
        let (_dir, repository) = repository_with_changes("read-status-stageable");

        let changes = changes(&repository, ChangeScope::Stageable).expect("status should be read");

        assert_eq!(
            paths(&changes),
            ["both.txt", "unstaged.txt", "dir/new file.txt"],
            "untracked files with spaces should be listed individually"
        );
        let untracked = changes.last().expect("the untracked file should be listed");
        assert!(untracked.is_untracked());
    }

    #[test]
    fn the_tracked_scope_lists_staged_and_unstaged_changes_without_untracked_files() {
        let (_dir, repository) = repository_with_changes("read-status-tracked");

        let changes = changes(&repository, ChangeScope::Tracked).expect("status should be read");

        assert_eq!(
            paths(&changes),
            ["both.txt", "renamed.txt", "staged.txt", "unstaged.txt"],
            "`git stash push` also stashes changes that are only staged"
        );
    }

    #[test]
    fn the_tracked_or_untracked_scope_adds_the_untracked_files() {
        let (_dir, repository) = repository_with_changes("read-status-tracked-untracked");

        let changes =
            changes(&repository, ChangeScope::TrackedOrUntracked).expect("status should be read");

        assert_eq!(
            paths(&changes),
            [
                "both.txt",
                "renamed.txt",
                "staged.txt",
                "unstaged.txt",
                "dir/new file.txt"
            ]
        );
    }

    #[test]
    fn a_staged_rename_reports_both_of_its_paths() {
        let (_dir, repository) = repository_with_changes("read-status-rename");

        let changes = changes(&repository, ChangeScope::Staged).expect("status should be read");

        let rename = changes
            .iter()
            .find(|change| change.path == "renamed.txt")
            .expect("the rename should be listed");
        assert_eq!(rename.original_path.as_deref(), Some("history.txt"));
        assert_eq!(rename.index_status, 'R');
    }

    #[test]
    fn an_unmodified_repository_has_no_changes() {
        let (_dir, repository, _head) = repository_with_one_commit("read-status-clean");

        for scope in [
            ChangeScope::Staged,
            ChangeScope::Worktree,
            ChangeScope::Stageable,
            ChangeScope::Tracked,
            ChangeScope::TrackedOrUntracked,
        ] {
            let changes = changes(&repository, scope).expect("status should be read");
            assert!(changes.is_empty(), "unexpected changes for {scope:?}");
        }
    }

    #[test]
    fn a_bare_repository_has_no_work_tree_to_inspect() {
        let dir = TempDir::new("read-status-bare");
        crate::test_support::init_bare_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = changes(&repository, ChangeScope::Worktree)
            .expect_err("a bare repository has no work tree");

        assert!(
            matches!(err, Error::NoWorktree),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn an_empty_diff_listing_yields_no_files() {
        let files = parse_changed_files(&[]).expect("empty output should parse");

        assert!(files.is_empty(), "unexpected files: {files:?}");
    }

    #[test]
    fn a_single_changed_file_is_read_from_its_nul_terminated_record() {
        // `-z` は区切りではなく終端であるため、1 件でも末尾に NUL が付く
        let files = parse_changed_files(b"src/main.rs\0").expect("one record should parse");

        assert_eq!(files, ["src/main.rs"]);
    }

    #[test]
    fn changed_files_are_read_in_the_order_git_reported_them() {
        let files = parse_changed_files(b"b.txt\0a.txt\0dir/c.txt\0")
            .expect("several records should parse");

        assert_eq!(files, ["b.txt", "a.txt", "dir/c.txt"]);
    }

    #[test]
    fn a_changed_path_containing_spaces_is_kept_verbatim() {
        // `-z` はエスケープを行わないため、空白を含むパスもそのまま 1 レコードになる
        let files = parse_changed_files(b"dir/new file.txt\0with  two.txt\0")
            .expect("paths with spaces should parse");

        assert_eq!(files, ["dir/new file.txt", "with  two.txt"]);
    }

    #[test]
    fn a_missing_trailing_nul_still_yields_the_last_path() {
        let files = parse_changed_files(b"a.txt\0b.txt").expect("the last record should parse");

        assert_eq!(files, ["a.txt", "b.txt"]);
    }

    #[test]
    fn a_non_utf8_changed_path_is_rejected_instead_of_being_converted_lossily() {
        let err =
            parse_changed_files(b"\xff\xfe.txt\0").expect_err("a non utf-8 path must be rejected");

        assert!(
            matches!(err, Error::RepositoryReadFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn the_unstaged_range_lists_the_work_tree_side_of_the_changes() {
        let (dir, _repository) = repository_with_changes("read-changed-files-unstaged");

        let files = changed_files(dir.path(), &[]).expect("the diff should be listed");

        assert_eq!(
            files,
            ["both.txt", "unstaged.txt"],
            "an empty range compares the index with the work tree"
        );
    }

    #[test]
    fn the_staged_range_lists_the_index_side_of_the_changes() {
        let (dir, _repository, _head) = repository_with_one_commit("read-changed-files-staged");
        write_file(dir.path(), "staged.txt", "staged\n");
        write_file(dir.path(), "unstaged.txt", "unstaged\n");
        git_in(dir.path(), &["add", "--", "staged.txt"]);

        let files =
            changed_files(dir.path(), &["--staged".to_owned()]).expect("the diff should be listed");

        assert_eq!(
            files,
            ["staged.txt"],
            "`--staged` compares HEAD with the index only"
        );
    }

    #[test]
    fn a_revision_range_lists_the_files_that_differ_between_the_two_sides() {
        let (dir, _repository, _head) = repository_with_one_commit("read-changed-files-revisions");
        create_branch(dir.path(), "feature");
        git_in(dir.path(), &["switch", "--quiet", "feature"]);
        write_file(dir.path(), "added.txt", "feature\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "add a file on the feature branch");
        git_in(dir.path(), &["switch", "--quiet", "main"]);

        let range = ["main".to_owned(), "feature".to_owned()];
        let files = changed_files(dir.path(), &range).expect("the diff should be listed");

        // `commit` はコミットのたびに history.txt へ 1 行追記するため、両方が差分に現れる
        assert_eq!(files, ["added.txt", "history.txt"]);

        // 引数の順が比較の向きを決める。逆向きでも差分のあるファイルの集合は変わらない
        let reversed = ["feature".to_owned(), "main".to_owned()];
        let files = changed_files(dir.path(), &reversed).expect("the diff should be listed");

        assert_eq!(files, ["added.txt", "history.txt"]);
    }

    #[test]
    fn a_range_without_differences_lists_nothing() {
        let (dir, _repository, _head) = repository_with_one_commit("read-changed-files-empty");

        let range = ["HEAD".to_owned(), "HEAD".to_owned()];
        let files = changed_files(dir.path(), &range).expect("the diff should be listed");

        assert!(files.is_empty(), "unexpected files: {files:?}");
    }

    #[test]
    fn revision_files_lists_the_files_of_that_revision_with_its_resolved_id() {
        let (dir, repository) = repository_with_changes("read-revision-files");
        let head = git_in(dir.path(), &["rev-parse", "HEAD"]);

        let files = revision_files(&repository, "HEAD").expect("HEAD should be listed");

        assert_eq!(files.id, head, "the revision should be resolved to its id");
        assert_eq!(
            files.paths,
            ["both.txt", "history.txt", "staged.txt", "unstaged.txt"],
            "the listing must reflect the commit, not the work tree"
        );
    }

    #[test]
    fn revision_files_lists_nested_paths_from_the_repository_root() {
        let dir = TempDir::new("read-revision-files-nested");
        init_repository(dir.path());
        write_file(dir.path(), "dir/with space.txt", "content\n");
        git_in(dir.path(), &["add", "--all"]);
        commit(dir.path(), "first commit");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let files = revision_files(&repository, "HEAD").expect("HEAD should be listed");

        assert!(
            files.paths.contains(&"dir/with space.txt".to_owned()),
            "unexpected listing: {:?}",
            files.paths
        );
    }

    #[test]
    fn an_unknown_revision_is_reported_with_its_name() {
        let (_dir, repository, _head) = repository_with_one_commit("read-revision-unknown");

        let err = revision_files(&repository, "no-such-revision")
            .expect_err("an unknown revision must not be silently ignored");

        match &err {
            Error::RepositoryReadFailed { operation, .. } => assert_eq!(
                *operation,
                ReadOperation::RevisionResolve {
                    revision: "no-such-revision".to_owned()
                }
            ),
            other => panic!("unexpected error: {other:?}"),
        }

        // 表示は言語ごとの `describe` が組み立てるため、言語を明示して検証する
        for language in [Language::Japanese, Language::English] {
            let described = language.messages().errors().describe(&err);
            assert!(
                described.contains("no-such-revision"),
                "{language:?} must name the revision: {described}"
            );
        }
    }

    #[test]
    fn a_repository_without_tags_yields_no_candidates() {
        let (_dir, repository, _head) = repository_with_one_commit("read-tags-empty");

        let tags = tags(&repository).expect("tags should be read");

        assert!(tags.is_empty(), "unexpected tags: {tags:?}");
    }

    #[test]
    fn lightweight_and_annotated_tags_are_listed_by_name() {
        let (dir, repository, head) = repository_with_one_commit("read-tags-kinds");
        create_lightweight_tag(dir.path(), "v2.0-light");
        create_annotated_tag(dir.path(), "v1.0", "リリース v1.0\n\n詳細な説明");

        let tags = tags(&repository).expect("tags should be read");

        assert_eq!(
            tags.iter().map(|tag| tag.name.as_str()).collect::<Vec<_>>(),
            ["v1.0", "v2.0-light"]
        );

        let annotated = tags.first().expect("the annotated tag should be listed");
        assert_eq!(
            annotated.message.as_deref(),
            Some("リリース v1.0"),
            "only the first line of the message is kept"
        );
        assert_ne!(
            annotated.id, head,
            "an annotated tag reference points at the tag object itself"
        );

        let lightweight = tags.last().expect("the lightweight tag should be listed");
        assert_eq!(
            lightweight.message, None,
            "a lightweight tag carries no message"
        );
        assert_eq!(
            lightweight.id, head,
            "a lightweight tag reference points at the commit"
        );
    }

    #[test]
    fn an_annotated_tag_id_resolves_to_its_commit_for_git() {
        let (dir, repository, head) = repository_with_one_commit("read-tags-peel");
        create_annotated_tag(dir.path(), "v1.0", "リリース");
        let tags = tags(&repository).expect("tags should be read");
        let tag = tags.first().expect("the tag should be listed");

        let peeled = git_in(
            dir.path(),
            &["rev-parse", &format!("{id}^{{commit}}", id = tag.id)],
        );

        assert_eq!(peeled, head);
    }

    #[test]
    fn the_head_reflog_is_returned_newest_first() {
        let dir = TempDir::new("read-reflog-order");
        init_repository(dir.path());
        commit(dir.path(), "first");
        commit(dir.path(), "second");
        git_in(dir.path(), &["switch", "--quiet", "-c", "feature"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = head_reflog(&repository).expect("the reflog should be read");

        let messages: Vec<&str> = entries.iter().map(|entry| entry.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "checkout: moving from main to feature",
                "commit: second",
                "commit (initial): first"
            ]
        );
        assert_eq!(
            entries.iter().map(|entry| entry.index).collect::<Vec<_>>(),
            [0, 1, 2],
            "the newest entry is HEAD@{{0}}"
        );
    }

    #[test]
    fn a_reflog_entry_carries_the_commit_it_moved_to() {
        let dir = TempDir::new("read-reflog-id");
        init_repository(dir.path());
        let first = commit(dir.path(), "first");
        let second = commit(dir.path(), "second");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = head_reflog(&repository).expect("the reflog should be read");

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [second, first]
        );
    }

    #[test]
    fn a_repository_without_a_reflog_yields_no_entries() {
        let dir = TempDir::new("read-reflog-empty");
        init_repository(dir.path());
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = head_reflog(&repository).expect("a missing reflog is not an error");

        assert!(entries.is_empty(), "unexpected entries: {entries:?}");
    }

    /// `git stash list -z --format=%gd%x00%gs` の出力を組み立てる。
    ///
    /// 実際の git は各レコードの後ろに NUL を置く（末尾のエントリも同様）。
    fn stash_output(records: &[&str]) -> Vec<u8> {
        let mut output = Vec::new();
        for record in records {
            output.extend_from_slice(record.as_bytes());
            output.push(0);
        }
        output
    }

    #[test]
    fn an_empty_stash_list_yields_no_entries() {
        let entries = parse_stash_list(&[]).expect("empty output should parse");

        assert!(entries.is_empty(), "unexpected entries: {entries:?}");
    }

    #[test]
    fn a_stash_entry_keeps_its_index_and_message() {
        let output = stash_output(&[
            "stash@{0}",
            "On main: 作業中",
            "stash@{1}",
            "WIP on main: 5d21a8c first",
        ]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(
            entries,
            [
                StashEntry {
                    index: 0,
                    message: "On main: 作業中".to_owned()
                },
                StashEntry {
                    index: 1,
                    message: "WIP on main: 5d21a8c first".to_owned()
                }
            ]
        );
    }

    #[test]
    fn a_message_containing_colons_and_spaces_is_kept_verbatim() {
        let output = stash_output(&["stash@{0}", "On main: fix: 認証 の バグ: 2 件"]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(
            entries[0].message, "On main: fix: 認証 の バグ: 2 件",
            "a colon inside the message must not split the record"
        );
    }

    #[test]
    fn an_output_without_a_trailing_separator_parses_as_well() {
        let mut output = stash_output(&["stash@{0}"]);
        output.extend_from_slice("On main: 作業中".as_bytes());

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "On main: 作業中");
    }

    #[test]
    fn an_empty_message_does_not_shift_the_following_entries() {
        let output = stash_output(&["stash@{0}", "", "stash@{1}", "On main: 次"]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(
            entries,
            [
                StashEntry {
                    index: 0,
                    message: String::new()
                },
                StashEntry {
                    index: 1,
                    message: "On main: 次".to_owned()
                }
            ]
        );
    }

    #[test]
    fn a_double_digit_index_is_parsed() {
        let output = stash_output(&["stash@{12}", "On main: 作業中"]);

        let entries = parse_stash_list(&output).expect("stash list should parse");

        assert_eq!(entries[0].index, 12);
        assert_eq!(entries[0].selector(), "stash@{12}");
    }

    #[test]
    fn a_dangling_record_is_rejected_instead_of_being_dropped() {
        let mut output = stash_output(&["stash@{0}", "On main: 作業中"]);
        output.extend_from_slice(b"stash@{1}\0");

        let err = parse_stash_list(&output).expect_err("an odd record count must be rejected");

        assert!(
            matches!(
                err,
                Error::GitOutputMalformed {
                    detail: MalformedOutput::StashRecordPairing { .. },
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn a_selector_of_an_unexpected_shape_is_rejected() {
        for selector in ["stash@{x}", "stash@{}", "refs/stash", "stash@{0", "0"] {
            let output = stash_output(&[selector, "On main: 作業中"]);

            let err = parse_stash_list(&output)
                .expect_err("a selector that is not stash@{n} must be rejected");

            match &err {
                Error::GitOutputMalformed {
                    detail: MalformedOutput::StashSelectorFormat { selector: reported },
                    ..
                } => assert_eq!(reported, selector),
                other => panic!("unexpected error for {selector:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_non_utf8_message_is_rejected_instead_of_being_converted_lossily() {
        let mut output = b"stash@{0}\0On main: ".to_vec();
        output.push(0xff);
        output.push(0);

        let err = parse_stash_list(&output).expect_err("a non utf-8 message must not be accepted");

        assert!(
            matches!(err, Error::RepositoryReadFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn the_stash_list_of_a_repository_is_read_newest_first() {
        let dir = TempDir::new("read-stash-list");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        write_file(dir.path(), "history.txt", "旧: 変更 1\n");
        stash_changes(dir.path(), Some("最初の: 退避"));
        write_file(dir.path(), "history.txt", "旧: 変更 2\n");
        stash_changes(dir.path(), None);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let entries = stashes(&repository).expect("the stash list should be read");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 0);
        assert!(
            entries[0].message.starts_with("WIP on main:"),
            "the newest stash comes first: {entries:?}"
        );
        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].message, "On main: 最初の: 退避");
    }

    #[test]
    fn a_repository_without_stashes_yields_no_entries() {
        let (_dir, repository, _head) = repository_with_one_commit("read-stash-empty");

        let entries = stashes(&repository).expect("the stash list should be read");

        assert!(entries.is_empty(), "unexpected entries: {entries:?}");
    }

    #[test]
    fn the_first_line_of_a_message_stops_at_the_newline() {
        assert_eq!(first_line("summary\n\nbody".into()), "summary");
        assert_eq!(first_line("summary".into()), "summary");
        assert_eq!(first_line("".into()), "");
    }

    #[test]
    fn commit_metadata_is_exposed_without_gix_types() {
        let (_dir, repository, head) = repository_with_one_commit("read-commit-metadata");

        let commits = commits(&repository, CommitScope::Head, 1).expect("commits should be read");
        let commit = commits.first().expect("one commit should be present");

        assert_eq!(commit.id, head);
        assert!(
            head.starts_with(&commit.short_id),
            "short id {:?} should be a prefix of {head:?}",
            commit.short_id
        );
        assert!(
            !commit.short_id.is_empty(),
            "short id should not be empty: {commit:?}"
        );
        assert_eq!(commit.summary, "first commit");
        assert_eq!(commit.author, "fuzgit test");
        assert_eq!(commit.time, COMMIT_DATE_SHORT);
    }

    #[test]
    fn the_current_branch_is_the_one_head_points_at() {
        let (dir, repository, _head) = repository_with_one_commit("read-current-branch");

        assert_eq!(
            current_branch(&repository).expect("HEAD should be readable"),
            Some("main".to_string())
        );

        git_in(dir.path(), &["switch", "--quiet", "--create", "feature"]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");
        assert_eq!(
            current_branch(&repository).expect("HEAD should be readable"),
            Some("feature".to_string())
        );
    }

    #[test]
    fn a_detached_head_has_no_current_branch() {
        let (dir, _repository, head) = repository_with_one_commit("read-current-branch-detached");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        assert_eq!(
            current_branch(&repository).expect("HEAD should be readable"),
            None
        );
    }

    #[test]
    fn remotes_are_listed_by_name() {
        let (dir, _repository, _head) = repository_with_one_commit("read-remotes");
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "upstream",
                "https://example.invalid/up.git",
            ],
        );
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/origin.git",
            ],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let remotes = remotes(&repository).expect("remotes should be read");

        assert_eq!(remotes, ["origin", "upstream"]);
    }

    #[test]
    fn a_repository_without_remotes_lists_none() {
        let (_dir, repository, _head) = repository_with_one_commit("read-remotes-empty");

        let remotes = remotes(&repository).expect("remotes should be read");

        assert!(remotes.is_empty(), "unexpected remotes: {remotes:?}");
    }

    /// 2 つのリモートと、その一方だけに追跡参照を持つテストリポジトリを用意する。
    fn repository_with_two_remotes(label: &str) -> (TempDir, String) {
        let (dir, _repository, head) = repository_with_one_commit(label);
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/origin.git",
            ],
        );
        git_in(
            dir.path(),
            &["remote", "add", "backup", "/srv/git/backup.git"],
        );
        create_remote_branch(dir.path(), "origin/main", &head);
        create_remote_branch(dir.path(), "origin/feature/login", &head);

        (dir, head)
    }

    #[test]
    fn a_remote_url_is_read_from_the_local_configuration() {
        let (dir, _head) = repository_with_two_remotes("read-remote-url");

        let output = capture_git_in(dir.path(), &to_str(&remote_url_args("origin")))
            .expect("the remote url should be readable");

        assert_eq!(
            String::from_utf8_lossy(&output).trim(),
            "https://example.invalid/origin.git"
        );
    }

    #[test]
    fn each_remote_reports_its_own_url() {
        let (dir, _head) = repository_with_two_remotes("read-remote-url-each");

        let output = capture_git_in(dir.path(), &to_str(&remote_url_args("backup")))
            .expect("the remote url should be readable");

        assert_eq!(
            String::from_utf8_lossy(&output).trim(),
            "/srv/git/backup.git"
        );
    }

    #[test]
    fn the_tracking_refs_of_a_remote_are_listed_by_their_short_name() {
        let (dir, _head) = repository_with_two_remotes("read-remote-tracking-refs");

        let output = capture_git_in(dir.path(), &to_str(&remote_tracking_refs_args("origin")))
            .expect("the tracking refs should be readable");

        let text = String::from_utf8_lossy(&output).into_owned();
        let refs: Vec<&str> = text.lines().collect();
        assert_eq!(refs, ["origin/feature/login", "origin/main"]);
    }

    #[test]
    fn a_remote_without_tracking_refs_lists_none() {
        // まだ一度も fetch していないリモートには追跡参照が無い（ネットワークは使わない）
        let (dir, _head) = repository_with_two_remotes("read-remote-tracking-refs-empty");

        let output = capture_git_in(dir.path(), &to_str(&remote_tracking_refs_args("backup")))
            .expect("the tracking refs should be readable");

        assert!(output.is_empty(), "unexpected output: {output:?}");
    }

    /// 追跡状況の各行を取り出す。
    fn tracking_lines(workdir: &Path) -> Vec<String> {
        let output = capture_git_in(workdir, &to_str(&branch_tracking_args()))
            .expect("the branch tracking state should be readable");

        String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `origin` を登録済みの、1 コミットだけ持つテストリポジトリを用意する。
    ///
    /// `git branch --set-upstream-to` は参照の存在だけでなく `remote.<name>` の設定も見るため、
    /// 追跡参照を作る前にリモートを登録しておく。
    fn repository_with_origin(label: &str) -> (TempDir, String) {
        let (dir, _repository, head) = repository_with_one_commit(label);
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/origin.git",
            ],
        );

        (dir, head)
    }

    #[test]
    fn every_local_branch_is_listed_with_its_upstream_and_its_distance() {
        let (dir, head) = repository_with_origin("read-branch-tracking");
        git_in(dir.path(), &["branch", "behind"]);
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        // upstream 側だけが 1 件進んでいる状態を作る（fetch 済みの追跡参照との比較）
        let ahead = commit(dir.path(), "second commit");
        create_remote_branch(dir.path(), "origin/behind", &ahead);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/behind", "behind"],
        );

        assert_eq!(
            tracking_lines(dir.path()),
            [
                "  behind → origin/behind [behind 1]",
                "* main → origin/main [ahead 1]",
            ]
        );
    }

    #[test]
    fn a_branch_without_an_upstream_is_listed_by_name_alone() {
        // upstream が無いブランチで空の欄（矢印だけの行）を出さない
        let (dir, _repository, _head) = repository_with_one_commit("read-branch-tracking-no-up");

        assert_eq!(tracking_lines(dir.path()), ["* main"]);
    }

    #[test]
    fn a_branch_level_with_its_upstream_shows_no_distance() {
        let (dir, head) = repository_with_origin("read-branch-tracking-level");
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );

        assert_eq!(tracking_lines(dir.path()), ["* main → origin/main"]);
    }

    #[test]
    fn a_branch_whose_upstream_disappeared_is_marked_as_gone() {
        // 追跡参照が消えたことを黙って隠さない（git の `[gone]` をそのまま出す）
        let (dir, head) = repository_with_origin("read-branch-tracking-gone");
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        git_in(
            dir.path(),
            &["update-ref", "-d", "refs/remotes/origin/main"],
        );

        assert_eq!(tracking_lines(dir.path()), ["* main → origin/main [gone]"]);
    }

    #[test]
    fn the_branch_tracking_state_is_read_without_walking_the_work_tree() {
        // `git status` は追跡状況を出す前に index を refresh する（全ファイルを stat し、
        // 食い違えば内容を読み直す）ため、大きな作業ツリーでは秒単位を要する。
        // プレビューはカーソル移動のたびに同期実行されるので、走査を伴う経路を使わない
        let arguments = branch_tracking_args();

        assert_eq!(arguments[0], "for-each-ref");
        assert!(
            !arguments.iter().any(|argument| argument == "status"),
            "the work tree must not be walked: {arguments:?}"
        );
    }

    #[test]
    fn a_large_work_tree_does_not_slow_the_branch_tracking_state_down() {
        // 作業ツリーの規模に依存しないことを、追跡参照だけを増やしても変わらない出力で示す
        let (dir, head) = repository_with_origin("read-branch-tracking-large");
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        for index in 0..200 {
            write_file(dir.path(), &format!("file{index}.txt"), "contents\n");
        }

        // 未追跡ファイルが 200 件あっても、走査しないため出力は変わらない
        assert_eq!(tracking_lines(dir.path()), ["* main → origin/main"]);
    }

    #[test]
    fn the_remote_preview_arguments_never_reach_the_network() {
        // ネットワークを伴う git のサブコマンド（fetch / ls-remote 等）を含まないことを構造的に確かめる
        for arguments in [
            remote_url_args("origin"),
            remote_tracking_refs_args("origin"),
            branch_tracking_args(),
        ] {
            assert!(
                matches!(arguments[0].as_str(), "remote" | "for-each-ref"),
                "unexpected subcommand: {arguments:?}"
            );
            assert!(
                !arguments.iter().any(|argument| argument.contains("fetch")),
                "a preview must not fetch: {arguments:?}"
            );
        }
    }

    #[test]
    fn tracking_refs_are_addressed_by_the_full_reference_prefix() {
        assert_eq!(
            remote_tracking_refs_args("origin"),
            ["for-each-ref", "refs/remotes/origin", SHORT_REF_FORMAT]
        );
    }

    #[test]
    fn an_upstream_is_read_from_the_branch_configuration() {
        let (dir, _repository, head) = repository_with_one_commit("read-upstream");
        git_in(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/origin.git",
            ],
        );
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let upstream = upstream(&repository, "main")
            .expect("the upstream should be readable")
            .expect("main tracks origin/main");

        assert_eq!(upstream.remote, "origin");
        assert_eq!(upstream.merge_ref, "refs/heads/main");
    }

    #[test]
    fn a_branch_without_tracking_configuration_has_no_upstream() {
        let (dir, repository, _head) = repository_with_one_commit("read-upstream-none");
        create_branch(dir.path(), "feature");

        assert_eq!(
            upstream(&repository, "main").expect("the upstream should be readable"),
            None
        );
        assert_eq!(
            upstream(&repository, "feature").expect("the upstream should be readable"),
            None
        );
    }

    #[test]
    fn an_upstream_is_compared_against_its_remote_tracking_ref() {
        // 比較はネットワークを使わずローカルの追跡参照で行う
        let upstream = Upstream {
            remote: "origin".to_owned(),
            merge_ref: "refs/heads/main".to_owned(),
        };

        assert_eq!(
            upstream.tracking_ref().as_deref(),
            Some("refs/remotes/origin/main")
        );
    }

    #[test]
    fn a_hierarchical_upstream_keeps_its_whole_branch_name() {
        let upstream = Upstream {
            remote: "origin".to_owned(),
            merge_ref: "refs/heads/feature/login".to_owned(),
        };

        assert_eq!(
            upstream.tracking_ref().as_deref(),
            Some("refs/remotes/origin/feature/login")
        );
    }

    #[test]
    fn an_upstream_outside_of_the_branch_namespace_has_no_tracking_ref() {
        let upstream = Upstream {
            remote: "origin".to_owned(),
            merge_ref: "refs/tags/v1.0".to_owned(),
        };

        assert_eq!(
            upstream.tracking_ref(),
            None,
            "a tracking ref must not be guessed"
        );
    }

    #[test]
    fn ahead_and_behind_are_counted_against_the_tracking_reference() {
        let (dir, _repository, _head) = repository_with_one_commit("read-ahead-behind");
        create_branch(dir.path(), "remote-side");
        commit(dir.path(), "local 1");
        commit(dir.path(), "local 2");

        git_in(dir.path(), &["switch", "--quiet", "remote-side"]);
        let remote_head = commit(dir.path(), "remote 1");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        create_remote_branch(dir.path(), "origin/main", &remote_head);

        let counts = ahead_behind(dir.path(), "main", "origin/main")
            .expect("the counts should be computed")
            .expect("origin/main exists");

        assert_eq!(counts, (2, 1), "the left side of the range is ahead");
    }

    #[test]
    fn a_branch_in_sync_with_its_tracking_reference_is_neither_ahead_nor_behind() {
        let (dir, _repository, head) = repository_with_one_commit("read-ahead-behind-sync");
        create_remote_branch(dir.path(), "origin/main", &head);

        let counts = ahead_behind(dir.path(), "main", "origin/main")
            .expect("the counts should be computed")
            .expect("origin/main exists");

        assert_eq!(counts, (0, 0));
    }

    #[test]
    fn a_missing_tracking_reference_yields_no_counts() {
        let (dir, _repository, _head) = repository_with_one_commit("read-ahead-behind-missing");

        let counts =
            ahead_behind(dir.path(), "main", "origin/main").expect("a missing ref is not an error");

        assert_eq!(counts, None);
    }

    #[test]
    fn ahead_behind_output_is_tab_separated() {
        // `git rev-list --left-right --count <local>...<remote>` は `<ahead>\t<behind>\n` を出す
        assert_eq!(
            parse_ahead_behind(b"3\t2\n").expect("the output should parse"),
            (3, 2)
        );
    }

    #[test]
    fn ahead_behind_output_parses_without_a_trailing_newline() {
        assert_eq!(
            parse_ahead_behind(b"0\t0").expect("the output should parse"),
            (0, 0)
        );
    }

    #[test]
    fn a_malformed_ahead_behind_output_is_rejected() {
        for output in [
            &b""[..],
            &b"3"[..],
            &b"3 2\n"[..],
            &b"3\t2\t1\n"[..],
            &b"3\t-2\n"[..],
            &b"a\tb\n"[..],
            &[0xff, b'\t', b'1'],
        ] {
            let err = parse_ahead_behind(output)
                .expect_err("a malformed output must not be silently accepted");

            assert!(
                matches!(
                    err,
                    Error::GitOutputMalformed {
                        detail: MalformedOutput::AheadBehind { .. },
                        ..
                    }
                ),
                "unexpected error for {output:?}: {err:?}"
            );
        }
    }

    #[test]
    fn the_current_branch_is_not_offered_as_a_merge_or_rebase_target() {
        let (dir, repository, head) = repository_with_one_commit("read-other-branches");
        create_branch(dir.path(), "feature");
        create_remote_branch(dir.path(), "origin/main", &head);

        let candidates = other_branches(&repository).expect("branches should be listed");

        assert_eq!(
            names(&candidates),
            ["feature", "origin/main"],
            "only the current branch is removed from the `--all` listing"
        );
    }

    #[test]
    fn a_detached_head_keeps_every_branch_as_a_target() {
        let (dir, _repository, head) = repository_with_one_commit("read-other-branches-detached");
        create_branch(dir.path(), "feature");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("repository should open");

        let candidates = other_branches(&repository).expect("branches should be listed");

        // detached HEAD ではどのブランチも「現在のブランチ」ではないため除外されない
        assert_eq!(names(&candidates), ["feature", "main"]);
    }

    #[test]
    fn the_commits_of_a_range_are_counted() {
        let (dir, _repository, _head) = repository_with_one_commit("read-commit-count");
        create_branch(dir.path(), "feature");
        git_in(dir.path(), &["switch", "--quiet", "feature"]);
        commit(dir.path(), "feature 1");
        commit(dir.path(), "feature 2");
        git_in(dir.path(), &["switch", "--quiet", "main"]);

        assert_eq!(
            commit_count(dir.path(), "HEAD..feature").expect("the range should be counted"),
            2,
            "the commits that would be merged are counted"
        );
        assert_eq!(
            commit_count(dir.path(), "feature..HEAD").expect("the range should be counted"),
            0,
            "the current branch has nothing the other branch lacks"
        );
    }

    #[test]
    fn a_commit_count_is_a_single_number() {
        assert_eq!(
            parse_commit_count(b"3\n").expect("the output should parse"),
            3
        );
        assert_eq!(
            parse_commit_count(b"0").expect("the output should parse"),
            0
        );
    }

    #[test]
    fn a_malformed_commit_count_is_rejected() {
        for output in [
            &b""[..],
            &b"-1\n"[..],
            &b"3 4\n"[..],
            &b"many\n"[..],
            &[0xff],
        ] {
            let err = parse_commit_count(output)
                .expect_err("a malformed output must not be silently accepted");

            assert!(
                matches!(
                    err,
                    Error::GitOutputMalformed {
                        detail: MalformedOutput::CommitCount { .. },
                        ..
                    }
                ),
                "unexpected error for {output:?}: {err:?}"
            );
        }
    }

    /// テストリポジトリにリモートを登録する（fetch は行わない）。
    fn add_remote(path: &Path, name: &str) {
        git_in(
            path,
            &[
                "remote",
                "add",
                name,
                &format!("https://example.invalid/{name}.git"),
            ],
        );
    }

    #[test]
    fn a_push_target_is_generated_for_every_remote() {
        let (dir, _repository, _head) = repository_with_one_commit("read-push-targets");
        add_remote(dir.path(), "upstream");
        add_remote(dir.path(), "origin");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(PushTarget::tracking_name)
                .collect::<Vec<_>>(),
            ["origin/main", "upstream/main"],
            "every remote is combined with the current branch, in name order"
        );
        assert!(
            targets.iter().all(|target| target.branch == "main"),
            "the current branch is fixed as the push target: {targets:?}"
        );
    }

    #[test]
    fn a_repository_without_remotes_has_no_push_target() {
        let (_dir, repository, _head) = repository_with_one_commit("read-push-targets-empty");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert!(targets.is_empty(), "unexpected targets: {targets:?}");
    }

    #[test]
    fn only_the_configured_upstream_is_marked_among_the_targets() {
        let (dir, _repository, head) = repository_with_one_commit("read-push-targets-upstream");
        add_remote(dir.path(), "origin");
        add_remote(dir.path(), "backup");
        create_remote_branch(dir.path(), "origin/main", &head);
        git_in(
            dir.path(),
            &["branch", "--set-upstream-to=origin/main", "main"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.tracking_name(), target.is_upstream))
                .collect::<Vec<_>>(),
            [
                ("backup/main".to_owned(), false),
                ("origin/main".to_owned(), true)
            ]
        );
    }

    #[test]
    fn a_push_target_carries_the_counts_against_its_tracking_reference() {
        let (dir, _repository, _head) = repository_with_one_commit("read-push-targets-counts");
        add_remote(dir.path(), "origin");
        add_remote(dir.path(), "backup");
        create_branch(dir.path(), "remote-side");
        commit(dir.path(), "local 1");
        commit(dir.path(), "local 2");
        git_in(dir.path(), &["switch", "--quiet", "remote-side"]);
        let remote_head = commit(dir.path(), "remote 1");
        git_in(dir.path(), &["switch", "--quiet", "main"]);
        create_remote_branch(dir.path(), "origin/main", &remote_head);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.tracking_name(), target.ahead_behind))
                .collect::<Vec<_>>(),
            [
                ("backup/main".to_owned(), None),
                ("origin/main".to_owned(), Some((2, 1)))
            ],
            "a remote without a tracking reference cannot be compared"
        );
    }

    #[test]
    fn a_push_target_of_a_branch_without_commits_has_no_counts() {
        let dir = TempDir::new("read-push-targets-unborn");
        init_repository(dir.path());
        add_remote(dir.path(), "origin");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let targets = push_targets(&repository).expect("push targets should be generated");

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.tracking_name(), target.ahead_behind))
                .collect::<Vec<_>>(),
            [("origin/main".to_owned(), None)]
        );
    }

    #[test]
    fn a_detached_head_has_no_push_target() {
        let (dir, _repository, head) = repository_with_one_commit("read-push-targets-detached");
        add_remote(dir.path(), "origin");
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let err = push_targets(&repository)
            .expect_err("a detached HEAD gives no branch to push and must be reported");

        assert!(matches!(err, Error::DetachedHead), "unexpected: {err:?}");
    }

    #[test]
    fn a_tracking_reference_is_addressed_by_its_full_name() {
        let target = PushTarget {
            remote: "origin".to_owned(),
            branch: "feature/login".to_owned(),
            is_upstream: false,
            ahead_behind: None,
        };

        assert_eq!(target.tracking_name(), "origin/feature/login");
        assert_eq!(
            target.tracking_ref(),
            "refs/remotes/origin/feature/login",
            "the full name keeps a same-named local branch or tag from winning"
        );
    }

    #[test]
    fn a_target_matches_the_upstream_only_when_both_the_remote_and_the_reference_agree() {
        let upstream = Upstream {
            remote: "origin".to_owned(),
            merge_ref: "refs/heads/main".to_owned(),
        };

        assert!(targets_upstream(Some(&upstream), "origin", "main"));
        assert!(
            !targets_upstream(Some(&upstream), "backup", "main"),
            "another remote is a different destination"
        );
        assert!(
            !targets_upstream(Some(&upstream), "origin", "trunk"),
            "a branch tracking a differently named reference is a different destination"
        );
        assert!(!targets_upstream(None, "origin", "main"));
    }

    /// `origin` を登録済みで、`main` に 1 コミットを持つ `gz pull` 用のリポジトリを用意する。
    fn repository_for_pull(label: &str) -> (TempDir, String) {
        let (dir, _repository, head) = repository_with_one_commit(label);
        add_remote(dir.path(), "origin");

        (dir, head)
    }

    /// `origin/<branch>` の追跡参照を作り、`<branch>` の upstream として設定する。
    ///
    /// ネットワークは使わず、fetch 済みの追跡参照がある状態を直接作る。
    fn set_upstream(path: &Path, branch: &str, head: &str) {
        create_remote_branch(path, &format!("origin/{branch}"), head);
        git_in(
            path,
            &[
                "branch",
                &format!("--set-upstream-to=origin/{branch}"),
                branch,
            ],
        );
    }

    /// 取り込み対象のブランチ名を候補順に取り出す。
    fn target_branches(scan: &PullScan) -> Vec<&str> {
        scan.targets
            .iter()
            .map(|target| target.branch.as_str())
            .collect()
    }

    #[test]
    fn a_pull_target_carries_the_upstream_of_its_branch() {
        let (dir, head) = repository_for_pull("read-pull-targets");
        set_upstream(dir.path(), "main", &head);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("pull targets should be generated");

        assert_eq!(
            scan.targets,
            [PullTarget {
                branch: "main".to_owned(),
                remote: "origin".to_owned(),
                tracking_ref: "refs/remotes/origin/main".to_owned(),
                is_current: true,
            }]
        );
        assert_eq!(scan.excluded, 0);
    }

    #[test]
    fn a_branch_without_an_upstream_is_excluded_and_counted() {
        let (dir, head) = repository_for_pull("read-pull-targets-no-upstream");
        set_upstream(dir.path(), "main", &head);
        create_branch(dir.path(), "solo");
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("pull targets should be generated");

        assert_eq!(
            target_branches(&scan),
            ["main"],
            "a branch without an upstream has no source to pull from"
        );
        assert_eq!(scan.excluded, 1, "the excluded branch must be counted");
    }

    #[test]
    fn a_branch_tracking_an_unregistered_remote_is_excluded_and_counted() {
        // `branch.<name>.remote` には URL を直接書ける。fuzgit が列挙していない対象へ
        // 通信させないため、登録済みのリモート名でないものは候補にしない
        let (dir, head) = repository_for_pull("read-pull-targets-url-remote");
        set_upstream(dir.path(), "main", &head);
        create_branch(dir.path(), "direct");
        git_in(
            dir.path(),
            &[
                "config",
                "branch.direct.remote",
                "https://example.invalid/direct.git",
            ],
        );
        git_in(
            dir.path(),
            &["config", "branch.direct.merge", "refs/heads/direct"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("pull targets should be generated");

        assert_eq!(target_branches(&scan), ["main"]);
        assert_eq!(scan.excluded, 1);
    }

    #[test]
    fn a_branch_checked_out_in_another_worktree_is_excluded_and_counted() {
        // git はチェックアウト中のブランチへの ref 更新を拒否するため、取り込めない
        let (dir, head) = repository_for_pull("read-pull-targets-worktree");
        set_upstream(dir.path(), "main", &head);
        create_branch(dir.path(), "shared");
        set_upstream(dir.path(), "shared", &head);
        git_in(
            dir.path(),
            &["worktree", "add", "--quiet", "linked", "shared"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("pull targets should be generated");

        assert_eq!(target_branches(&scan), ["main"]);
        assert_eq!(scan.excluded, 1);
    }

    #[test]
    fn every_kind_of_exclusion_is_counted_together() {
        let (dir, head) = repository_for_pull("read-pull-targets-excluded-count");
        set_upstream(dir.path(), "main", &head);
        create_branch(dir.path(), "solo");
        create_branch(dir.path(), "direct");
        git_in(
            dir.path(),
            &[
                "config",
                "branch.direct.remote",
                "https://example.invalid/direct.git",
            ],
        );
        git_in(
            dir.path(),
            &["config", "branch.direct.merge", "refs/heads/direct"],
        );
        create_branch(dir.path(), "shared");
        set_upstream(dir.path(), "shared", &head);
        git_in(
            dir.path(),
            &["worktree", "add", "--quiet", "linked", "shared"],
        );
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("pull targets should be generated");

        assert_eq!(target_branches(&scan), ["main"]);
        assert_eq!(
            scan.excluded, 3,
            "the branches without an upstream, with an unregistered remote, \
and checked out elsewhere are all counted"
        );
    }

    #[test]
    fn the_current_branch_comes_first_and_the_rest_follow_by_name() {
        let (dir, head) = repository_for_pull("read-pull-targets-order");
        for branch in ["zebra", "alpha"] {
            create_branch(dir.path(), branch);
            set_upstream(dir.path(), branch, &head);
        }
        set_upstream(dir.path(), "main", &head);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("pull targets should be generated");

        assert_eq!(target_branches(&scan), ["main", "alpha", "zebra"]);
        assert_eq!(
            scan.targets
                .iter()
                .map(|target| target.is_current)
                .collect::<Vec<_>>(),
            [true, false, false]
        );
    }

    #[test]
    fn a_detached_head_still_offers_the_other_branches() {
        // `push_targets` は detached HEAD を `Error::DetachedHead` にするが、`gz pull` は
        // 複数のブランチを対象にする一括処理であり、現在のブランチが無いだけで成立する
        let (dir, head) = repository_for_pull("read-pull-targets-detached");
        set_upstream(dir.path(), "main", &head);
        create_branch(dir.path(), "alpha");
        set_upstream(dir.path(), "alpha", &head);
        git_in(dir.path(), &["switch", "--quiet", "--detach", &head]);
        let repository = discover(dir.path()).expect("test repository should be discoverable");

        let scan = pull_targets(&repository).expect("a detached HEAD must not be an error");

        assert_eq!(target_branches(&scan), ["alpha", "main"]);
        assert!(
            scan.targets.iter().all(|target| !target.is_current),
            "a detached HEAD checks out no branch: {targets:?}",
            targets = scan.targets
        );
        assert_eq!(scan.excluded, 0);
    }

    #[test]
    fn no_operation_is_in_progress_in_a_clean_repository() {
        let (_dir, repository, _head) = repository_with_one_commit("read-state-clean");

        assert_eq!(operation_in_progress(&repository), None);
    }

    #[test]
    fn a_merge_in_progress_is_detected() {
        let (dir, repository, head) = repository_with_one_commit("read-state-merge");
        write_file(dir.path(), ".git/MERGE_HEAD", &format!("{head}\n"));

        assert_eq!(operation_in_progress(&repository), Some(Operation::Merge));
    }

    #[test]
    fn a_rebase_in_progress_is_detected() {
        let (dir, repository, _head) = repository_with_one_commit("read-state-rebase");
        // 非対話の `git rebase` でも merge バックエンドは `rebase-merge/interactive` を作る
        write_file(dir.path(), ".git/rebase-merge/interactive", "");

        assert_eq!(operation_in_progress(&repository), Some(Operation::Rebase));
    }

    #[test]
    fn a_rebase_started_by_the_apply_backend_is_detected() {
        let (dir, repository, _head) = repository_with_one_commit("read-state-rebase-apply");
        write_file(dir.path(), ".git/rebase-apply/rebasing", "");

        assert_eq!(operation_in_progress(&repository), Some(Operation::Rebase));
    }

    #[test]
    fn an_operation_without_a_recovery_menu_is_not_reported() {
        // cherry-pick は fuzgit の復帰メニューの対象外（git の hint をそのまま見せる）
        let (dir, repository, head) = repository_with_one_commit("read-state-cherry-pick");
        write_file(dir.path(), ".git/CHERRY_PICK_HEAD", &format!("{head}\n"));

        assert_eq!(operation_in_progress(&repository), None);
    }

    #[test]
    fn unmerged_status_codes_are_recognised() {
        // git がマージ未解決として扱う 7 種類の状態コード
        for code in ["DD", "AU", "UD", "UA", "DU", "AA", "UU"] {
            assert!(
                change("conflicted.txt", code).is_unmerged(),
                "{code} must be treated as unmerged"
            );
        }
    }

    #[test]
    fn ordinary_status_codes_are_not_unmerged() {
        for code in [
            "M ", " M", "MM", "A ", " D", "D ", "??", "R ", "C ", "AM", "AD",
        ] {
            assert!(
                !change("plain.txt", code).is_unmerged(),
                "{code} must not be treated as unmerged"
            );
        }
    }

    #[test]
    fn unmerged_entries_are_extracted_from_a_conflicted_status() {
        let output = status_output(&[
            "UU both modified.txt",
            "AA both added.txt",
            "DD both deleted.txt",
            "M  staged.txt",
            "?? new.txt",
        ]);

        let changes = parse_status(&output).expect("status output should parse");
        let unmerged: Vec<&str> = changes
            .iter()
            .filter(|change| ChangeScope::Unmerged.includes(change))
            .map(|change| change.path.as_str())
            .collect();

        assert_eq!(
            unmerged,
            ["both modified.txt", "both added.txt", "both deleted.txt"]
        );
    }

    #[test]
    fn a_real_merge_conflict_is_reported_as_an_unmerged_change() {
        let dir = TempDir::new("read-unmerged-real");
        init_repository(dir.path());
        write_file(dir.path(), "shared.txt", "base\n");
        git_in(dir.path(), &["add", "--", "shared.txt"]);
        git_in(dir.path(), &["commit", "--quiet", "-m", "base"]);

        create_branch(dir.path(), "other");
        write_file(dir.path(), "shared.txt", "main side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "main side"]);

        git_in(dir.path(), &["switch", "--quiet", "other"]);
        write_file(dir.path(), "shared.txt", "other side\n");
        git_in(dir.path(), &["commit", "--quiet", "-a", "-m", "other side"]);

        assert!(
            !try_git_in(dir.path(), &["merge", "--no-edit", "main"]),
            "the merge is expected to conflict"
        );

        let repository = discover(dir.path()).expect("test repository should be discoverable");
        assert_eq!(operation_in_progress(&repository), Some(Operation::Merge));

        let unmerged =
            changes(&repository, ChangeScope::Unmerged).expect("the status should be read");

        assert_eq!(
            unmerged
                .iter()
                .map(|change| (change.path.as_str(), change.status_code()))
                .collect::<Vec<_>>(),
            [("shared.txt", "UU".to_string())]
        );
    }

    /// `main`（1 コミット）に、merged なブランチと unmerged なブランチを 1 つずつ持つ
    /// テストリポジトリを用意する。HEAD は `main` に戻した状態で返す。
    fn repository_with_merged_and_unmerged_branches(label: &str) -> TempDir {
        let dir = TempDir::new(label);
        init_repository(dir.path());
        commit(dir.path(), "first commit");

        // HEAD と同じコミットを指すブランチは merged
        create_branch(dir.path(), "merged");

        git_in(dir.path(), &["switch", "--quiet", "--create", "unmerged"]);
        commit(dir.path(), "second commit");
        git_in(dir.path(), &["switch", "--quiet", "main"]);

        dir
    }

    #[test]
    fn merged_branches_are_listed_relative_to_head() {
        let dir = repository_with_merged_and_unmerged_branches("read-merged-branches");

        let merged = merged_branches(dir.path(), "HEAD").expect("merged branches should be read");

        let mut names: Vec<&str> = merged.iter().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["main", "merged"]);
    }

    #[test]
    fn merged_branches_follow_the_given_base() {
        let dir = repository_with_merged_and_unmerged_branches("read-merged-branches-base");

        let merged =
            merged_branches(dir.path(), "unmerged").expect("merged branches should be read");

        let mut names: Vec<&str> = merged.iter().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["main", "merged", "unmerged"],
            "基準を変えると merged 判定も変わる"
        );
    }

    #[test]
    fn an_empty_merged_branch_list_yields_no_names() {
        let merged = parse_merged_branches(b"").expect("empty output should parse");

        assert!(merged.is_empty(), "unexpected names: {merged:?}");
    }

    #[test]
    fn merged_branch_names_are_read_line_by_line() {
        let merged =
            parse_merged_branches(b"main\nfeature/login\n").expect("branch names should parse");

        let mut names: Vec<&str> = merged.iter().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["feature/login", "main"]);
    }

    #[test]
    fn a_non_utf8_merged_branch_list_is_rejected() {
        let err = parse_merged_branches(&[0xff, b'\n'])
            .expect_err("a non utf-8 branch name must not be accepted");

        assert!(matches!(err, Error::RepositoryReadFailed { .. }));
    }

    #[test]
    fn branch_activity_reports_a_relative_date_for_every_local_branch() {
        let dir = repository_with_merged_and_unmerged_branches("read-branch-activity");

        let activity = branch_activity(dir.path()).expect("branch activity should be read");

        let mut names: Vec<&str> = activity.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["main", "merged", "unmerged"]);
        for (name, relative_date) in &activity {
            // 文言は git に委ねるため、相対表記であることだけを確かめる
            assert!(
                relative_date.ends_with("ago"),
                "unexpected relative date for {name}: {relative_date}"
            );
        }
    }

    /// `git for-each-ref --format=%(refname:short)%00%(committerdate:relative)` の出力を組み立てる。
    fn activity_output(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut output = Vec::new();
        for (name, relative_date) in entries {
            output.extend_from_slice(name.as_bytes());
            output.push(0);
            output.extend_from_slice(relative_date.as_bytes());
            output.push(b'\n');
        }
        output
    }

    #[test]
    fn an_empty_branch_activity_output_yields_no_entries() {
        let activity = parse_branch_activity(b"").expect("empty output should parse");

        assert!(activity.is_empty(), "unexpected entries: {activity:?}");
    }

    #[test]
    fn branch_activity_keeps_the_relative_date_verbatim() {
        let output = activity_output(&[
            ("main", "3 days ago"),
            ("feature/login", "12 minutes ago"),
            ("old", "2 years, 4 months ago"),
        ]);

        let activity = parse_branch_activity(&output).expect("branch activity should parse");

        assert_eq!(activity.len(), 3);
        assert_eq!(activity.get("main").map(String::as_str), Some("3 days ago"));
        assert_eq!(
            activity.get("feature/login").map(String::as_str),
            Some("12 minutes ago")
        );
        assert_eq!(
            activity.get("old").map(String::as_str),
            Some("2 years, 4 months ago"),
            "空白やカンマを含む表記もそのまま保持する"
        );
    }

    #[test]
    fn a_branch_activity_line_without_a_separator_is_rejected() {
        let err = parse_branch_activity(b"main 3 days ago\n")
            .expect_err("a missing NUL separator must be rejected");

        assert!(matches!(
            err,
            Error::GitOutputMalformed {
                detail: MalformedOutput::BranchActivityPair { .. },
                ..
            }
        ));
    }

    /// `git worktree list --porcelain -z` の 1 レコード分の出力を組み立てる。
    ///
    /// 実際の git は各属性行を NUL で終端し、レコードの末尾に空行（＝もう 1 つの NUL）を置く。
    fn worktree_record(lines: &[&str]) -> Vec<u8> {
        let mut record = Vec::new();
        for line in lines {
            record.extend_from_slice(line.as_bytes());
            record.push(0);
        }
        record.push(0);
        record
    }

    /// 複数レコード分の `git worktree list --porcelain -z` の出力を組み立てる。
    fn worktree_output(records: &[&[&str]]) -> Vec<u8> {
        records
            .iter()
            .flat_map(|lines| worktree_record(lines))
            .collect()
    }

    #[test]
    fn an_empty_worktree_list_yields_no_entries() {
        let worktrees = parse_worktree_list(b"").expect("empty output should parse");

        assert!(worktrees.is_empty(), "unexpected entries: {worktrees:?}");
    }

    #[test]
    fn the_main_worktree_comes_first_and_linked_ones_follow() {
        let output = worktree_output(&[
            &[
                "worktree /repo",
                "HEAD 1111111111111111111111111111111111111111",
                "branch refs/heads/main",
            ],
            &[
                "worktree /repo/../feature",
                "HEAD 2222222222222222222222222222222222222222",
                "branch refs/heads/feature/login",
            ],
        ]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(
            worktrees,
            [
                WorktreeInfo {
                    path: "/repo".to_owned(),
                    head: Some("1111111111111111111111111111111111111111".to_owned()),
                    branch: Some("main".to_owned()),
                    is_main: true,
                    is_locked: false,
                    prunable: false,
                },
                WorktreeInfo {
                    path: "/repo/../feature".to_owned(),
                    head: Some("2222222222222222222222222222222222222222".to_owned()),
                    branch: Some("feature/login".to_owned()),
                    is_main: false,
                    is_locked: false,
                    prunable: false,
                }
            ]
        );
    }

    #[test]
    fn a_detached_worktree_has_a_head_but_no_branch() {
        let output = worktree_output(&[&[
            "worktree /repo/detached",
            "HEAD 3333333333333333333333333333333333333333",
            "detached",
        ]]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].branch, None);
        assert_eq!(
            worktrees[0].head.as_deref(),
            Some("3333333333333333333333333333333333333333")
        );
    }

    #[test]
    fn a_locked_worktree_is_detected_with_and_without_a_reason() {
        let output = worktree_output(&[
            &[
                "worktree /repo",
                "HEAD 1111111111111111111111111111111111111111",
                "branch refs/heads/main",
            ],
            &[
                "worktree /repo/with reason",
                "HEAD 2222222222222222222222222222222222222222",
                "branch refs/heads/feature",
                // 理由は空白を含み得る（ラベルの後ろがすべて理由）
                "locked 作業中 のため 触らないこと",
            ],
            &[
                "worktree /repo/without reason",
                "HEAD 3333333333333333333333333333333333333333",
                "detached",
                "locked",
            ],
        ]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(
            worktrees
                .iter()
                .map(|worktree| worktree.is_locked)
                .collect::<Vec<bool>>(),
            [false, true, true]
        );
    }

    #[test]
    fn a_prunable_worktree_is_detected_with_and_without_a_reason() {
        let output = worktree_output(&[
            &[
                "worktree /repo",
                "HEAD 1111111111111111111111111111111111111111",
                "branch refs/heads/main",
            ],
            &[
                "worktree /repo/gone",
                "HEAD 2222222222222222222222222222222222222222",
                "detached",
                "prunable gitdir file points to non-existent location",
            ],
            &[
                // 理由が付かない形（ラベルのみの行）でも整理対象として扱う
                "worktree /repo/also gone",
                "HEAD 3333333333333333333333333333333333333333",
                "detached",
                "prunable",
            ],
        ]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(
            worktrees
                .iter()
                .map(|worktree| worktree.prunable)
                .collect::<Vec<bool>>(),
            [false, true, true]
        );
    }

    #[test]
    fn a_worktree_can_be_locked_and_prunable_at_the_same_time() {
        let output = worktree_output(&[&[
            "worktree /repo/gone",
            "HEAD 1111111111111111111111111111111111111111",
            "branch refs/heads/feature",
            "locked 移動予定",
            "prunable gitdir file points to non-existent location",
        ]]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert!(worktrees[0].is_locked);
        assert!(worktrees[0].prunable);
    }

    #[test]
    fn several_linked_worktrees_keep_their_own_state() {
        // main / 通常の linked / detached / locked かつ prunable が混在する実運用に近い形
        let output = worktree_output(&[
            &[
                "worktree /repo",
                "HEAD 1111111111111111111111111111111111111111",
                "branch refs/heads/main",
            ],
            &[
                "worktree /repo/../feature",
                "HEAD 2222222222222222222222222222222222222222",
                "branch refs/heads/feature/login",
            ],
            &[
                "worktree /repo/../review",
                "HEAD 3333333333333333333333333333333333333333",
                "detached",
            ],
            &[
                "worktree /repo/../gone",
                "HEAD 4444444444444444444444444444444444444444",
                "branch refs/heads/wip",
                "locked",
                "prunable gitdir file points to non-existent location",
            ],
        ]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(
            worktrees
                .iter()
                .map(|worktree| (
                    worktree.path.as_str(),
                    worktree.branch.as_deref(),
                    worktree.is_main,
                    worktree.is_locked,
                    worktree.prunable
                ))
                .collect::<Vec<_>>(),
            [
                ("/repo", Some("main"), true, false, false),
                (
                    "/repo/../feature",
                    Some("feature/login"),
                    false,
                    false,
                    false
                ),
                ("/repo/../review", None, false, false, false),
                ("/repo/../gone", Some("wip"), false, true, true),
            ],
            "only the first record is the main worktree"
        );
    }

    #[test]
    fn a_bare_repository_can_still_have_linked_worktrees() {
        let output = worktree_output(&[
            &["worktree /repo.git", "bare"],
            &[
                "worktree /repo.git/../feature",
                "HEAD 2222222222222222222222222222222222222222",
                "branch refs/heads/feature",
            ],
        ]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(worktrees.len(), 2);
        // bare な本体には HEAD 属性が無く、先頭であることだけが main の根拠になる
        assert!(worktrees[0].is_main);
        assert_eq!(worktrees[0].head, None);
        assert_eq!(worktrees[0].branch, None);
        assert!(!worktrees[1].is_main);
        assert_eq!(worktrees[1].branch.as_deref(), Some("feature"));
    }

    #[test]
    fn an_attribute_that_should_carry_a_value_is_rejected_without_one() {
        // 値を持つはずの属性が値を欠くのは、出力を読み違えている証拠であるため推測しない
        for lines in [
            vec!["worktree /repo", "HEAD"],
            vec![
                "worktree /repo",
                "HEAD 1111111111111111111111111111111111111111",
                "branch",
            ],
        ] {
            let output = worktree_record(&lines);

            let err = parse_worktree_list(&output)
                .expect_err("an attribute without its value must be rejected");

            assert!(
                matches!(
                    err,
                    Error::GitOutputMalformed {
                        detail: MalformedOutput::WorktreeAttributeValueMissing { .. },
                        ..
                    }
                ),
                "unexpected error for {lines:?}: {err:?}"
            );
        }
    }

    #[test]
    fn a_bare_worktree_has_neither_head_nor_branch() {
        // bare なリポジトリの main worktree には HEAD 属性そのものが無い
        let output = worktree_output(&[&["worktree /repo.git", "bare"]]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(
            worktrees,
            [WorktreeInfo {
                path: "/repo.git".to_owned(),
                head: None,
                branch: None,
                is_main: true,
                is_locked: false,
                prunable: false,
            }]
        );
    }

    #[test]
    fn a_path_containing_spaces_is_kept_intact() {
        let output = worktree_output(&[&[
            "worktree /repo/linked one two",
            "HEAD 1111111111111111111111111111111111111111",
            "branch refs/heads/feature",
        ]]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(worktrees[0].path, "/repo/linked one two");
    }

    #[test]
    fn an_unborn_worktree_keeps_the_null_head_reported_by_git() {
        // まだコミットの無いブランチでは git が全 0 のハッシュを返す
        let output = worktree_output(&[&[
            "worktree /repo",
            "HEAD 0000000000000000000000000000000000000000",
            "branch refs/heads/main",
        ]]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(
            worktrees[0].head.as_deref(),
            Some("0000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn an_unknown_attribute_is_ignored() {
        // 将来の git が属性を増やしても一覧が読めなくならないようにする
        let output = worktree_output(&[&[
            "worktree /repo",
            "HEAD 1111111111111111111111111111111111111111",
            "branch refs/heads/main",
            "brand-new-attribute value",
            "brand-new-flag",
        ]]);

        let worktrees = parse_worktree_list(&output).expect("worktree list should parse");

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn a_record_that_does_not_start_with_the_worktree_attribute_is_rejected() {
        let output = worktree_record(&["HEAD 1111111111111111111111111111111111111111"]);

        let err = parse_worktree_list(&output)
            .expect_err("a record must start with the worktree attribute");

        assert!(matches!(
            err,
            Error::GitOutputMalformed {
                detail: MalformedOutput::WorktreeRecordStart { .. },
                ..
            }
        ));
    }

    #[test]
    fn an_unterminated_record_is_rejected() {
        // 終端の空行が無い出力は途中で切れている。読めた分だけを返さない
        let output = b"worktree /repo\0HEAD 1111111111111111111111111111111111111111\0";

        let err = parse_worktree_list(output).expect_err("an unterminated record must be rejected");

        assert!(matches!(
            err,
            Error::GitOutputMalformed {
                detail: MalformedOutput::WorktreeRecordUnterminated { .. },
                ..
            }
        ));
    }

    #[test]
    fn an_output_cut_in_the_middle_of_a_line_is_rejected() {
        let output = b"worktree /repo\0HEAD 1111111111111111111111111111111111111111";

        let err = parse_worktree_list(output).expect_err("a truncated output must be rejected");

        assert!(matches!(
            err,
            Error::GitOutputMalformed {
                detail: MalformedOutput::WorktreeRecordUnterminated { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_record_without_a_path_is_rejected() {
        let output = worktree_record(&["worktree"]);

        let err = parse_worktree_list(&output).expect_err("the worktree path is required");

        assert!(matches!(
            err,
            Error::GitOutputMalformed {
                detail: MalformedOutput::WorktreeAttributeValueMissing { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_branch_outside_of_refs_heads_is_rejected() {
        let output = worktree_record(&[
            "worktree /repo",
            "HEAD 1111111111111111111111111111111111111111",
            "branch refs/remotes/origin/main",
        ]);

        let err = parse_worktree_list(&output).expect_err("only branches can be checked out");

        assert!(matches!(
            err,
            Error::GitOutputMalformed {
                detail: MalformedOutput::WorktreeBranchReference { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_non_utf8_worktree_path_is_rejected() {
        let mut output = b"worktree /repo/".to_vec();
        output.push(0xff);
        output.extend_from_slice(b"\0\0");

        let err = parse_worktree_list(&output).expect_err("a non utf-8 path must not be accepted");

        assert!(matches!(err, Error::RepositoryReadFailed { .. }));
    }

    #[test]
    fn the_worktrees_of_a_repository_are_read_with_the_main_one_first() {
        let dir = TempDir::new("read-worktrees");
        init_repository(dir.path());
        let head = commit(dir.path(), "first commit");
        create_branch(dir.path(), "feature");
        let linked = dir.path().join("linked worktree");
        git_in(
            dir.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                &linked.to_string_lossy(),
                "feature",
            ],
        );

        let worktrees = worktrees(dir.path()).expect("worktrees should be read");

        assert_eq!(worktrees.len(), 2, "unexpected worktrees: {worktrees:?}");
        assert!(worktrees[0].is_main, "main worktree comes first");
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(worktrees[0].head.as_deref(), Some(head.as_str()));
        assert!(!worktrees[1].is_main);
        assert_eq!(worktrees[1].branch.as_deref(), Some("feature"));
        assert!(!worktrees[1].is_locked);
        assert!(!worktrees[1].prunable);
        assert!(
            worktrees[1].path.ends_with("linked worktree"),
            "unexpected path: {path}",
            path = worktrees[1].path
        );
    }

    /// テスト用の worktree エントリを組み立てる。
    fn worktree(path: &str, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path: path.to_owned(),
            head: Some("1111111111111111111111111111111111111111".to_owned()),
            branch: branch.map(str::to_owned),
            is_main: false,
            is_locked: false,
            prunable: false,
        }
    }

    #[test]
    fn every_branch_checked_out_by_a_worktree_is_collected() {
        let worktrees = [
            worktree("/repo", Some("main")),
            worktree("/repo/../feature", Some("feature/login")),
        ];

        let in_use = checked_out_branches(&worktrees);

        assert_eq!(
            in_use,
            HashSet::from(["main".to_owned(), "feature/login".to_owned()])
        );
    }

    #[test]
    fn a_detached_worktree_holds_no_branch() {
        let worktrees = [worktree("/repo", Some("main")), worktree("/detached", None)];

        let in_use = checked_out_branches(&worktrees);

        assert_eq!(in_use, HashSet::from(["main".to_owned()]));
    }

    #[test]
    fn an_empty_worktree_list_holds_no_branch() {
        assert!(checked_out_branches(&[]).is_empty());
    }

    #[test]
    fn the_branches_in_use_are_read_from_a_real_repository() {
        let dir = TempDir::new("read-checked-out-branches");
        init_repository(dir.path());
        commit(dir.path(), "first commit");
        create_branch(dir.path(), "feature");
        let linked = dir.path().join("linked");
        git_in(
            dir.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                &linked.to_string_lossy(),
                "feature",
            ],
        );

        let worktrees = worktrees(dir.path()).expect("worktrees should be read");
        let in_use = checked_out_branches(&worktrees);

        assert_eq!(
            in_use,
            HashSet::from(["main".to_owned(), "feature".to_owned()]),
            "both the current branch and the linked worktree are in use"
        );
    }
}
