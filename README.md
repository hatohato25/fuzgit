# fuzgit

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
  書き込み系の操作（`switch` / `cherry-pick` / `restore` / `add` / `stash` など）とプレビュー用の
  色付き差分生成は、システムの `git` コマンドへシェルアウトして実行します。
  リポジトリ情報の読み取りには [gix](https://crates.io/crates/gix) を使いますが、
  gix は上記の書き込み操作を提供していないためです。
  `git` が PATH 上に無い場合は「git コマンドが見つかりません」と表示して終了します。
- **`gz merge` のコンフリクト予測には Git 2.38 以降が必要です（任意）**
  予測には `git merge-tree --write-tree` を使います。2.38 未満の git ではこのオプションが
  拒否されるため、**予測の表示だけを省略して merge の実行はそのまま続けます**
  （確認プロンプトに「コンフリクト予測: 省略しました」と表示されます）。エラーで停止することはありません。
  これ以外の機能に git の最低バージョン要件はありません。
- ビルドする場合は stable の Rust ツールチェイン
  （edition 2024 を使うため Rust 1.85 以降。開発時の確認は 1.95.0）

## インストール

crates.io へは未公開のため、リポジトリを取得してローカルからインストールします。

```sh
git clone https://github.com/nasu-hayato/fuzgit.git
cd fuzgit
cargo install --path .
```

`~/.cargo/bin/gz` がインストールされます（パッケージ名は `fuzgit`、コマンド名は `gz`）。

インストールせずに試す場合:

```sh
cargo build --release
./target/release/gz --help
```

## 使い方

```sh
gz <サブコマンド> [オプション]
```

引数なしの `gz` および `gz --help` でサブコマンド一覧を表示します。

| サブコマンド | 概要 | 選択 |
|---|---|---|
| `gz branch` | ブランチを選んで切り替える（サブコマンドで作成・削除・整理も行う） | 単一／サブコマンド次第 |
| `gz log` | コミット履歴を辿り、フルハッシュを標準出力へ出す | 単一 |
| `gz cherry-pick` | コミットを選んで cherry-pick する | 複数 |
| `gz restore` | ファイルを選んで復元・アンステージする | 複数 |
| `gz add` | 未ステージ・未追跡ファイルを選んでステージする | 複数 |
| `gz stash <サブコマンド>` | 変更を stash へ退避し、stash を検索して適用・破棄する | サブコマンド次第 |
| `gz tag` | タグを選んで出力・切替・差分表示する | 単一 |
| `gz reflog` | HEAD の reflog を辿り、失われたコミットを取り出す | 単一 |
| `gz commit` | 変更ファイルを選んで、選んだものだけをコミットする | 複数 |
| `gz push` | push 先（リモート × 現在ブランチ）を選んで push する | 単一 |
| `gz fixup` | 修正対象のコミットを選んで fixup コミットを作る | 単一 |
| `gz merge` | merge するブランチを選ぶ（進行中は復帰メニュー） | 単一 |
| `gz rebase` | rebase の base を選ぶ（進行中は復帰メニュー） | 単一 |
| `gz revert` | 打ち消すコミットを選んで revert する | 複数 |
| `gz status` | 変更ファイルを一覧し、選んだファイルに操作を行う（2 段選択） | 複数 → 単一 |
| `gz diff` | 比較対象を選んで差分を表示する | モード次第 |
| `gz fetch` | リモートを選んで fetch する（**ネットワークを使う**） | 単一 |
| `gz sync` | 現在ブランチを upstream と同期する（**ネットワークを使う**） | 選択なし |
| `gz worktree` | worktree を一覧・管理する | 単一 |

### `gz branch` — ブランチの切替・作成・削除・整理

```sh
gz branch                  # ローカルブランチから選んで git switch（従来どおりの切替）
gz branch --all            # リモート追跡ブランチ（origin/... 等）も候補に含める
gz branch create <name>    # 作成元を選んで新しいブランチを作る
gz branch delete           # ブランチを選んで削除する
gz branch cleanup          # merged なブランチを一括で削除する
```

**引数なしの `gz branch` と `gz branch --all` は従来どおりのブランチ切替です。**
`create` / `delete` / `cleanup` は後から追加した管理操作で、切替用のフラグ（`-a` / `--all`）と
管理サブコマンドの併用は、どちらの操作を意図したのかが曖昧になるため clap の段階で拒否されます
（`gz branch --all create x` はエラー）。

#### 切替（サブコマンドなし）

| オプション | 説明 |
|---|---|
| `-a`, `--all` | リモート追跡ブランチも候補に含める |

- 現在のブランチは `git branch` と同じく行頭の `*` で識別できます。
- プレビューには選択中ブランチの直近 50 件のコミットログ（`git log --oneline --decorate`）を表示します。
- リモート追跡ブランチ（`origin/feature`）を選ぶと短縮名（`feature`）で `git switch` するため、
  git の DWIM により追跡ローカルブランチが作成されます。

#### `gz branch create <name>` — ブランチの作成

| 引数・オプション | 説明 |
|---|---|
| `<NAME>` | 作成するブランチ名（必須の位置引数） |
| `--switch` | 作成後にそのブランチへ切り替える |

- fuzzy finder で選ぶのは**作成元**です。候補はローカルブランチ・リモート追跡ブランチ・タグで、
  行頭に種別（`branch` / `tag`）が付きます（同名のブランチとタグが共存できるため）。
- 実行するのは `git branch -- <name> <作成元>` です。annotated tag を選んだ場合は解決済みの ID を渡し、
  git が対象コミットまで peel します。
- **ブランチ名は位置引数です**（skim にテキスト入力 UI が無いため。「スコープ外・非対応」参照）。
  名前の妥当性検証は git の `check-ref-format` に委ねており、`-` 始まりの名前は `--` で保護したうえで
  git が拒否します。
- `--switch` を付けない場合は作成のみを行い、切り替えるためのコマンドを標準エラーへ案内します。

#### `gz branch delete` — ブランチの削除

| オプション | 説明 |
|---|---|
| `--force` | merged でないブランチも削除する（`git branch -D`） |
| `--into <BRANCH>` | merged 判定の基準ブランチ（既定は HEAD） |

- 候補はローカルブランチのうち、**現在のブランチと、他の worktree でチェックアウト中のブランチを除いた**
  ものです（どちらも git が削除を許可しないため）。
- 候補行は `<名前>  merged|unmerged  <相対更新日時>  追跡: <upstream>|追跡なし` です。
  いずれも全件を一括取得できる情報に限っており、最終コミットの詳細はプレビュー（`git log --oneline`）で確認します。
- Tab で複数選択できます。**実行前に確認プロンプト（`[y/N]`）で対象を全件列挙します。**
- 既定は `git branch -d`（merged のみ削除可）です。**unmerged が選択に 1 件でも含まれると、
  `git branch` を実行する前に専用エラーで停止します**（一部だけ削除して止まることはありません）。
- `--force` を指定したときだけ `git branch -D` を使い、確認プロンプトに unmerged である旨の警告を含めます。

#### `gz branch cleanup` — merged なブランチの一括削除

| オプション | 説明 |
|---|---|
| `--into <BRANCH>` | merged 判定の基準ブランチ（既定は HEAD） |

- 候補を merged のブランチのみに絞り、**全件を起動時から選択済み**で表示します（Tab で外せます）。
- 確認プロンプトを経て `git branch -d` で一括削除します。`--force` はありません
  （unmerged を扱うのは `gz branch delete --force` の役目です）。

### `gz log` — コミット履歴の探索

```sh
gz log                 # 既定 1000 件を候補にする
gz log --limit 200     # 取得件数を制限する（-n でも可）
git show "$(gz log)"   # 選んだコミットのフルハッシュをそのまま使う
```

| オプション | 既定値 | 説明 |
|---|---|---|
| `-n`, `--limit <N>` | `1000` | 取得するコミットの最大件数 |

- 候補は `<短縮ハッシュ> <日付> <サマリ> (<作者>)` の 1 行で、この文字列がそのまま絞り込み対象です。
- プレビューは `git show --color=always`（コミットの詳細と差分）。
- 決定すると**選択コミットのフルハッシュだけ**を標準出力へ出します（パイプ・コピー用途）。
  メッセージ類は標準エラーへ出すため、`$(gz log)` で安全に受け取れます。

### `gz cherry-pick` — cherry-pick するコミットの選択

```sh
gz cherry-pick                     # 全ブランチのコミットを候補にする
gz cherry-pick --branch feature    # 対象ブランチを指定する
```

| オプション | 説明 |
|---|---|
| `-b`, `--branch <BRANCH>` | 対象ブランチ（未指定時は全ブランチのコミットを候補にする） |

- Tab で**複数選択**できます。選んだ順に関わらず、常に古い順（履歴順）に cherry-pick します。
- 候補件数の上限は `gz log --limit` の既定値と同じ 1000 件です。
- コンフリクト時は git のメッセージをそのまま表示して非ゼロ終了します。
  続行・中止は `git cherry-pick --continue` / `git cherry-pick --abort` で行ってください。

### `gz restore` — 復元・アンステージするファイルの選択

```sh
gz restore                       # 作業ツリーの変更を破棄する（確認あり）
gz restore --staged              # ステージ済みの変更をアンステージする
gz restore --source HEAD~1       # 指定リビジョンの内容で上書きする（確認あり）
```

| オプション | 説明 |
|---|---|
| `-s`, `--source <REV>` | 復元元のリビジョン。指定するとそのコミットのファイル一覧が候補になる |
| `--staged` | ステージ済みの変更をアンステージする |

- Tab で複数選択できます。候補には `git status` と同じ状態コードが付きます。
- プレビューは対象ファイルの差分（`--staged` 指定時は HEAD とインデックスの差分）。
- **作業ツリーを書き換える操作は実行前に確認プロンプト（`[y/N]`）を表示します。**
  `--staged` によるアンステージは作業ツリーを壊さないため確認しません。

### `gz add` — ステージするファイルの選択

```sh
gz add
```

- 候補は未ステージの変更と未追跡ファイル（既にステージ済みで差分の無いファイルは出ません）。
- Tab で複数選択できます。
- プレビューは追跡ファイルは差分、未追跡ファイルはファイルの内容（先頭 64KiB まで）。

### `gz stash` — stash の作成・検索・復元

`push` が選ぶのは作業ツリーの「ファイル」、`apply` / `pop` / `drop` が選ぶのは既存の「stash」です。
選択対象が異なるため、フラグではなくサブコマンドで分けています。
引数なしの `gz stash` はサブコマンド一覧のヘルプを表示します（どちらかへ暗黙に倒しません）。

```sh
gz stash push                      # 追跡済みの変更から選んで退避する
gz stash push -m "作業中"          # メッセージを付ける
gz stash push -u                   # 未追跡ファイルも候補に含める
gz stash apply                     # 選んだ stash を適用する（stash は残る）
gz stash pop                       # 適用して、その stash を取り除く
gz stash drop                      # 選んだ stash を破棄する（確認あり）
```

| サブコマンド | オプション | 説明 |
|---|---|---|
| `push` | `-m`, `--message <MESSAGE>` | stash に付けるメッセージ |
| `push` | `-u`, `--include-untracked` | 未追跡ファイルも候補に含める（既定は追跡済みの変更のみ） |
| `apply` / `pop` / `drop` | （なし） | stash 一覧から 1 件選ぶ |

- `push` は Tab で複数選択でき、**選んだファイルだけ**が退避されます（選ばなかった変更は作業ツリーに残ります）。
  ステージ済みの変更も退避対象になるため、プレビューは HEAD との差分（`git diff HEAD`）です。
- `apply` / `pop` / `drop` の候補は `stash@{n}: <メッセージ>` で、メッセージで絞り込めます。
  プレビューは `git stash show -p --color=always`。
- **`drop` は元に戻せないため、実行前に確認プロンプト（`[y/N]`）を表示します。**

### `gz tag` — タグの選択

```sh
gz tag                  # タグ名を標準出力へ出す（既定）
gz tag --switch         # 選んだタグへ detached HEAD で切り替える
gz tag --diff           # 選んだタグと HEAD の差分を表示する
```

| オプション | 説明 |
|---|---|
| `--switch` | 選択したタグへ detached HEAD で切り替える |
| `--diff` | 選択したタグと HEAD の差分を表示する |

- `--switch` と `--diff` は同時に指定できません。
- annotated tag は候補行にタグメッセージを併記し、プレビューにもタグメッセージを含めます。
- 既定では**タグ名だけ**を標準出力へ出します。

### `gz reflog` — 削除済みブランチの調査

```sh
gz reflog                            # 選んだエントリのコミットハッシュを標準出力へ出す
gz reflog --restore recovered        # 選んだコミットから新規ブランチ `recovered` を作成する
```

| オプション | 説明 |
|---|---|
| `--restore <NAME>` | 選択したコミットから指定名の新規ブランチを作成する |

- 候補は `<短縮ハッシュ> HEAD@{n}: <メッセージ>` で、`checkout: moving from ...` 等のメッセージで絞り込めます。
- 既定では**フルハッシュだけ**を標準出力へ出します。`--restore` の実行結果は標準エラーへ出るため、
  標準出力はパイプ用途のまま空けてあります。

### `gz commit` — コミットするファイルの選択

```sh
gz commit                      # 選んだファイルだけをコミットする（メッセージはエディタで入力）
gz commit -m "認証を修正する"   # メッセージを直接指定する（エディタを起動しない）
```

| オプション | 説明 |
|---|---|
| `-m`, `--message <MESSAGE>` | コミットメッセージ（省略時は git がエディタを起動する） |

- 候補はステージ済み・未ステージ・未追跡の変更を**1 つの一覧**にしたもので、
  行頭には `git status` と同じ状態コードが付きます。
- **ステージ済みの変更があるファイルは起動時から選択済み**です。Tab で選択を外せます
  （外した選択が絞り込みのたびに復活することはありません）。
- 実行するのは `git commit -- <paths>...` のパス指定コミットです。そのため
  **選ばなかった変更はステージ済みであってもコミットされず、ステージ状態のまま残ります。**
- 選んだ未追跡ファイルは、パス指定コミットの対象にできないため先に `git add` します。
- プレビューは HEAD との差分（`git diff --color=always HEAD -- <path>`。未追跡ファイルは内容）。
  パス指定コミットが記録するのは作業ツリーの内容であるため、index との差分ではなく HEAD との差分を表示します。
- 変更が 1 件も無い場合は「選択できる候補がありません」と表示して終了します。

#### エディタの設定に関する注意

`-m` を省略した場合、コミットメッセージの入力は `git commit`（＝ `EDITOR` / `GIT_EDITOR` /
`core.editor`）に委ねられます。**VS Code のような GUI エディタを終了待ちなしで設定していると、
エディタが即座に終了するため git はメッセージが空だと判断し、
`Aborting commit due to empty commit message.` でコミットが中止されます**
（これは素の `git commit` でも同じです）。終了を待つオプションを付けてください。

```sh
git config --global core.editor "code --wait"   # VS Code の例
```

`-m` でメッセージを指定すればエディタを起動しないため、この問題を回避できます。
コミットが失敗したときは、上記の内容をヒントとして標準エラーへ表示します。

### `gz push` — push 先の選択

```sh
gz push        # push 先を選んで git push <remote> <branch>
gz push -u     # 選んだ push 先を現在ブランチの upstream に設定する
```

| オプション | 説明 |
|---|---|
| `-u`, `--set-upstream` | push 先を現在ブランチの upstream として設定する |

- push 対象は**現在のブランチに固定**で、候補は「リモート × 現在のブランチ」です
  （`origin/main  ahead 2 / behind 1  (upstream)` のように表示）。
- リモート追跡参照がまだ無い候補（未 push のリモート）は `追跡参照なし` と表示します。
  現在のブランチの upstream に対応する候補には `(upstream)` が付きます。
- プレビューは push されるコミット（`git log --oneline <追跡参照>..HEAD`、最大 50 件）。
  追跡参照が無い候補は push で参照が新規作成されるため、HEAD から辿れるコミットを表示します。
- 候補一覧・プレビューはローカルの追跡参照だけを読むため**ネットワークを使いません**。
  実際に通信するのは決定後の `git push` だけです。
- `git push` は継承 stdio で実行するため、認証プロンプトや進捗表示は git のものがそのまま出ます。
- **force push（`--force` / `--force-with-lease`）は提供しません。**
  リモート履歴の破壊は確認プロンプトだけでは担保できないため、意図的にスコープ外としています
  （必要な場合は素の `git push --force-with-lease` を使ってください）。これらのフラグを渡すと
  未知の引数として拒否されます。
- upstream の設定は `-u` を指定したときだけ行い、暗黙に `branch.<name>.remote` を書き換えません。
- detached HEAD では push 対象のブランチが定まらないため、その旨を表示して終了します。
  リモートが 1 つも無い場合は `git remote add` を促します。

### `gz fixup` — fixup コミットの対象選択

```sh
gz fixup            # git commit --fixup=<選んだコミット>
gz fixup --squash   # git commit --squash=<選んだコミット>
```

| オプション | 説明 |
|---|---|
| `--squash` | fixup ではなく squash コミット（メッセージを結合する）を作成する |

- コミット対象は**ステージ済みの変更**です。ステージ済みの変更が無い場合は、
  finder を起動する前に「ステージ済みの変更がありません」と表示して終了します。
- 候補は HEAD からのコミット（既定 1000 件、`gz log --limit` の既定値と共通）。
  プレビューは `git show --color=always`。
- 実行後、履歴へ取り込むための手順を標準エラーへ表示します。
  **rebase は自動実行しません**（履歴改変はユーザーの明示操作に委ねます）。

  ```console
  ヒント: 作成したコミットを履歴へ取り込むには次を実行してください。
    git rebase -i --autosquash <選んだコミットのフルハッシュ>^
  ```

  選んだコミットが最初のコミット（親が無い）の場合は `<hash>^` を解決できないため、
  起点が `--root` になります（理由も併記します）。
- `--squash` は git がメッセージ本文の追記のためにエディタを起動します（`--fixup` は起動しません）。

### `gz merge` — merge するブランチの選択

```sh
gz merge             # 通常の merge（fast-forward できる場合は fast-forward）
gz merge --no-ff     # fast-forward できる場合でもマージコミットを作る
gz merge --squash    # 結果を作業ツリー・index へ反映するだけでコミットしない
gz merge --ff-only   # fast-forward できる場合のみ merge する
```

| オプション | 説明 |
|---|---|
| `--no-ff` | fast-forward できる場合でもマージコミットを作成する |
| `--squash` | マージ結果を作業ツリー・index へ反映するだけでコミットしない |
| `--ff-only` | fast-forward できる場合のみ merge する |

- 3 つのオプションは相互排他です（同時に指定すると選択を始める前に拒否されます）。
- merge 先は現在の位置（`HEAD`）に固定で、候補は**現在のブランチを除く**ローカルブランチと
  リモート追跡ブランチです。
- プレビューは取り込まれるコミット（`git log --oneline HEAD..<候補>`、最大 50 件）。
- **実行前に確認プロンプト（`[y/N]`）を表示します。** 取り込まれるコミット数・実際に実行する
  コマンド・コンフリクト予測を提示します。
- コンフリクト予測は `git merge-tree --write-tree` のドライラン（**Git 2.38 以降が必要**）です。
  予測できない場合（2.38 未満など）は「省略しました」と表示し、merge の実行は継続します。
  「予測できなかった」を「コンフリクトしない」として扱うことはありません。
- コンフリクトした場合は git のメッセージがそのまま表示され、非ゼロ終了します。
  解決は後述の**復帰メニュー**（`gz merge` を再実行）または素の git で行います。

### `gz rebase` — rebase の base の選択

```sh
gz rebase   # base を選んで git rebase <base>
```

- 候補は現在のブランチを除くローカルブランチとリモート追跡ブランチです
  （tag や任意コミットは候補に含めません）。
- プレビューは replay されるコミット（`git log --oneline <候補>..HEAD`、最大 50 件）。
- **履歴改変操作のため、実行前に確認プロンプト（`[y/N]`）を表示します。**
  replay されるコミット数と、コミットハッシュが変わることを提示します。
- コンフリクトした場合は git のメッセージがそのまま表示され、非ゼロ終了します。

### コンフリクト時の復帰メニュー（`gz merge` / `gz rebase` / `gz sync`）

merge / rebase が進行中の状態で `gz merge` / `gz rebase` / `gz sync` を実行すると、
通常のフローではなく**復帰メニュー**が表示されます。
メニューの内容は**進行中の操作の種類で決まります**（rebase 進行中に `gz merge` を実行しても
rebase のメニューが出ます）。

| 進行中 | メニュー項目 |
|---|---|
| merge | コンフリクトファイルを確認して解決済みにする / merge を再開する（`git merge --continue`）/ merge を中止する（`git merge --abort`） |
| rebase | コンフリクトファイルを確認して解決済みにする / rebase を再開する（`git rebase --continue`）/ 現在のコミットを飛ばす（`git rebase --skip`）/ rebase を中止する（`git rebase --abort`） |

- どの項目を選んでいても、プレビューには現在の状態（`git status --short --branch`）が出ます。
  未解決（`UU` 等）と解決済み（`M `）の区別をその場で確認できます。
- 「コンフリクトファイルを確認して解決済みにする」を選ぶと、コンフリクト中（unmerged）の
  ファイル一覧が出ます。プレビューには**コンフリクトマーカー（`<<<<<<<` 等）を含む差分**が表示され、
  Tab で選んだファイルを `git add`（＝解決済みとして stage）します。
  ファイルの編集そのものは各自のエディタで行ってください（fuzgit はエディタを起動しません）。
- 未解決のファイルが 1 件も無い状態でこの項目を選んだ場合は、continue で再開できる旨を表示して終了します。
- **中止（abort）はここまでの解決内容が失われるため、実行前に確認プロンプト（`[y/N]`）を表示します。**
- continue / skip は継承 stdio で実行するため、コミットメッセージの入力が必要な場合は
  git がエディタを起動します。
- cherry-pick / revert には復帰メニューを用意していません（git の
  `--continue` / `--abort` の案内に委ねます）。

### `gz revert` — 打ち消すコミットの選択

```sh
gz revert             # 選んだコミットを revert する（メッセージはエディタで確認）
gz revert --no-edit   # エディタを起動せず git の既定メッセージのままコミットする
```

| オプション | 説明 |
|---|---|
| `--no-edit` | エディタを起動せず、git の既定メッセージのままコミットする |

- 候補は HEAD からのコミット（既定 1000 件、`gz log --limit` の既定値と共通）。
  プレビューは `git show --color=always`。
- Tab で**複数選択**できます。選んだ順に関わらず、常に**新しい順**に revert します
  （古いコミットを先に打ち消すと、その上に積まれた後続の変更と衝突しやすいため。
  `gz cherry-pick` の古い順とはちょうど逆になります）。
- 複数件を選ぶと `--no-edit` を付けない限りエディタが件数分だけ開きます。
- **選択にマージコミット（親が 2 つ以上）が含まれる場合は、`git revert` を実行する前に停止します。**
  マージコミットの revert には `-m <parent-number>` の指定が必要ですが、
  mainline の選択は「選ぶ」価値が乗らないため非対応です。停止時には
  `git revert -m 1 <フルハッシュ>` の形でそのまま実行できるコマンドを提示します
  （暗黙に候補から除外することはしません）。
- コンフリクトした場合は git のメッセージをそのまま表示して非ゼロ終了します。
  続行・中止は `git revert --continue` / `git revert --abort` で行ってください。

### `gz status` — 状態ダッシュボード（2 段選択）

```sh
gz status
```

オプションはありません。**ファイルを選ぶ → アクションを選ぶ**の 2 段構成です。

1. **ファイル選択（複数選択）**
   - 候補はステージ済み・未ステージ・未追跡の変更を 1 つの一覧にしたもの（`gz commit` と同じ候補生成）で、
     行頭には `git status` と同じ状態コードが付きます。
   - ヘッダーに現在の状態を 1 行で表示します。

     ```console
     main  |  ahead 2 / behind 1  |  staged 2 / unstaged 2 / untracked 1 / stash 1
     ```

     upstream が設定されていない場合、ahead / behind の区画ごと省略します（detached HEAD では
     ブランチ名の代わりに `detached HEAD` と表示します）。ahead / behind はローカルのリモート追跡参照から
     算出するため**ネットワークを使いません**。
   - プレビューは状態別のセクションに分かれます。ステージ済みの変更があれば `staged`
     （`git diff --cached`）、未ステージの変更があれば `unstaged`（`git diff`）を表示し、
     該当しないセクションは git を実行しません。未追跡ファイルは内容をそのまま表示します。

2. **アクションメニュー（単一選択）**

   | 項目 | 実行内容 |
   |---|---|
   | 選択したファイルをステージする (git add) | `gz add` と同じ |
   | 選択したファイルの変更を破棄する (git restore) | `gz restore` と同じ（**確認プロンプトあり**） |
   | 選択したファイルを stash へ退避する (git stash push) | `gz stash push` と同じ（未追跡を選んだ場合は `--include-untracked` が付く） |
   | 選択したファイルをコミットする (git commit) | `gz commit` と同じ（未追跡ファイルは事前に `git add`、メッセージはエディタ） |
   | 選択したファイルのパスを標準出力へ出力する | パイプ用途。パス以外は出力しない |

   - 各アクションの実体は対応する既存コマンドと同じ実装であり、安全策（restore の確認プロンプト、
     commit の未追跡ファイルの事前 add など）もそのまま働きます。
   - メニューのプレビューには現在の状態（`git status --short --branch`）を表示します。

- 「パスを標準出力へ出力する」以外のメッセージ類はすべて標準エラーへ出すため、標準出力はパイプ用途に空いています。
- **変更が 1 件も無い場合はエラーにせず、終了コード 0 で終わります**（クリーンな状態の確認も status の用途のため）。
  この場合はヘッダー相当の情報を標準エラーへ出します。

  ```console
  $ gz status
  変更はありません（作業ツリーはクリーンです）
  main  |  staged 0 / unstaged 0 / untracked 0 / stash 0
  ```

### `gz diff` — 比較対象を選んで差分表示

```sh
gz diff              # 未ステージの変更（git diff と同じ）
gz diff --staged     # ステージ済みの変更（git diff --staged と同じ）
gz diff --head       # HEAD と作業ツリー（ステージ済みの変更も含む）
gz diff --upstream   # HEAD と upstream（リモート追跡参照）
gz diff --branch     # ブランチを 2 回選んで比較する
gz diff --commit     # コミットを 2 回選んで比較する
```

| オプション | 説明 |
|---|---|
| `--staged` | ステージ済みの変更を対象にする（`git diff --staged` と同じ） |
| `--head` | HEAD と作業ツリーを比較する（ステージ済みの変更を含む） |
| `--upstream` | HEAD と upstream を比較する |
| `--branch` | ブランチを 2 回選択して比較する |
| `--commit` | コミットを 2 回選択して比較する |

- 比較モードは相互排他です。フラグ名は git 本体の語彙に合わせています。
- `--branch` / `--commit` は比較元・比較先の順に fuzzy finder を**2 回**起動します。
  どちらを選んでいるかはヘッダー（`1/2 比較元のブランチ` / `2/2 比較先のブランチ`）で分かります。
  候補・プレビューはそれぞれ `gz branch` / `gz log` と共通です。
- 比較範囲が確定すると変更ファイル一覧が出るので、Tab で表示したいファイルを絞り込めます。
  ヘッダーには確定した比較範囲を表示し、プレビューは選択中ファイルに限定した色付き差分です。
- 決定すると `git diff <範囲> -- <pathspec>...` を継承 stdio で実行します（ページャ・色は git に委ねます）。
  リビジョン同士の比較は `<a>..<b>` ではなく `git diff <a> <b>` の 2 引数で渡します。
- `--upstream` は upstream が設定されていない場合・detached HEAD の場合に専用エラーで停止します。
  比較先はローカルのリモート追跡参照（`refs/remotes/<remote>/<branch>`）であり、**ネットワークを使いません**。
  最新の状態と比べたい場合は先に `gz fetch` を実行してください。
- **比較範囲に変更ファイルが 1 件も無い場合はエラーにせず、終了コード 0 で終わります。**

  ```console
  $ gz diff
  差分はありません（未ステージの変更: index と作業ツリー）
  ```

### `gz fetch` — リモートの取得

```sh
gz fetch            # リモートを選んで git fetch <remote>
gz fetch --prune    # リモートで削除されたブランチの追跡参照も掃除する
```

| オプション | 説明 |
|---|---|
| `--prune` | リモートで削除されたブランチの追跡参照を削除する（`git fetch --prune`） |

- 候補は登録済みのリモート名に、固定候補「すべてのリモート」を加えたものです。
  「すべてのリモート」を選ぶと `git fetch --all` を実行します。
- **プレビューはローカル情報だけを表示します**（`リモート URL` と `既知のリモート追跡ブランチ` の
  2 セクション）。一度も fetch していないリモートは追跡参照が無いため、そのセクションごと省略されます。
  プレビューでネットワークを使わないのは設計上の原則です（下記「ネットワーク操作について」参照）。
- `git fetch` は継承 stdio で実行するため、更新された参照の一覧・認証プロンプト・進捗表示は
  git のものがそのまま出ます。
- ネットワーク・認証の失敗は git のメッセージをそのまま表示して非ゼロ終了します
  （fuzgit 側でのタイムアウト・リトライは行いません）。
- リモートが 1 つも登録されていない場合は `git remote add` を促して終了します。

### `gz sync` — upstream との同期

```sh
gz sync            # fetch → fast-forward（既定）
gz sync --rebase   # fetch → upstream の上へ rebase（履歴改変）
gz sync --merge    # fetch → upstream を merge
```

| オプション | 説明 |
|---|---|
| `--rebase` | upstream の上へ rebase して取り込む（履歴改変） |
| `--merge` | upstream を merge して取り込む |

- 対象は**現在ブランチの upstream に固定**です。fuzzy finder は起動しません
  （選ぶ対象が無いため。任意のリモート・ブランチから取り込みたい場合は
  `gz fetch` のあとに `gz merge` / `gz rebase` を使ってください）。
- upstream が設定されていない場合・detached HEAD の場合は、**`git fetch` を実行する前に**
  専用エラーで停止します。upstream の `branch.<name>.remote` が登録済みのリモート名でない場合も
  同様に、ネットワークへ出る前に停止します。
- 処理の流れは `git fetch <remote>` →（追跡参照が更新された状態で）ahead / behind の再計算 →
  確認プロンプト → 取り込みの実行です。
- behind が 0 の場合は「最新です」と表示して**終了コード 0** で終わります。
- **既定は fast-forward のみ（`git merge --ff-only`）です。** fast-forward できない（diverged）場合は
  git の `fatal: Not possible to fast-forward, aborting.` をそのまま表示して停止し、
  **暗黙に merge / rebase へ倒すことはありません**。取り込み方法は `--rebase` / `--merge` で明示してください
  （2 つは相互排他）。
- **取り込みの実行前に確認プロンプト（`[y/N]`）を表示します。** 取り込まれるコミット数と実際に実行する
  コマンドを提示し、`--rebase` の場合は履歴改変になる旨も併記します。
- merge / rebase が進行中の状態で実行した場合は、前述の**復帰メニュー**が表示されます。
- `--prune` はありません。追跡参照の削除は `gz fetch --prune` として明示的に行う操作であり、
  同期のついでには行いません。

### `gz worktree` — worktree の一覧・管理

```sh
gz worktree                  # 一覧から選んでパスを標準出力へ出す
cd "$(gz worktree)"          # シェル連携（選んだ worktree へ移動する）
gz worktree add <path>       # ブランチを選んで新しい worktree を作る
gz worktree remove           # worktree を選んで削除する（確認あり）
gz worktree prune            # 実体を失った worktree の管理情報を整理する（確認あり）
```

#### 引数なし（一覧 → パス出力）

- 候補行は `<パス>  main|linked  <ブランチ|detached|bare>[  locked][  prunable]` です。
- プレビューには選択中 worktree の HEAD のコミットログを表示します
  （bare とコミットが 1 件も無い worktree はプレビューを出しません）。
- 決定すると**選んだ worktree のパスだけ**を標準出力へ出します。メッセージ類は標準エラーです。

> **注意（既知の挙動）**: 標準出力を**ファイルへリダイレクト**すると、skim の描画エスケープも
> そのファイルへ混ざります（`gz worktree > out.txt` / `gz log > log.txt` のいずれも同じ）。
> パイプ・コマンド置換（`$(gz worktree)`）では描画は端末側へ出るため、
> `cd "$(gz worktree)"` は期待どおり動作します。

#### `gz worktree add <path>`

| 引数 | 説明 |
|---|---|
| `<PATH>` | 作成する worktree のパス（必須の位置引数） |

- fuzzy finder で選ぶのはチェックアウトするブランチです。候補は**他の worktree で使用中でない**
  ローカルブランチに限ります（git は同じブランチを複数の worktree で同時にチェックアウトできないため）。
- 実行するのは `git worktree add -- <path> <branch>` です。**ディレクトリ名の自動提案は行いません。**
- `-` で始まるパスを指定する場合は `gz worktree add -- -dashy` のように書きます
  （clap 側の `--` であり、fuzgit は git へ渡す際にも別途 `--` で保護します）。

#### `gz worktree remove`

- 候補は linked worktree のみで、**main worktree は候補に含めません**（git が削除を許可しないため）。
- **実行前に確認プロンプト（`[y/N]`）を表示します。**
- locked な worktree・未コミット変更を含む worktree は、git のエラーをそのまま表示して非ゼロ終了します
  （`--force` は提供していません）。

#### `gz worktree prune`

- 先に `git worktree prune --dry-run --verbose` を実行し、整理される worktree と理由を
  確認プロンプトに提示します。承認された場合のみ `git worktree prune` を実行します。
- 対象が 1 件も無い場合は「整理する worktree はありません」と表示して**終了コード 0** で終わります。

## ネットワーク操作について

- **ネットワークを使うのは `gz fetch` と `gz sync` だけです**（`gz push` は決定後の `git push` のみ）。
  ほかのコマンドはすべてローカルのリポジトリ情報だけで動作します。
- **候補一覧の生成とプレビューではネットワークを使いません。** これは設計上の原則です。
  プレビューは選択項目が変わるたびに生成されるため、そこでネットワーク往復（遅延・タイムアウト・
  認証プロンプト）が発生すると描画がブロックされてしまいます。
  したがって `gz fetch` のプレビューはローカル情報（`git remote get-url` /
  `git for-each-ref refs/remotes/<remote>`）に限定しており、`git fetch --dry-run` を
  プレビュー内で実行することはしません。更新される参照の一覧は `git fetch` 自身が実行時に表示します。
- `gz status` の ahead / behind、`gz push` の ahead / behind、`gz diff --upstream` の比較先は、
  いずれもローカルのリモート追跡参照を読んだ結果です。最新の状態と比べたい場合は先に
  `gz fetch` を実行してください。
- **認証は git に委ねます。** `git fetch` / `git push` は継承 stdio で実行するため、
  認証情報の入力・credential helper・進捗表示はすべて git のものがそのまま動きます。
  fuzgit 側でのタイムアウト・リトライ・ソケットの直接操作は一切行いません。

## キー操作

fuzzy finder は skim の既定キーバインドをそのまま使います。

| キー | 動作 |
|---|---|
| 文字入力 | インクリメンタルに絞り込む |
| `↑` / `↓`、`Ctrl-p` / `Ctrl-n`、`Ctrl-k` / `Ctrl-j` | 候補を移動する |
| `Tab` / `Shift-Tab` | 候補の選択を切り替える（**複数選択モードのみ**） |
| `Enter` | 決定する |
| `Shift-↑` / `Shift-↓` | プレビューをスクロールする |
| `Esc` / `Ctrl-C` | 中断する |

- 複数選択に対応するのは `cherry-pick` / `restore` / `add` / `stash push` / `commit` /
  `revert` / `status`（ファイル選択）/ `diff`（ファイル選択）/ `branch delete` / `branch cleanup`、
  および復帰メニューの「コンフリクトファイルを確認して解決済みにする」です。
  `Tab` で 1 件も選ばずに `Enter` を押した場合は、カーソル位置の候補が対象になります。
- 起動時から選択済みになるのは `gz commit`（ステージ済みのファイル）と
  `gz branch cleanup`（merged なブランチ全件）です。いずれも `Tab` で外せます。
- 選択結果は常に候補一覧を基準に並べ直してから git へ渡します
  （skim が返す順序は選んだ順であり、候補の並び順とは限らないため）。
  `gz cherry-pick` は古い順、`gz revert` は新しい順というように、
  順序が意味を持つコマンドはそれぞれの向きへ揃えます。
- **`Esc` / `Ctrl-C` で中断した場合、git 操作は一切実行せず終了コード 130 で終了します。**

## 終了コード

| コード | 意味 |
|---|---|
| `0` | 正常終了（`gz status` の変更ゼロ、`gz diff` の差分ゼロ、`gz sync` の behind ゼロ、`gz worktree prune` の対象ゼロを含む） |
| `1` | エラー（リポジトリ外での実行、候補が 0 件、git コマンドの失敗など。メッセージは標準エラーへ） |
| `2` | コマンドライン引数が不正（引数なしの `gz` / `gz stash` を含む。clap がヘルプを表示） |
| `130` | fuzzy finder の中断（`Esc` / `Ctrl-C`）、または確認プロンプトでの否認。git 操作は実行されない |

git リポジトリ外で実行した場合は「git リポジトリではありません」と表示して非ゼロ終了します。

## スコープ外・非対応

意図的に実装していない機能です。いずれも素の git で実行できます。

| 項目 | 理由 |
|---|---|
| `gz pull` | 方式選択は clap フラグにする方針であり、残る選択（remote × branch）は実質 upstream の選択と同義。upstream 固定の `gz sync` に簡約した。任意のリモート・ブランチからの取り込みは `gz fetch` → `gz merge` / `gz rebase` で行える |
| force push（`gz push --force` / `--force-with-lease`） | リモート履歴の破壊は確認プロンプトだけでは担保できない |
| マージコミットの revert（`git revert -m`） | mainline の選択は履歴構造の理解を要し、fuzzy finder で「選ぶ」価値が乗らない。選択された場合は実行前に停止し、素の `git revert -m 1 <hash>` を案内する |
| `gz fetch` のプレビューでの `--dry-run` 実行 | プレビューは選択項目ごとに都度実行されるため、ネットワーク往復が描画をブロックする。更新された参照は `git fetch` 自身が実行時に表示する |
| `gz diff` の tag vs tag | 使用頻度が低く、tag vs HEAD は `gz tag --diff` で行える |
| `gz worktree` の move / lock / unlock / repair、`remove --force` | 日常頻度が低く、いずれも素の git で 1 コマンドで済む。未コミット変更の破棄は確認プロンプトだけでは担保しにくい |
| ブランチ名・worktree パスのテキスト入力 UI | skim にテキスト入力 UI は無い。名前・パスは位置引数（`gz branch create <name>` / `gz worktree add <path>`）とし、fuzzy 選択は作成元・ブランチに限定する。`gz worktree add` のディレクトリ名の自動提案も行わない |
| hunk 単位の部分 commit / stage | ファイル単位の選択で用途を満たす。hunk 選択は skim の候補モデル（行＝項目）に乗らない |
| `gz fixup` 後の autosquash rebase の自動実行 | 履歴改変の自動実行は行わない。手順を標準エラーへ表示するに留める |
| cherry-pick / revert / fetch の復帰メニュー | 復帰メニューは merge / rebase に限定する。cherry-pick / revert のコンフリクトは git の標準メッセージと `--continue` / `--abort` の案内に委ねる |
| コンフリクトファイルを「エディタで開く」 | git を介さず `$EDITOR` を直接起動する経路になり、シェル非経由方針との整合に別途設計判断が必要 |

## デバッグ

環境変数 `FUZGIT_DEBUG=1` を指定すると、fuzgit が実行した git コマンドを標準エラーへ出力します。

```console
$ FUZGIT_DEBUG=1 gz branch
[fuzgit] git log --color=always --oneline --decorate -n 50 feature --
[fuzgit] git switch feature
```

- 有効になるのは値が厳密に `1` の場合だけです。未設定・空文字・`0`・`true` などはすべて無効です。
- 出力先は必ず標準エラーです（標準出力はハッシュ・タグ名・パスのパイプ用途のために空けてあります）。
- ログにはコマンドの引数配列がそのまま並びます。プレビュー生成のたびに出力されるため、
  fuzzy finder の表示中は画面の描画と混ざって見えることがあります。
- 別ディレクトリを対象に実行するコマンドは `[fuzgit] (cwd: <ディレクトリ>) git ...` の形式で出力します。

## 開発

### 品質ゲート

以下の順にすべて成功することを、変更のたびに確認してください。

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo test
```

### テスト方針

- 端末（TUI）を占有する対話パスは自動テストの対象外とし、手動確認とします。
- 自動テストの対象は、候補の整形・git 引数の組み立て・`git status` / `git stash list` /
  `git worktree list` のパースなどの純ロジック（単体テスト）と、`--help`・リポジトリ外実行・
  候補 0 件などの非対話パス（`assert_cmd` による統合テスト）です。
- ネットワークを伴う経路（`gz fetch` / `gz sync`）は自動テスト化せず、非対話パス
  （リモートゼロ・upstream 未設定などのエラー）のみを検証します。実機確認はローカルの
  bare リポジトリを remote に設定して行い、外部ネットワークへは接続しません。

### 設計方針

- **読み取りは gix、書き込みは `git` へのシェルアウト**。gix は switch / cherry-pick / restore / add /
  stash の書き込み操作を提供していないため、挙動互換性を優先してシステムの git に委譲しています。
  worktree の一覧だけは gix ではなく `git worktree list --porcelain -z` を読みます
  （gix は main worktree とチェックアウト中のブランチを扱えないため）。
- 外部コマンドは常に `Command::new("git").args([...])` の**引数配列渡し**で実行し、シェルを一切経由しません。
  ユーザー由来のパスは `--` の後ろに `:(top,literal)<path>` のパススペックとして渡し、
  オプションやワイルドカードとして解釈される余地を排除しています。
- プレビューは skim にシェルコマンド文字列を渡す方式ではなく、Rust 側で git を実行した結果を
  表示する方式を採り、候補文字列由来のインジェクションを構造的に防いでいます。
- ブランチ名・コミットハッシュのように `--` で保護できない位置引数（`switch` / `merge` / `rebase` /
  `push` / `fixup` / `revert` / `diff`）は、**選択結果が候補一覧に含まれることを確かめてから** git へ渡します。
  渡る値は gix が列挙した実在の参照に由来するものだけです。
  `gz sync` は `branch.<name>.remote` が登録済みのリモート名であることを検証してから `git fetch` します
  （設定には URL を直接書けるため、検証しないと列挙していない対象へ接続することになります）。
- **候補生成・プレビューでネットワークアクセスを行いません。**
- 破壊的操作（`restore` による変更破棄、`stash drop`、`branch delete` / `cleanup`、
  `rebase` による履歴改変、`sync` の取り込み、`worktree remove` / `prune`、
  merge / rebase の `abort`）は実行前に確認を挟みます。`merge` にも確認を設け、
  取り込まれるコミット数とコンフリクト予測を提示します。
- 履歴改変の自動実行は行いません（`gz fixup` は autosquash の手順を表示するだけで rebase しません）。
- force push は提供しません（`gz push` に `--force` / `--force-with-lease` はありません）。

### モジュール構成

```
src/
├── main.rs          # エントリポイント（CLI パース → dispatch → 終了コード）
├── lib.rs
├── cli.rs           # clap derive によるコマンド定義
├── error.rs         # thiserror によるドメインエラー
├── finder.rs        # skim ラッパー（候補・プレビュー・中断判定）
├── git/
│   ├── repo.rs      # gix によるリポジトリのオープン
│   ├── read.rs      # gix / git による候補データの読み取り
│   └── exec.rs      # git の実行（run_git / capture_git）とデバッグログ
└── commands/        # 各サブコマンドのユースケース実装
```

## ライセンス

MIT License. 全文は [LICENSE](LICENSE) を参照してください。
