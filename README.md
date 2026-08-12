# fuzgit

fuzzy finder で「選ぶ」「探す」「辿る」ことを軸にした git 操作 CLI ツールです。

ブランチ名やコミットハッシュを正確に覚えていなくても、絞り込みと選択だけで日常的な git 操作を
完結できることを目指しています。すべてのサブコマンドが
**候補一覧の取得 → fuzzy finder で絞り込み・選択（＋プレビュー）→ git 操作の実行**
という同じ操作モデルに従います。fuzzy finder は [skim](https://crates.io/crates/skim) を
ライブラリとして組み込んでいるため、外部の `fzf` / `sk` バイナリは不要です。

- パッケージ名: **`fuzgit`**
- 実行コマンド名（バイナリ名）: **`gz`**

## ドキュメント

**各コマンドの詳細・キー操作・ネットワーク操作・終了コード・設計方針は、ドキュメントサイトを参照してください。**

- **<https://hatohato25.github.io/fuzgit/>** — ランディングページ
- **<https://hatohato25.github.io/fuzgit/docs.html>** — ドキュメント（English / 日本語）

## 前提条件

- **システムに `git` がインストールされていること（必須）**
  書き込み系の操作とプレビュー用の色付き差分生成は、システムの `git` コマンドへシェルアウトして
  実行します（リポジトリ情報の読み取りには [gix](https://crates.io/crates/gix) を使います）。
- **`gz merge` のコンフリクト予測には Git 2.38 以降が必要です（任意）**
  2.38 未満では予測の表示だけを省略し、merge の実行はそのまま続けます。
- ビルドする場合は stable の Rust ツールチェイン（edition 2024 を使うため Rust 1.85 以降）

## インストール

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
```

引数なしの `gz` および `gz --help` でサブコマンド一覧を表示します。

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
> * main
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
`.git` を持つディレクトリを候補にします。候補行は `<ディレクトリ名>  <現在のブランチ>  <リモート>` で、
現在のリポジトリは選択済みで始まります。`Tab` で複数選択すれば、複数リポジトリをまとめて
fetch できます。fetch できないリポジトリは黙って消さず、除外した件数をヘッダーに示します。

```
$ gz fetch --siblings
現在のリポジトリを選択済みにしています。Tab: 選択の切替 / Enter: 取得  |  除外 1 件（リモート未登録 / bare）
>>mike  main  origin
  alpha  main  origin
  zulu  main  origin
```

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

## コマンド一覧

| サブコマンド | 概要 |
|---|---|
| [`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) | ブランチを選んで切り替える（サブコマンドで作成・削除・整理も行う） |
| [`gz log`](https://hatohato25.github.io/fuzgit/docs.html#log) | コミット履歴を辿り、フルハッシュを標準出力へ出す |
| [`gz cherry-pick`](https://hatohato25.github.io/fuzgit/docs.html#cherry-pick) | コミットを選んで cherry-pick する |
| [`gz restore`](https://hatohato25.github.io/fuzgit/docs.html#restore) | ファイルを選んで復元・アンステージする |
| [`gz add`](https://hatohato25.github.io/fuzgit/docs.html#add) | 未ステージ・未追跡ファイルを選んでステージする |
| [`gz stash <サブコマンド>`](https://hatohato25.github.io/fuzgit/docs.html#stash) | 変更を stash へ退避し、stash を検索して適用・破棄する |
| [`gz tag`](https://hatohato25.github.io/fuzgit/docs.html#tag) | タグを選んで出力・切替・差分表示する |
| [`gz reflog`](https://hatohato25.github.io/fuzgit/docs.html#reflog) | HEAD の reflog を辿り、失われたコミットを取り出す |
| [`gz commit`](https://hatohato25.github.io/fuzgit/docs.html#commit) | 変更ファイルを選んで、選んだものだけをコミットする |
| [`gz push`](https://hatohato25.github.io/fuzgit/docs.html#push) | push 先（リモート × 現在ブランチ）を選んで push する |
| [`gz fixup`](https://hatohato25.github.io/fuzgit/docs.html#fixup) | 修正対象のコミットを選んで fixup コミットを作る |
| [`gz merge`](https://hatohato25.github.io/fuzgit/docs.html#merge) | merge するブランチを選ぶ（進行中は復帰メニュー） |
| [`gz rebase`](https://hatohato25.github.io/fuzgit/docs.html#rebase) | rebase の base を選ぶ（進行中は復帰メニュー） |
| [`gz revert`](https://hatohato25.github.io/fuzgit/docs.html#revert) | 打ち消すコミットを選んで revert する |
| [`gz status`](https://hatohato25.github.io/fuzgit/docs.html#status) | 変更ファイルを一覧し、選んだファイルに操作を行う（2 段選択） |
| [`gz diff`](https://hatohato25.github.io/fuzgit/docs.html#diff) | 比較対象を選んで差分を表示する |
| [`gz fetch`](https://hatohato25.github.io/fuzgit/docs.html#fetch) | fetch の対象を決めて取得する（`--siblings` で隣のリポジトリも一括取得。**ネットワークを使う**） |
| [`gz pull`](https://hatohato25.github.io/fuzgit/docs.html#pull) | ブランチを選んで upstream へまとめて追随させる（fast-forward のみ。**ネットワークを使う**） |
| [`gz sync`](https://hatohato25.github.io/fuzgit/docs.html#sync) | 現在ブランチを upstream と同期する（**ネットワークを使う**） |
| [`gz worktree`](https://hatohato25.github.io/fuzgit/docs.html#worktree) | worktree を一覧・管理する |

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
