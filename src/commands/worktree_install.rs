//! `gz worktree add` が作った worktree へ依存を入れる（FR-30）
//!
//! `git worktree add` が新しい作業ツリーへ書き出すのは**追跡ファイルだけ**であり、
//! `node_modules/` も `vendor/` も `.venv/` も無い。作った直後にテストを走らせられない
//! のはこのためで、利用者は毎回「このプロジェクトは何を叩けばよいか」を思い出して
//! 手で実行している。この 1 手を fuzgit 自身が引き受ける
//!
//! # git 以外の外部コマンドを起動する 2 例目である
//!
//! 起動を [`crate::git::exec`] へ通さない。`exec` は「git を実行する」ための不変条件
//! （(A)/(B) の分類・ロケールの適用・デバッグログ）を持っており、別のコマンドをそこへ
//! 通すとその不変条件が意味を失う（1 例目である [`crate::notify`] のモジュール
//! doc comment と同じ理由）。共有するのはデバッグログの体裁（`DEBUG_PREFIX` と
//! `debug_enabled()`）だけに留める
//!
//! # ロケールを加工しない「第三の扱い」である
//!
//! FR-26 の (A)（`LC_MESSAGES=C` に固定して出力を解釈する）と (B)（表示言語を伝播して
//! そのままユーザーへ見せる）は**git の実行**の分類であり、インストールコマンドはその
//! どちらでもない。fuzgit はこのコマンドの出力を一切解釈しないため (A) の固定は要らず、
//! `LANGUAGE` を設定しても npm / pnpm / bundler がその規約（GNU gettext）に従う保証は
//! 無いため (B) の伝播も意味を持たない。したがって**ロケール関連を含め子プロセスの
//! 環境変数を一切加工せず**（`env_clear()` も使わず）、親の環境をそのまま継承する
//!
//! この扱いを `exec` の (A)/(B) 表へ混ぜてはならない。混ぜると「[`crate::i18n::Language`]
//! を取る関数が (B)」という型による不変条件が崩れる。ロケールを加工しないこの経路は
//! `Language` を受け取らないことでそれを表している（この module の関数はどれも
//! `Language` を引数に取らない）
//!
//! # 実行するのは固定の検出テーブルだけである
//!
//! プログラム名も引数も [`RECIPES`] に持つ `&'static str` の定数であり、設定・環境変数・
//! リポジトリ内のファイルから受け取った文字列がコマンドラインへ流れる経路は無い
//! （[`Plan`] の全フィールドが `&'static` であることがその証拠になっている）
//! lockfile は「どの定数を実行するか」の分岐条件にしか使わず、内容を読むことも引数へ
//! 入れることも無い（存在の有無だけを見る）
//!
//! なお、これは**信頼できないリポジトリを安全にする仕組みではない**。`npm install` 等は
//! `package.json` の `postinstall` を実行し得るため、インストールは事実上リポジトリ由来の
//! コードの実行になる。ただし対象は既に clone しチェックアウト済みのリポジトリであり、
//! 同じことは利用者が手で `pnpm install` を叩けば起きる（fuzgit が新しい信頼境界を作る
//! わけではない）。そのうえで「知らないリポジトリで worktree を作った瞬間に走る」ことは
//! 事実であるため、(1) 実行前に何を実行するかを必ず表示し、(2) `--no-install` で事前に
//! 抑止できるようにする

use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::Command;

use crate::git::exec::{DEBUG_PREFIX, debug_enabled};
use crate::git::read::tracked_files_named;
use crate::i18n::Messages;

/// 競合した lockfile を列挙するときの区切り。
const LOCKFILE_SEPARATOR: &str = ", ";

/// `--no-install` の指定を型で表したもの。
///
/// `bool` を commands 層へ持ち回さず、走査すら行わないことを型で示す
/// （既存の `PruneMode` / `NotifySetting` と同方針）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// lockfile を走査し、対応するインストールコマンドを実行する（既定）。
    Run,
    /// 走査もインストールも行わない（`--no-install`）。
    Skip,
}

impl InstallMode {
    /// `--no-install` の指定から組み立てる。
    ///
    /// 変換をここに 1 か所だけ置き、`bool` が届く範囲を CLI の境界に閉じ込める。
    #[must_use]
    pub fn from_no_install(no_install: bool) -> Self {
        if no_install {
            InstallMode::Skip
        } else {
            InstallMode::Run
        }
    }
}

/// 依存を解決するエコシステム。
///
/// 同一エコシステム内で lockfile が複数並ぶ場合（Node の 3 種など）は、どちらが実運用の
/// ものか fuzgit には分からないため実行しない。**異なるエコシステムの同居は競合ではない**
/// （Rails + JS など）ため、この型で「競合する範囲」を区切る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    /// Node.js（pnpm / yarn / npm）。
    Node,
    /// Python（uv）。
    Python,
    /// Ruby（bundler）。
    Ruby,
    /// PHP（composer）。
    Php,
}

