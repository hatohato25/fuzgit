# fuzgit

[English](README.md) | **日本語**

fuzzy finder で「選ぶ」「探す」「辿る」ことを軸にした git 操作 CLI ツールです。

ブランチ名やコミットハッシュを正確に覚えていなくても、絞り込みと選択だけで日常的な git 操作を
完結できることを目指しています。すべてのサブコマンドが
**候補一覧の取得 → fuzzy finder で絞り込み・選択（＋プレビュー）→ git 操作の実行**
という同じ操作モデルに従います。fuzzy finder は [skim](https://crates.io/crates/skim) を
ライブラリとして組み込んでいるため、外部の `fzf` / `sk` バイナリは不要です。

- パッケージ名: **`fuzgit`**
- 実行コマンド名（バイナリ名）: **`gz`**

## 前提条件

- **システムに `git` がインストールされていること（必須）**
  書き込み系の操作とプレビュー用の色付き差分生成は、システムの `git` コマンドへシェルアウトして
  実行します（リポジトリ情報の読み取りには [gix](https://crates.io/crates/gix) を使います）。
- **`gz merge` のコンフリクト予測には Git 2.38 以降が必要です（任意）**
  2.38 未満では予測の表示だけを省略し、merge の実行はそのまま続けます。
- ビルドする場合は stable の Rust ツールチェイン（edition 2024 を使うため Rust 1.85 以降）

## インストール

### Homebrew（推奨）

```sh
brew tap hatohato25/fuzgit
brew trust --formula hatohato25/fuzgit/fuzgit
brew install hatohato25/fuzgit/fuzgit
```

Formula 名は `fuzgit` ですが、**インストールされるコマンドは `gz`** です。

```sh
gz --version
```

ビルド済みバイナリは macOS（Apple Silicon / Intel）と Linux（x86_64）向けに提供しています。

### Linux / WSL（インストールスクリプト）

macOS は Homebrew、Linux と WSL はこちらを使ってください。

```sh
curl -fsSL https://raw.githubusercontent.com/hatohato25/fuzgit/main/install.sh | sh
```

実行環境に合うリリース資産を取得し、**SHA-256 チェックサムを検証**したうえで `gz` を
`~/.local/bin` へ配置します。`sudo` は不要で、そのディレクトリの外には何も書きません。
`~/.local/bin` が `PATH` に無い場合は、追加すべき行を表示します（**シェルの設定ファイルを
勝手に書き換えることはしません**）。

オプション:

```sh
# 最新ではなく特定のバージョンを入れる
curl -fsSL .../install.sh | sh -s -- --version v0.5.0

# 配置先を変える（ディレクトリによっては sudo が必要）
curl -fsSL .../install.sh | sh -s -- --bin-dir /usr/local/bin
```

環境変数 `FUZGIT_VERSION` / `FUZGIT_BIN_DIR` でも同じ指定ができます。

現在 Linux 向けに配布しているのは **x86_64 のみ**です。`aarch64` では動かないバイナリを
入れずに停止し、ソースからのビルドを案内します。

> スクリプトをシェルへパイプするのは、読んでいないコードを実行することでもあります。
> 中身はこのリポジトリの [`install.sh`](install.sh) です。先に読んでから手元で実行しても
> 構いません。

### ソースからインストール

crates.io へは未公開のため、リポジトリを取得してローカルからインストールします。

```sh
git clone https://github.com/hatohato25/fuzgit.git
cd fuzgit
cargo install --path .
```

`~/.cargo/bin/gz` がインストールされます（パッケージ名は `fuzgit`、コマンド名は `gz`）。

インストールせずに試す場合:

```sh
cargo build --release
./target/release/gz --help
```

## クイックスタート

```sh
gz branch              # ブランチを選んで切り替える
gz status              # 変更ファイルを選んで、add / restore / stash / commit などを行う
git show "$(gz log)"   # コミットを選んでフルハッシュを受け取る
gz log --action        # コミットを選んで、続けて行う操作も選ぶ
```

引数なしの `gz` および `gz --help` でサブコマンド一覧を表示します。

### 見つけたコミットから次の操作へ

`gz log` と `gz reflog` はフルハッシュを 1 行出して終わります。これがあるおかげで
`$(gz log)` が成立します。`--action` を付けると、その後にもう 1 つメニューが出ます
— 詳細を表示する、detached HEAD で切り替える、cherry-pick する、revert する、
fixup コミットを作る、（`gz reflog` なら）`y/N` の確認を経て現在のブランチをそこへ戻す。
「フルハッシュを出力する」もメニューの項目なので、パイプの使い方へいつでも戻れます。

**`--action` を付けなければ挙動は一切変わりません。**既定の標準出力は従来のままなので、
`git show "$(gz log)"` はそのまま動きます。`gz reflog --restore <NAME>` も非推奨には
なりません。**入力が必要な名前を渡すなら `--restore`、名前を要さない操作なら `--action`**
と使い分けてください（両者は同時に指定できません）。

## 表示言語

メッセージ・確認プロンプト・finder のヘッダー・`--help` は **日本語と英語**に対応しています。
表示言語は**ロケールから自動的に決まり**、日本語ロケールでない場合や解釈できない場合は
**英語**になります。明示的に固定するには次のように指定します。

```sh
git config --global fuzgit.lang ja   # 永続設定。--local ならリポジトリごとに変えられる。`en` なら英語に固定
gz --lang ja branch                  # 単発の上書き。全サブコマンドで指定できる
```

表示言語は次の順で解決し、先に決まった層より下は参照しません。

| 優先 | 取得元 |
|---|---|
| 1 | `--lang <ja\|en\|auto>`（全サブコマンド共通のグローバルオプション） |
| 2 | `FUZGIT_LANG` 環境変数 |
| 3 | `git config fuzgit.lang`（system / global / local / worktree の階層がそのまま効く） |
| 4 | `LC_ALL` → `LC_MESSAGES` → `LANGUAGE` → `LANG` |
| 5 | フォールバック: `en` |

層 1〜3 は fuzgit への明示的な指定なので、`ja` / `en` / `auto` 以外の値はエラーで停止します。
層 4 は環境の記述にすぎないため、解釈できない値（`C` / `POSIX` を含む）はエラーにせず
フォールバックへ進みます。`auto` は以降の明示指定を飛ばして環境からの自動判定へ進みます。
fuzgit 独自の設定ファイルは持たず、git config の `fuzgit.lang` を借ります（リポジトリの外でも
読めます）。

以下の 2 点は制約として把握しておいてください。

- **git 本体のメッセージが翻訳されることは保証しません。** fuzgit は起動する git へ言語を
  伝えますが、翻訳が存在するかは git のビルド（NLS の有無）とロケールデータに依存します。
  とくに **git 本体には日本語カタログがありません**。`ja` を選んでも git 自身の出力は英語のままです。
- **clap が自前で出す固定文言（`Usage:` / `Options:` / `Commands:` / パーサエラー）は英語のまま**です
  （clap 4 に多言語化の機構がないため）。`--help` のうち fuzgit 自身の説明は切り替わります。

以降の端末例は、日本語表示（`fuzgit.lang ja`）での出力です。

## 主な機能

全 20 コマンドの中でも、fuzgit の性格がよく分かる 4 つです。

### 探して選ぶ

**[`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) — ブランチを選んで切り替える**

ブランチ名を正確に覚えていなくても、絞り込んで選ぶだけで切り替えられます。プレビューには
選択中ブランチの直近 50 件のコミット（`git log --oneline --decorate`）を表示します。
`--all` を付けるとリモート追跡ブランチも候補に含まれ、`origin/feature` を選ぶと git の DWIM により
追跡ローカルブランチが作成されます。

```
$ gz branch --all
>  * main
     feature/login
     origin/feature/search
```

**[`gz stash`](https://hatohato25.github.io/fuzgit/docs.html#stash) — stash を検索して復元する**

`apply` / `pop` / `drop` の候補は `stash@{n}: <メッセージ>` 形式なので、番号ではなくメッセージで
絞り込めます。プレビューは `git stash show -p --color=always`、`drop` は実行前に確認プロンプト
（`[y/N]`）を表示します。`gz stash push` は `Tab` で複数選択でき、**選んだファイルだけ**が
退避されます（選ばなかった変更は作業ツリーに残ります）。

### 選んでまとめて実行する

**[`gz fetch -s`](https://hatohato25.github.io/fuzgit/docs.html#fetch) — 隣のリポジトリもまとめて fetch**

`-s` / `--siblings` を付けると、現在の worktree root の親ディレクトリ直下だけを走査して（再帰しません）、
`.git` を持つディレクトリを候補にします。候補行は `<ディレクトリ名>  <リモート>/<現在のブランチ>` で、
現在のリポジトリは選択済みで始まります。`Tab` で複数選択すれば、複数リポジトリをまとめて
fetch できます。fetch できないリポジトリは黙って消さず、除外した件数をヘッダーに示します。

```
$ gz fetch --siblings
現在のリポジトリを選択済みにしています。Tab: 選択の切替 / Enter: 取得  |  除外 1 件（リモート未登録 / bare）
>> mike   origin/main
   alpha  origin/main
   zulu   origin/main
```

選んだリポジトリは**並列に**取得します。待ちの実体がネットワークの往復であり、対象が互いに独立した
別リポジトリだからです。同時実行数は既定 4 で、`git config fuzgit.fetchJobs <n>` で変更できます
（`1` を指定すると従来どおりの直列実行に戻ります）。**この設定が効くのは `gz fetch --siblings` だけ**で、
`gz fetch` 単体と `gz pull` は fetch が 1 回だけなので並列化する対象がありません。

各リポジトリの出力はキャプチャして、完了した順にひとまとまりで書き出すため、更新表が混ざることは
ありません。並列フェーズは一切の対話ができないので、**パスワードや passphrase が必要なものは、
そのあと端末をつないだまま 1 件ずつ実行し直します**（リトライではなく実行方法の切り替えです）。
git config に `core.sshCommand` を設定している場合、並列フェーズはそれを上書きするため対象がすべて
この直列フェーズへ回ります。結果は正しく得られますが、高速化の恩恵は受けられません。

**[`gz pull`](https://hatohato25.github.io/fuzgit/docs.html#pull) — 複数ブランチをまとめて追随させる**

選ばせるのは「どのローカルブランチを upstream へ追随させるか」だけで、取り込みは fast-forward のみです。
現在のブランチは選択済みで始まります。候補一覧の順に 1 件ずつ直列に実行し、途中で失敗しても中断せず、
最後に成功・失敗の件数を集計します。upstream 未設定などで対象にできないブランチは、除外件数を
ヘッダーに示します。

```
$ gz pull
[1/4] main
[2/4] alpha
[3/4] diverged
[4/4] zeta
成功 3 件 / 失敗 1 件（失敗: diverged）
```

こちらを直列のままにしているのは意図的です。リモートごとの fetch は対象数が実質 1 件であり
（大半のリポジトリで upstream のリモートは 1 つ）、ブランチの取り込みは同一リポジトリへの
書き込みで index や参照の lock が競合するためです。

**長い実行が終わったときのデスクトップ通知。** `gz fetch --siblings` と `gz pull` は、完了を
通知で知らせることができます。席を外しているときに役立ちます。

```sh
git config --global fuzgit.notify true
```

**明示的に有効化しない限り出ません**。また 10 秒未満で終わった実行では通知しません。本文は件数だけで、
リポジトリ名・ブランチ名・パスは含めません。実際にバナーが出るかは環境によります（macOS は
`osascript` を使うため端末アプリへの通知許可が必要、Linux は `notify-send` を使うため未導入だと
出ません）。fuzgit はこれを失敗として扱わず、通知の有無に関わらず集計は必ず標準エラーへ出すため、
**通知はあくまで補助であり、結果を知る唯一の経路にはなりません**。

## コマンド一覧

| サブコマンド | 概要 |
|---|---|
| [`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) | ブランチを選んで切り替える（サブコマンドで作成・削除・整理も行う） |
| [`gz log`](https://hatohato25.github.io/fuzgit/docs.html#log) | コミット履歴を辿り、フルハッシュを標準出力へ出す（`--action` で次の操作を選ぶ） |
| [`gz cherry-pick`](https://hatohato25.github.io/fuzgit/docs.html#cherry-pick) | コミットを選んで cherry-pick する |
| [`gz restore`](https://hatohato25.github.io/fuzgit/docs.html#restore) | ファイルを選んで復元・アンステージする |
| [`gz add`](https://hatohato25.github.io/fuzgit/docs.html#add) | 未ステージ・未追跡ファイルを選んでステージする |
| [`gz stash <サブコマンド>`](https://hatohato25.github.io/fuzgit/docs.html#stash) | 変更を stash へ退避し、stash を検索して適用・破棄する |
| [`gz reflog`](https://hatohato25.github.io/fuzgit/docs.html#reflog) | HEAD の reflog を辿り、失われたコミットを取り出す（`--action` で次の操作を選ぶ） |
| [`gz commit`](https://hatohato25.github.io/fuzgit/docs.html#commit) | 変更ファイルを選んで、選んだものだけをコミットする |
| [`gz fixup`](https://hatohato25.github.io/fuzgit/docs.html#fixup) | 修正対象のコミットを選んで fixup コミットを作る |
| [`gz merge`](https://hatohato25.github.io/fuzgit/docs.html#merge) | merge するブランチを選ぶ（進行中は復帰メニュー） |
| [`gz rebase`](https://hatohato25.github.io/fuzgit/docs.html#rebase) | rebase の base を選ぶ（進行中は復帰メニュー） |
| [`gz revert`](https://hatohato25.github.io/fuzgit/docs.html#revert) | 打ち消すコミットを選んで revert する |
| [`gz status`](https://hatohato25.github.io/fuzgit/docs.html#status) | 変更ファイルを一覧し、選んだファイルに操作を行う（2 段選択） |
| [`gz diff`](https://hatohato25.github.io/fuzgit/docs.html#diff) | 比較対象を選んで差分を表示する |
| [`gz fetch`](https://hatohato25.github.io/fuzgit/docs.html#fetch) | fetch の対象を決めて取得する（`--siblings` で隣のリポジトリも一括取得。**ネットワークを使う**） |
| [`gz pull`](https://hatohato25.github.io/fuzgit/docs.html#pull) | ブランチを選んで upstream へまとめて追随させる（fast-forward のみ。**ネットワークを使う**） |
| [`gz worktree`](https://hatohato25.github.io/fuzgit/docs.html#worktree) | worktree を一覧・管理する（`add <名前>` はリポジトリの兄弟に作成し、`.claude/` を複写する） |

オプション・候補の作り方・プレビューの内容・確認プロンプトの有無は
[ドキュメント](https://hatohato25.github.io/fuzgit/docs.html)に記載しています。

## 開発

変更のたびに、以下の順にすべて成功することを確認してください。

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo test
```

- テスト方針・設計方針・モジュール構成:
  [ドキュメント](https://hatohato25.github.io/fuzgit/docs.html#development)
- ドキュメントサイトのソースは `docs/`（GitHub Pages が `main` ブランチの `docs/` を公開します）

## ライセンス

MIT License. 全文は [LICENSE](LICENSE) を参照してください。
