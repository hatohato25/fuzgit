# fuzgit

**English** | [日本語](README.ja.md)

A git CLI built around picking, searching, and tracing with a fuzzy finder.

Even when you don't remember the exact branch name or commit hash, you can complete everyday git
operations just by filtering and selecting. Every subcommand follows the same model:
**collect candidates → filter and select in a fuzzy finder (with preview) → run the git operation**.
The fuzzy finder is [skim](https://crates.io/crates/skim) embedded as a library, so no external
`fzf` / `sk` binary is required.

- Package name: **`fuzgit`**
- Executable (binary) name: **`gz`**

## Documentation

**For per-command details, key bindings, network operations, exit codes, and design notes, see the
documentation site.**

- **<https://hatohato25.github.io/fuzgit/>** — landing page
- **<https://hatohato25.github.io/fuzgit/docs.html>** — documentation (English / 日本語)

Note: the CLI's own messages, headers, and prompts are currently in Japanese, so the terminal
examples below show the actual Japanese output.

## Requirements

- **`git` must be installed on your system (required)**
  Write operations and colored diff generation for previews shell out to the system `git` command
  ([gix](https://crates.io/crates/gix) is used for reading repository information).
- **Git 2.38 or later for `gz merge` conflict prediction (optional)**
  On older versions only the prediction display is skipped; the merge itself still runs.
- A stable Rust toolchain if you build from source (Rust 1.85 or later, since it uses edition 2024)

## Installation

The crate is not published on crates.io yet, so clone the repository and install locally.

```sh
git clone https://github.com/hatohato25/fuzgit.git
cd fuzgit
cargo install --path .
```

This installs `~/.cargo/bin/gz` (package name `fuzgit`, command name `gz`).

To try it without installing:

```sh
cargo build --release
./target/release/gz --help
```

## Quick start

```sh
gz branch              # pick a branch and switch to it
gz status              # pick changed files, then add / restore / stash / commit them
git show "$(gz log)"   # pick a commit and get its full hash
```

Running `gz` with no arguments, or `gz --help`, lists the subcommands.

## Highlights

Four of the 20 commands that best show what fuzgit is about.

### Search and pick

**[`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) — pick a branch and switch to it**

You don't need to remember the exact branch name; just filter and select. The preview shows the last
50 commits of the highlighted branch (`git log --oneline --decorate`). With `--all`, remote-tracking
branches are included as candidates, and selecting `origin/feature` creates a tracking local branch
through git's DWIM behavior.

```
$ gz branch --all
> * main
    feature/login
    origin/feature/search
```

**[`gz stash`](https://hatohato25.github.io/fuzgit/docs.html#stash) — search stashes and restore them**

Candidates for `apply` / `pop` / `drop` are shown as `stash@{n}: <message>`, so you can filter by
message instead of by number. The preview is `git stash show -p --color=always`, and `drop` asks for
confirmation (`[y/N]`) before running. `gz stash push` supports multi-select with `Tab`, and stashes
**only the files you picked** (unselected changes stay in the working tree).

### Pick many, run once

**[`gz fetch -s`](https://hatohato25.github.io/fuzgit/docs.html#fetch) — fetch neighboring repositories too**

With `-s` / `--siblings`, fuzgit scans only the directory directly above the current worktree root
(no recursion) and offers every directory containing a `.git` as a candidate. Each line is
`<directory name>  <current branch>  <remote>`, and the current repository starts out selected.
Multi-select with `Tab` to fetch several repositories at once. Repositories that can't be fetched are
not silently dropped — the number excluded is shown in the header.

```
$ gz fetch --siblings
現在のリポジトリを選択済みにしています。Tab: 選択の切替 / Enter: 取得  |  除外 1 件（リモート未登録 / bare）
>>mike  main  origin
  alpha  main  origin
  zulu  main  origin
```

**[`gz pull`](https://hatohato25.github.io/fuzgit/docs.html#pull) — bring several branches up to date at once**

The only thing you pick is which local branches should follow their upstream, and integration is
fast-forward only. The current branch starts out selected. Branches run one at a time in list order;
a failure doesn't abort the run, and the successes and failures are tallied at the end. Branches that
can't be targeted (for example, no upstream configured) are reported as an excluded count in the
header.

```
$ gz pull
[1/4] main
[2/4] alpha
[3/4] diverged
[4/4] zeta
成功 3 件 / 失敗 1 件（失敗: diverged）
```

## Commands

| Subcommand | Description |
|---|---|
| [`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) | Pick a branch and switch to it (subcommands also create, delete, and tidy up) |
| [`gz log`](https://hatohato25.github.io/fuzgit/docs.html#log) | Trace commit history and print the full hash to stdout |
| [`gz cherry-pick`](https://hatohato25.github.io/fuzgit/docs.html#cherry-pick) | Pick a commit and cherry-pick it |
| [`gz restore`](https://hatohato25.github.io/fuzgit/docs.html#restore) | Pick files to restore or unstage |
| [`gz add`](https://hatohato25.github.io/fuzgit/docs.html#add) | Pick unstaged and untracked files to stage |
| [`gz stash <subcommand>`](https://hatohato25.github.io/fuzgit/docs.html#stash) | Stash changes, then search stashes to apply or drop them |
| [`gz tag`](https://hatohato25.github.io/fuzgit/docs.html#tag) | Pick a tag to print, switch to, or diff |
| [`gz reflog`](https://hatohato25.github.io/fuzgit/docs.html#reflog) | Trace the HEAD reflog and recover lost commits |
| [`gz commit`](https://hatohato25.github.io/fuzgit/docs.html#commit) | Pick changed files and commit only those |
| [`gz push`](https://hatohato25.github.io/fuzgit/docs.html#push) | Pick a push target (remote × current branch) and push |
| [`gz fixup`](https://hatohato25.github.io/fuzgit/docs.html#fixup) | Pick the commit to amend and create a fixup commit |
| [`gz merge`](https://hatohato25.github.io/fuzgit/docs.html#merge) | Pick a branch to merge (resume menu while one is in progress) |
| [`gz rebase`](https://hatohato25.github.io/fuzgit/docs.html#rebase) | Pick the rebase base (resume menu while one is in progress) |
| [`gz revert`](https://hatohato25.github.io/fuzgit/docs.html#revert) | Pick a commit to revert |
| [`gz status`](https://hatohato25.github.io/fuzgit/docs.html#status) | List changed files and act on the ones you pick (two-step selection) |
| [`gz diff`](https://hatohato25.github.io/fuzgit/docs.html#diff) | Pick what to compare and show the diff |
| [`gz fetch`](https://hatohato25.github.io/fuzgit/docs.html#fetch) | Choose what to fetch (`--siblings` fetches neighboring repositories too. **uses the network**) |
| [`gz pull`](https://hatohato25.github.io/fuzgit/docs.html#pull) | Pick branches and bring them up to their upstream at once (fast-forward only. **uses the network**) |
| [`gz sync`](https://hatohato25.github.io/fuzgit/docs.html#sync) | Sync the current branch with its upstream (**uses the network**) |
| [`gz worktree`](https://hatohato25.github.io/fuzgit/docs.html#worktree) | List and manage worktrees |

Options, how candidates are built, what the preview shows, and whether a confirmation prompt appears
are all described in the [documentation](https://hatohato25.github.io/fuzgit/docs.html).

## Development

After every change, make sure all of the following succeed, in this order.

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo test
```

- Testing policy, design notes, and module layout:
  [documentation](https://hatohato25.github.io/fuzgit/docs.html#development)
- The documentation site's source lives in `docs/` (GitHub Pages publishes `docs/` from the `main` branch)

## License

MIT License. See [LICENSE](LICENSE) for the full text.