impl Ecosystem {
    /// 警告に示す呼称。
    ///
    /// エコシステムの名前は固有名詞であるため翻訳しない
    /// （`main` / `linked` を翻訳しない [`crate::commands::worktree`] と同じ扱い）。
    fn label(self) -> &'static str {
        match self {
            Ecosystem::Node => "Node",
            Ecosystem::Python => "Python",
            Ecosystem::Ruby => "Ruby",
            Ecosystem::Php => "PHP",
        }
    }
}

/// 検出テーブルの 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Recipe {
    /// 競合する範囲を決めるエコシステム。
    ecosystem: Ecosystem,
    /// worktree のルート直下に在るかどうかを見るファイル名。
    lockfile: &'static str,
    /// そのエコシステムで人が書く側のファイル名（プレビューに出す）。
    ///
    /// lockfile は依存の解決結果であり、先頭に並ぶのは `integrity` の羅列で
    /// 「これは何のプロジェクトか」が読み取れない。判断の材料になるのは
    /// **名前と直接依存が書かれているこちら**である。
    manifest: &'static str,
    /// 起動するプログラム名。
    program: &'static str,
    /// 起動時に渡す引数。
    arguments: &'static [&'static str],
}

impl Recipe {
    /// この Recipe が実行する 1 手。
    fn plan(&self) -> Plan {
        Plan {
            lockfile: self.lockfile,
            program: self.program,
            arguments: self.arguments,
        }
    }
}

/// 検出テーブル（requirements.md FR-30 の表そのもの）。
///
/// **この配列以外に、実行するコマンドの出所を作らない。**選ぶのは各ツールの
/// **既定のインストールコマンド**（`npm install` / `pnpm install` / `yarn install` /
/// `uv sync` …）であり、lockfile に厳密に従う側の綴り（`npm ci` /
/// `--frozen-lockfile` / `--immutable` / `--frozen`）は使わない。厳密な綴りは
/// マニフェストと lockfile が同期していないリポジトリで**必ず失敗し**、そのリポジトリでは
/// `gz worktree add` のたびに手で入れ直すことになる（利用者からの報告。lockfile の更新が
/// コミットされていない構成は珍しくない）。既定の綴りなら同期済みのときは同じ結果になり、
/// 未同期のときは lockfile を更新して入れる（worktree が dirty になるが、その差分は
/// 本来コミットされるべき lockfile の更新そのものである）。
///
/// 既定の綴りに揃えたことで Yarn 1 / Yarn 2 以降の区別（`--frozen-lockfile` と
/// `--immutable`）が要らなくなり、`yarn.lock` のヘッダを読む必要も無い。
///
/// `Cargo.lock` / `go.sum` は**意図的に含めない**。`cargo fetch` / `go mod download` は
/// グローバルキャッシュ（`CARGO_HOME` / `GOMODCACHE`）を温めるだけでプロジェクト直下に
/// 実体を作らず、Cargo と Go はビルド時に依存を自動取得するため、新しい worktree は
/// インストールを挟まずにそのままビルド・テストできる（requirements.md「スコープ外
/// （FR-30 の検討で明示的に除外した項目）」）。
///
/// 順序は実行順でもある。異なるエコシステムが同居する場合はこの順に直列実行する。
const RECIPES: [Recipe; 6] = [
    Recipe {
        ecosystem: Ecosystem::Node,
        lockfile: "pnpm-lock.yaml",
        manifest: "package.json",
        program: "pnpm",
        arguments: &["install"],
    },
    Recipe {
        ecosystem: Ecosystem::Node,
        lockfile: "yarn.lock",
        manifest: "package.json",
        program: "yarn",
        arguments: &["install"],
    },
    Recipe {
        ecosystem: Ecosystem::Node,
        lockfile: "package-lock.json",
        manifest: "package.json",
        program: "npm",
        arguments: &["install"],
    },
    Recipe {
        ecosystem: Ecosystem::Python,
        lockfile: "uv.lock",
        manifest: "pyproject.toml",
        program: "uv",
        arguments: &["sync"],
    },
    Recipe {
        ecosystem: Ecosystem::Ruby,
        lockfile: "Gemfile.lock",
        manifest: "Gemfile",
        program: "bundle",
        arguments: &["install"],
    },
    Recipe {
        ecosystem: Ecosystem::Php,
        lockfile: "composer.lock",
        manifest: "composer.json",
        program: "composer",
        arguments: &["install"],
    },
];

/// 実行する 1 つのインストールコマンド
///
/// 全フィールドが `&'static` であることが「ユーザー由来の文字列がコマンドラインへ
/// 流れない」という不変条件そのものである（module の doc comment を参照）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// 実行する根拠になった lockfile 名。
    lockfile: &'static str,
    /// 起動するプログラム名。
    program: &'static str,
    /// 起動時に渡す引数。
    arguments: &'static [&'static str],
}

