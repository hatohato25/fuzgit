1. 変更ファイルを選んで commit

`gz commit`
変更ファイルを fuzzy finder で表示し、複数選択してから commit します。

常に選択画面を出して、

```
Staged Changes
[ ] src/main.rs
[x] src/git.rs
[x] README.md

Unstaged Changes
[ ] Cargo.toml
```

を分けて表示する設計です。既に stage されたものを勝手に外したり、未選択の変更まで commit したりしないことが重要です。

プレビューには各ファイルの diff を表示します。
ファイル単位で stage
選択したファイルだけ commit
選択しなかった変更は作業ツリーに残す
commit message を入力

2. fixup commit の対象を選ぶ

`gz fixup`
過去のコミットを fuzzy finder で検索し、選択したコミットに対して次を実行します。

```
git commit --fixup=<selected-commit>
```

「この修正は、前に作ったあのコミットに含めたい」という場面に非常に向いています。
さらに、以下も候補になります。

- fixup
- squash
- amend

interactive rebase の対象選択
commit メッセージと diff をプレビューできると、対象を間違えにくくなります。

3. push 先を選ぶ

`gz push`
push 可能な対象を一覧表示します。

```
origin/feature/login       3 commits ahead
origin/main                0 commits ahead
upstream/develop            2 commits ahead
```

選択した remote / branch に対して push します。

- remote の選択
- push 先ブランチの選択
- upstream 未設定ブランチへの設定
- fork の origin / upstream の切り替え

4. fuzgit merge
基本操作

`gz merge`
現在のブランチを固定し、merge 対象のブランチを選びます。

```
Current branch: feature/login

> feature/payment       +4 / -1
  origin/main           +12 / -3
  feature/session       +2 / -5
  release/v2.1
```

候補には以下を表示すると便利です。

- ブランチ名
- upstream
- ahead / behind
- 最終更新日時
- 最後の commit message
- 現在のブランチとの共通祖先

選択したブランチについて、右側に次をプレビューします。

- merge される commit 一覧
- diff の概要
- 変更ファイル
- コンフリクトの可能性

```
Merge feature/payment into feature/login?

4 commits, 8 files changed
Potential conflicts: src/auth.rs

Merge strategy:
> regular merge
  no-ff
  squash
  ff-only
```

gz merge では、次の選択に集中するのがよいです。

- merge 対象ブランチ
- merge 方式
- 必要なら merge commit のメッセージ

5. 通常の rebase

`fuzgit rebase`
現在のブランチを固定し、rebase 先の base を選びます。

```
Current branch: feature/login

> origin/main            124 commits, updated 2h ago
  origin/develop          38 commits, updated 1d ago
  release/v2.1
  8f21abc Fix API error handling
  1ab23cd Add authentication flow
```

選択後に、rebase の内容を表示します。

```
Rebase feature/login onto origin/main

Commits to replay:
* Add login API
* Add retry handling
* Fix timeout handling

Before:
origin/main - A - B - C
                    \
                     D - E - F  feature/login

After:
origin/main - A - B - C - D' - E' - F'
```

ここで fuzzy finder が役立つのは、base 候補を探す部分です。

- main
- develop
- release/*
- origin/main
- 特定の tag
- 特定の過去 commit

を、名前や commit message で検索できます。

6. コンフリクト発生時の UX
merge / rebase 中は、fuzzy finder を操作の入口ではなく、状態確認と復帰に使うとよいです。

merge 実行中なら次のメニューを出します。

```
Merge in progress

> show conflicted files
  preview conflict
  continue
  abort
```

rebase では、

```
Rebase in progress

> show conflicted files
  continue
  skip current commit
  abort
  show current commit
```

とします。
コンフリクトファイルの選択後に、

- diff を表示
- エディタで開く
- 解決済みとして stage
- git add 相当を実行

まで行えると、既存の gz add とも連携できます