impl Plan {
    /// 表示用のコマンド文字列を組み立てる
    ///
    /// **あくまで表示専用**であり、この文字列をコマンドとして実行することはない
    /// （`exec::display_args` と同じ注記）。実行は常に引数配列で行う
    pub fn command_line(&self) -> String {
        if self.arguments.is_empty() {
            return self.program.to_owned();
        }

        format!(
            "{program} {arguments}",
            program = self.program,
            arguments = self.arguments.join(" ")
        )
    }
}

/// 検出の結果として取る 1 手
///
/// 「実行しない」も結果として持つのは、黙って何もしないことを避けるためである
/// （依存が入らなかったことは利用者が知るべき結果である）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// インストールコマンドを実行する
    Run(Plan),
    /// 同一エコシステムに複数の lockfile が並び、どれが実運用のものか決められない
    Ambiguous {
        /// 競合したエコシステム。
        ecosystem: Ecosystem,
        /// 検出した lockfile 名（検出テーブル順）
        lockfiles: Vec<&'static str>,
    },
}

/// インストールコマンドの実行結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 正常終了した。
    Completed,
    /// プログラムが PATH に無く、起動できなかった
    Missing,
    /// 起動はできたが失敗した（非ゼロ終了）
    Failed,
}

/// worktree のルート直下に在る lockfile を検出テーブル順で集める（薄い IO 層）
///
/// 走査するのは**ルート直下 1 階層のみ**で、再帰もグロブも行わない
/// （探索範囲を親ディレクトリ直下 1 階層に限る `gz fetch --siblings` と同型の判断）
/// 実体は検出テーブルの件数（6 回）の `is_file()` であり、判断そのものは
/// 純関数 [`plan`] が持つ
#[must_use]
/// 依存インストールの対象になり得るディレクトリ 1 件
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// worktree のルートから見た相対パス（ルート自身は空文字）
    pub relative: String,
    /// そのディレクトリで行う手順
    pub steps: Vec<Step>,
}

impl Target {
    /// 候補一覧に出す、実行内容の要約
    ///
    /// 実行するコマンドをそのまま並べる。**何が走るのかを選ぶ前に見せる**ためであり、
    /// この文字列を実行することはない（実行は常に引数配列）
    pub fn summary(&self) -> String {
        let commands: Vec<String> = self
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Run(plan) => Some(plan.command_line()),
                _ => None,
            })
            .collect();

        if commands.is_empty() {
            // 実行できない理由がある候補（同一エコシステムの lockfile が複数など）
            // 一覧から消さずに残し、選んだときに理由を出す
            return String::new();
        }

        commands.join(COMMAND_SEPARATOR)
    }

    /// この対象で参照するマニフェストの名前（プレビューに出す）
    ///
    /// 同じディレクトリに Node と Ruby が同居する場合など、複数返り得る
    pub fn manifests(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self
            .lockfiles()
            .into_iter()
            .filter_map(|lockfile| {
                RECIPES
                    .iter()
                    .find(|recipe| recipe.lockfile == lockfile)
                    .map(|recipe| recipe.manifest)
            })
            .collect();
        names.dedup();
        names
    }

    /// この対象で検出した lockfile の名前（プレビューに出す）
    pub fn lockfiles(&self) -> Vec<&'static str> {
        self.steps
            .iter()
            .flat_map(|step| match step {
                Step::Run(plan) => vec![plan.lockfile],
                Step::Ambiguous { lockfiles, .. } => lockfiles.clone(),
            })
            .collect()
    }
}

/// [`Target::summary`] で複数のコマンドを並べるときの区切り
const COMMAND_SEPARATOR: &str = " + ";

/// 検出テーブルが見る lockfile の名前一覧
fn lockfile_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = RECIPES.iter().map(|recipe| recipe.lockfile).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// worktree の中から、依存インストールの対象になり得るディレクトリを列挙する
///
/// `git ls-files` で引き、ファイルシステムを歩かない。走査で探すと `target/` や
/// `node_modules/` を踏んで秒単位に達する（実測 800ms）。`git ls-files` は追跡ファイルしか
/// 返さないため、それらを踏まないことがコマンドの選択で担保される
///
/// 返る並びは git の出力順（辞書順）であり、ルートが先に来る
///
/// # Errors
///
/// `git ls-files` の実行に失敗した場合にエラーを返す。
pub fn targets(worktree: &Path) -> crate::error::Result<Vec<Target>> {
    let names = lockfile_names();
    let names: Vec<&str> = names.to_vec();
    let paths = tracked_files_named(worktree, &names)?;

    let mut directories: Vec<String> = paths
        .iter()
        .map(|path| match path.rsplit_once('/') {
            Some((directory, _file)) => directory.to_owned(),
            // ルート直下の lockfile
            None => String::new(),
        })
        .collect();
    directories.sort();
    directories.dedup();

    Ok(directories
        .into_iter()
        .map(|relative| {
            let steps = detect(&worktree.join(&relative));
            Target { relative, steps }
        })
        .filter(|target| !target.steps.is_empty())
        .collect())
}

pub fn found(root: &Path) -> Vec<&'static str> {
    RECIPES
        .iter()
        .filter(|recipe| root.join(recipe.lockfile).is_file())
        .map(|recipe| recipe.lockfile)
        .collect()
}

/// 検出した lockfile から、実行する手順を組み立てる（純関数）
///
/// ファイルシステムに触れず、「どの lockfile が在ったか」だけを引数で受け取る
/// （`locale_environment` / `is_debug_enabled` と同じ流儀）
///
/// 同一エコシステムの Recipe が 2 つ以上該当した場合は [`Step::Ambiguous`] を 1 つ返し、
/// そのエコシステムの [`Step::Run`] は返さない。優先順位で暗黙にどちらかへ倒すと、
/// 選ばれなかった側の利用者は worktree を作るたびに無駄な待ちを負い、しかも実運用と
/// 食い違う依存が入るためである。異なるエコシステムは互いに影響しない
#[must_use]
pub fn plan(found: &[&str]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut handled: Vec<Ecosystem> = Vec::new();

    for recipe in &RECIPES {
        if handled.contains(&recipe.ecosystem) {
            continue;
        }
        handled.push(recipe.ecosystem);

        let matched: Vec<&Recipe> = RECIPES
            .iter()
            .filter(|candidate| candidate.ecosystem == recipe.ecosystem)
            .filter(|candidate| found.contains(&candidate.lockfile))
            .collect();

        match matched.as_slice() {
            [] => {}
            [only] => steps.push(Step::Run(only.plan())),
            several => steps.push(Step::Ambiguous {
                ecosystem: recipe.ecosystem,
                lockfiles: several.iter().map(|recipe| recipe.lockfile).collect(),
            }),
        }
    }

    steps
}

/// ディレクトリ直下を走査し、実行する手順を組み立てる
///
/// lockfile は存在の有無だけを見る（内容は開かない）
fn detect(root: &Path) -> Vec<Step> {
    plan(&found(root))
}

/// worktree のルート直下を走査し、対応するインストールコマンドを実行する
///
/// **成否を呼び出し側へ返さない。**worktree の作成そのものは既に成功しており、
/// インストールの失敗を `gz worktree add` の失敗として返すと、`&&` で繋いだシェルや
/// スクリプトに「worktree ができていない」と誤解させるためである
///
/// `directory` には、`git worktree list --porcelain -z` が報告した**登録済みの
/// worktree パス**を渡すこと（利用者が打った文字列をそのまま渡してはならない
/// [`crate::commands::worktree`] を参照）
///
/// `writer` には標準エラーを渡す。標準出力は `cd "$(gz worktree)"` のパイプ用途のために
/// 空けたままにする
pub fn install_dependencies(messages: &dyn Messages, directory: &Path, writer: &mut impl Write) {
    install(messages, directory, &detect(directory), spawn, writer);
}

/// 検出済みの手順をそのまま実行する
///
/// 候補として先に検出しておいた [`Target::steps`] を、選択後に実行するために使う
/// （[`install_dependencies`] は検出と実行を続けて行うが、こちらは検出をやり直さない）
pub fn install_steps(
    messages: &dyn Messages,
    directory: &Path,
    steps: &[Step],
    writer: &mut impl Write,
) {
    install(messages, directory, steps, spawn, writer);
}

/// 組み立てた手順を 1 つずつ実行する
///
/// 実行そのものを `run` で受け取るのは、プロセス起動を伴わずに順序・出力・分岐を
/// 単体テストできるようにするため（`gz fetch --siblings` / `gz pull` で確立済みのパターン）
///
/// 並列にしない。継承 stdio では出力が混ざり、どちらのコマンドの行なのか読めなくなる
/// （同居する例は高々 2 つであり、並列化の利得が複雑さに見合わない）
///
/// どの経路も `Result` を返さない。コマンド不在・非ゼロ終了・競合はいずれも警告を
/// 書いて次の手順へ進む（通知（FR-29）と違って黙らないのは、依存が入らなかった
/// ことが利用者の知るべき結果そのものであるため）
fn install(
    messages: &dyn Messages,
    directory: &Path,
    steps: &[Step],
    mut run: impl FnMut(&Path, &Plan) -> Outcome,
    writer: &mut impl Write,
) {
    for step in steps {
        match step {
            Step::Run(plan) => {
                // install は数分掛かり得るため、何を待っているのかが分からないまま
                // 端末が止まらないよう、起動の直前に 1 行出す（FR-23 の `[n/N]` と同じ流儀）
                report(
                    writer,
                    &messages
                        .worktree()
                        .install_running(directory, &plan.command_line()),
                );

                match run(directory, plan) {
                    Outcome::Completed => {}
                    Outcome::Missing => report(
                        writer,
                        &messages.worktree().install_command_missing(plan.program),
                    ),
                    Outcome::Failed => report(
                        writer,
                        &messages.worktree().install_failed(&plan.command_line()),
                    ),
                }
            }
            Step::Ambiguous {
                ecosystem,
                lockfiles,
            } => report(
                writer,
                &messages
                    .worktree()
                    .install_ambiguous(ecosystem.label(), &lockfiles.join(LOCKFILE_SEPARATOR)),
            ),
        }
    }
}

/// 1 行を書き出す。書き込みに失敗しても無視する
///
/// worktree の作成は既に成功しており、警告を書けなかったことでその結果を変えない
/// （この module の関数がどの経路でも `Result` を返さないのはそのため）
fn report(writer: &mut impl Write, line: &str) {
    let _ = writeln!(writer, "{line}");
}

/// インストールコマンドを起動する（既定の実行方法）。
///
/// **シェルを経由せず引数配列で**起動し、`directory` を作業ディレクトリにする
/// （git・通知と同じ規則）。標準入出力はすべて継承。出力をそのまま流すのは
/// 進捗を見せるためであり、fuzgit はこれを解釈しない。標準入力を塞がないのは、
/// プライベートレジストリの認証や bundler の資格情報の入力が必要になった場合に、
/// 塞いでいると理由の分からない失敗になるためである
///
/// 環境変数は一切加工しない（module の doc comment「第三の扱い」を参照）
fn spawn(directory: &Path, plan: &Plan) -> Outcome {
    log_spawn(directory, plan);

    match Command::new(plan.program)
        .args(plan.arguments)
        .current_dir(directory)
        .status()
    {
        Ok(status) if status.success() => Outcome::Completed,
        // シグナルで終了した場合も終了コードを持たないだけで失敗である
        Ok(_) => Outcome::Failed,
        // PATH に無い場合だけを [`Outcome::Missing`] とし、「入れれば動く」ことを
        // 案内できるようにする。`which` を自前で実装して事前に探すことはしない
        // （探索規則を OS と重複して持つことになり、結果が食い違えば事故になる）
        Err(error) if error.kind() == ErrorKind::NotFound => Outcome::Missing,
        // 権限が無い等、起動できたか否かに関わらず「実行できなかった」ことは同じであるため、
        // 非ゼロ終了と同じ警告（再実行するコマンドの提示）へ寄せる
        Err(error) => {
            log_spawn_error(plan, &error);
            Outcome::Failed
        }
    }
}

/// 起動するコマンドをデバッグログへ出す
///
/// 検出結果が想定と違う場合の切り分けのため、`FUZGIT_DEBUG=1` のときだけ出す
/// （体裁は git・通知と揃える）。出力先を標準エラーにするのは、標準出力を
/// パス出力のパイプ用途に空けておくためである
fn log_spawn(directory: &Path, plan: &Plan) {
    if !debug_enabled() {
        return;
    }

    // ログの書き込み失敗で主処理を止めたくないため結果を破棄する（`exec` の `log_command` と同じ）
    let _ = writeln!(
        std::io::stderr(),
        "{DEBUG_PREFIX} install (cwd: {directory}) {command} [{lockfile}]",
        directory = directory.display(),
        command = plan.command_line(),
        lockfile = plan.lockfile,
    );
}

/// 起動そのものに失敗した理由をデバッグログへ出す
///
/// 利用者へ見せる警告は「何を実行しようとして失敗したか」だけで足りるが、
/// PATH 不在以外の理由（権限など）は切り分けに要るためデバッグログへ残す
fn log_spawn_error(plan: &Plan, error: &std::io::Error) {
    if !debug_enabled() {
        return;
    }

    let _ = writeln!(
        std::io::stderr(),
        "{DEBUG_PREFIX} install failed to start ({program}): {error}",
        program = plan.program,
    );
}

#[cfg(test)]
mod tests {

    /// FR-30 改訂: 対象の列挙と要約
    mod targets {
        use super::*;

        #[test]
        fn the_summary_lists_the_commands_that_would_run() {
            let target = Target {
                relative: "projectA".to_owned(),
                steps: plan(&["package-lock.json"]),
            };

            assert_eq!(target.summary(), "npm install");
        }

        #[test]
        fn several_ecosystems_in_one_directory_are_joined() {
            // Rails + JS のように同居する構成では、そのディレクトリで 2 つ走る
            let target = Target {
                relative: String::new(),
                steps: plan(&["package-lock.json", "Gemfile.lock"]),
            };

            let summary = target.summary();
            assert!(summary.contains("npm install"), "{summary}");
            assert!(summary.contains("bundle"), "{summary}");
            assert!(summary.contains(COMMAND_SEPARATOR), "{summary}");
        }

        #[test]
        fn a_target_that_cannot_run_has_an_empty_summary_but_stays_a_candidate() {
            // 同一エコシステムの lockfile が複数ある。選んだときに理由を出すため、
            // 一覧からは消さない
            let target = Target {
                relative: "projectA".to_owned(),
                steps: plan(&["pnpm-lock.yaml", "package-lock.json"]),
            };

            assert!(target.summary().is_empty());
            assert!(!target.steps.is_empty());
        }

        #[test]
        fn the_lockfiles_of_a_target_are_reported_for_the_preview() {
            let target = Target {
                relative: String::new(),
                steps: plan(&["package-lock.json", "Gemfile.lock"]),
            };

            let lockfiles = target.lockfiles();
            assert!(lockfiles.contains(&"package-lock.json"));
            assert!(lockfiles.contains(&"Gemfile.lock"));
        }

        #[test]
        fn every_recipe_lockfile_is_searched_for() {
            // 検出テーブルに行を足したら、探す名前も自動で増えること
            let names = lockfile_names();

            for recipe in &RECIPES {
                assert!(
                    names.contains(&recipe.lockfile),
                    "{} を探していない",
                    recipe.lockfile
                );
            }
        }
    }

    use super::*;
    use crate::i18n::Language;
    use crate::test_support::TempDir;

    /// 実行の記録だけを取り、プロセスを起動しない `run`
    fn recorder<'a>(
        log: &'a mut Vec<String>,
        outcome: Outcome,
    ) -> impl FnMut(&Path, &Plan) -> Outcome + 'a {
        move |directory, plan| {
            log.push(format!(
                "{directory}: {command}",
                directory = directory.display(),
                command = plan.command_line()
            ));
            outcome
        }
    }

    /// `install` を走らせ、書き出された標準エラー相当の文字列を返す
    fn run_install(steps: &[Step], outcome: Outcome) -> (Vec<String>, String) {
        let mut log = Vec::new();
        let mut written = Vec::new();

        install(
            Language::English.messages(),
            Path::new("/tmp/worktree"),
            steps,
            recorder(&mut log, outcome),
            &mut written,
        );

        (
            log,
            String::from_utf8(written).expect("the wording must be UTF-8"),
        )
    }

    /// 手順から実行されるコマンド文字列だけを取り出す
    fn command_lines(steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .filter_map(|step| match step {
                Step::Run(plan) => Some(plan.command_line()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_detection_table_is_the_one_written_in_the_requirements() {
        // 実行するコマンドの出所を 1 か所に保つため、表そのものを固定する
        let table: Vec<(&str, String)> = RECIPES
            .iter()
            .map(|recipe| (recipe.lockfile, recipe.plan().command_line()))
            .collect();

        assert_eq!(
            table,
            [
                ("pnpm-lock.yaml", "pnpm install".to_owned()),
                ("yarn.lock", "yarn install".to_owned()),
                ("package-lock.json", "npm install".to_owned()),
                ("uv.lock", "uv sync".to_owned()),
                ("Gemfile.lock", "bundle install".to_owned()),
                ("composer.lock", "composer install".to_owned()),
            ]
        );
    }

    #[test]
    fn no_recipe_uses_the_strict_spelling_that_fails_on_a_stale_lockfile() {
        // マニフェストと lockfile が同期していないリポジトリで必ず失敗する綴りは使わない
        // （利用者からの報告。`npm ci` が `package.json` と `package-lock.json` の不一致で
        // 毎回失敗していた）。足されたら気付けるように固定する
        for recipe in &RECIPES {
            assert_ne!(recipe.arguments, ["ci"], "{}", recipe.program);
            for argument in recipe.arguments {
                assert!(
                    !matches!(*argument, "--frozen-lockfile" | "--immutable" | "--frozen"),
                    "`{program} {argument}` は同期していない lockfile で失敗する",
                    program = recipe.program
                );
            }
        }
    }

    #[test]
    fn cargo_and_go_are_deliberately_absent_from_the_table() {
        // グローバルキャッシュを温めるだけのコマンドは足さない
        // （requirements.md「スコープ外（FR-30 …）」）。追加されたら気付けるように固定する
        for lockfile in ["Cargo.lock", "go.sum", "go.mod", "bun.lockb"] {
            assert!(
                !RECIPES.iter().any(|recipe| recipe.lockfile == lockfile),
                "`{lockfile}` must not be in the detection table"
            );
        }
    }

    #[test]
    fn no_lockfile_produces_no_step_at_all() {
        assert_eq!(plan(&[]), []);
    }

    #[test]
    fn a_single_lockfile_produces_the_command_of_its_row() {
        for (lockfile, expected) in [
            ("pnpm-lock.yaml", "pnpm install"),
            ("yarn.lock", "yarn install"),
            ("package-lock.json", "npm install"),
            ("uv.lock", "uv sync"),
            ("Gemfile.lock", "bundle install"),
            ("composer.lock", "composer install"),
        ] {
            assert_eq!(command_lines(&plan(&[lockfile])), [expected]);
        }
    }

    #[test]
    fn two_lockfiles_of_the_same_ecosystem_stop_that_ecosystem() {
        assert_eq!(
            plan(&["pnpm-lock.yaml", "package-lock.json"]),
            [Step::Ambiguous {
                ecosystem: Ecosystem::Node,
                lockfiles: vec!["pnpm-lock.yaml", "package-lock.json"],
            }]
        );
    }

    #[test]
    fn every_node_lockfile_at_once_is_reported_in_the_order_of_the_table() {
        assert_eq!(
            plan(&["package-lock.json", "yarn.lock", "pnpm-lock.yaml"]),
            [Step::Ambiguous {
                ecosystem: Ecosystem::Node,
                lockfiles: vec!["pnpm-lock.yaml", "yarn.lock", "package-lock.json"],
            }]
        );
    }

    #[test]
    fn lockfiles_of_different_ecosystems_are_not_a_conflict() {
        // Rails + JS のような同居。検出テーブル順に 1 つずつ実行する
        assert_eq!(
            command_lines(&plan(&["Gemfile.lock", "package-lock.json"])),
            ["npm install", "bundle install"]
        );
    }

    #[test]
    fn a_conflict_in_one_ecosystem_leaves_the_others_running() {
        let steps = plan(&["pnpm-lock.yaml", "package-lock.json", "uv.lock"]);

        assert_eq!(
            steps,
            [
                Step::Ambiguous {
                    ecosystem: Ecosystem::Node,
                    lockfiles: vec!["pnpm-lock.yaml", "package-lock.json"],
                },
                Step::Run(Plan {
                    lockfile: "uv.lock",
                    program: "uv",
                    arguments: &["sync"],
                }),
            ]
        );
    }

    #[test]
    fn all_four_ecosystems_run_in_the_order_of_the_table() {
        assert_eq!(
            command_lines(&plan(&[
                "composer.lock",
                "Gemfile.lock",
                "uv.lock",
                "yarn.lock"
            ])),
            [
                "yarn install",
                "uv sync",
                "bundle install",
                "composer install",
            ]
        );
    }

    #[test]
    fn only_the_lockfiles_that_exist_are_found() {
        let dir = TempDir::new("install-found");
        std::fs::write(dir.path().join("package-lock.json"), "{}").expect("writable");
        std::fs::write(dir.path().join("Gemfile.lock"), "").expect("writable");
        std::fs::write(dir.path().join("Cargo.lock"), "").expect("writable");

        assert_eq!(found(dir.path()), ["package-lock.json", "Gemfile.lock"]);
    }

    #[test]
    fn a_directory_named_like_a_lockfile_is_not_a_lockfile() {
        let dir = TempDir::new("install-found-dir");
        std::fs::create_dir(dir.path().join("uv.lock")).expect("creatable");

        assert_eq!(found(dir.path()), [] as [&str; 0]);
    }

    #[test]
    fn the_scan_does_not_descend_into_subdirectories() {
        // モノレポのサブパッケージまでは見ない（requirements.md「スコープ外（FR-30 …）」）
        let dir = TempDir::new("install-found-nested");
        let nested = dir.path().join("packages").join("app");
        std::fs::create_dir_all(&nested).expect("creatable");
        std::fs::write(nested.join("pnpm-lock.yaml"), "").expect("writable");

        assert_eq!(found(dir.path()), [] as [&str; 0]);
    }

    #[test]
    fn a_yarn_lockfile_is_installed_whatever_its_version_is() {
        // 既定の綴り `yarn install` は Yarn 1 / Yarn 2 以降で同じであり、
        // 版の判別（`yarn.lock` のヘッダ読み）を要しない
        let dir = TempDir::new("install-detect-yarn");
        std::fs::write(dir.path().join("yarn.lock"), "# handwritten\n").expect("writable");
        std::fs::write(dir.path().join("uv.lock"), "").expect("writable");

        assert_eq!(
            detect(dir.path()),
            [
                Step::Run(Plan {
                    lockfile: "yarn.lock",
                    program: "yarn",
                    arguments: &["install"],
                }),
                Step::Run(Plan {
                    lockfile: "uv.lock",
                    program: "uv",
                    arguments: &["sync"],
                }),
            ]
        );
    }

    #[test]
    fn a_worktree_without_a_lockfile_prints_nothing() {
        let (log, written) = run_install(&[], Outcome::Completed);

        assert!(log.is_empty());
        assert_eq!(written, "");
    }

    #[test]
    fn every_command_is_announced_before_it_starts() {
        let steps = plan(&["package-lock.json", "Gemfile.lock"]);
        let (log, written) = run_install(&steps, Outcome::Completed);

        assert_eq!(
            log,
            [
                "/tmp/worktree: npm install",
                "/tmp/worktree: bundle install"
            ]
        );
        assert!(written.contains("npm install") && written.contains("bundle install"));
        assert_eq!(
            written.lines().count(),
            2,
            "a successful run only announces itself: {written}"
        );
    }

    #[test]
    fn a_missing_command_is_reported_and_the_next_step_still_runs() {
        let steps = plan(&["package-lock.json", "Gemfile.lock"]);
        let (log, written) = run_install(&steps, Outcome::Missing);

        assert_eq!(log.len(), 2, "the following step must still be attempted");
        assert!(
            written.contains("npm") && written.contains("bundle"),
            "both missing commands must be named: {written}"
        );
    }

    #[test]
    fn a_failing_command_is_reported_and_the_next_step_still_runs() {
        let steps = plan(&["package-lock.json", "Gemfile.lock"]);
        let (log, written) = run_install(&steps, Outcome::Failed);

        assert_eq!(log.len(), 2, "the following step must still be attempted");
        assert_eq!(
            written.matches("npm install").count(),
            2,
            "the command to run again must be shown: {written}"
        );
    }

    #[test]
    fn a_conflict_is_named_instead_of_being_silently_skipped() {
        let steps = plan(&["pnpm-lock.yaml", "package-lock.json"]);
        let (log, written) = run_install(&steps, Outcome::Completed);

        assert!(log.is_empty(), "nothing may be executed for a conflict");
        assert!(
            written.contains("pnpm-lock.yaml") && written.contains("package-lock.json"),
            "the lockfiles that were found must be listed: {written}"
        );
    }

    #[test]
    fn the_wording_of_every_outcome_is_translated() {
        let steps = plan(&[
            "pnpm-lock.yaml",
            "package-lock.json",
            "yarn.lock",
            "uv.lock",
        ]);

        let mut written = [Vec::new(), Vec::new()];
        for (index, language) in [Language::Japanese, Language::English]
            .into_iter()
            .enumerate()
        {
            let mut log = Vec::new();
            install(
                language.messages(),
                Path::new("/tmp/worktree"),
                &steps,
                recorder(&mut log, Outcome::Failed),
                &mut written[index],
            );
        }

        let [japanese, english] =
            written.map(|bytes| String::from_utf8(bytes).expect("the wording must be UTF-8"));
        assert_ne!(japanese, english, "the wording must be translated");
    }

    #[test]
    fn the_no_install_flag_becomes_a_mode() {
        assert_eq!(InstallMode::from_no_install(true), InstallMode::Skip);
        assert_eq!(InstallMode::from_no_install(false), InstallMode::Run);
    }

    #[test]
    fn a_missing_program_is_told_apart_from_a_failing_one() {
        // 実在しないプログラム名で `ErrorKind::NotFound` の経路を通す
        // （インストールコマンドの実在を前提にしない。`notify` の同種のテストと同じ形）
        let plan = Plan {
            lockfile: "package-lock.json",
            program: "fuzgit-installer-that-does-not-exist",
            arguments: &["install"],
        };

        assert_eq!(spawn(Path::new("."), &plan), Outcome::Missing);
    }

    #[test]
    fn a_program_that_exits_non_zero_is_a_failure() {
        // `false` はどの環境にもある「必ず失敗するコマンド」
        let plan = Plan {
            lockfile: "package-lock.json",
            program: "false",
            arguments: &[],
        };

        assert_eq!(spawn(Path::new("."), &plan), Outcome::Failed);
    }

    #[test]
    fn a_program_that_exits_zero_completes() {
        let plan = Plan {
            lockfile: "package-lock.json",
            program: "true",
            arguments: &[],
        };

        assert_eq!(spawn(Path::new("."), &plan), Outcome::Completed);
    }

    #[test]
    fn the_displayed_command_is_the_arguments_joined_by_spaces() {
        // 表示専用であり、この文字列を実行することはない
        assert_eq!(
            Plan {
                lockfile: "uv.lock",
                program: "uv",
                arguments: &["sync", "--all-groups"],
            }
            .command_line(),
            "uv sync --all-groups"
        );
        assert_eq!(
            Plan {
                lockfile: "Gemfile.lock",
                program: "bundle",
                arguments: &[],
            }
            .command_line(),
            "bundle"
        );
    }
}
