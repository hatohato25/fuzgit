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

## Requirements

- **`git` must be installed on your system (required)**
  Write operations and colored diff generation for previews shell out to the system `git` command
  ([gix](https://crates.io/crates/gix) is used for reading repository information).
- **Git 2.38 or later for `gz merge` conflict prediction (optional)**
  On older versions only the prediction display is skipped; the merge itself still runs.
- A stable Rust toolchain if you build from source (Rust 1.85 or later, since it uses edition 2024)

## Installation

### Homebrew (recommended)

```sh
brew tap hatohato25/fuzgit
brew trust --formula hatohato25/fuzgit/fuzgit
brew install hatohato25/fuzgit/fuzgit
```

The formula is named `fuzgit`, but **the installed command is `gz`**.

```sh
gz --version
```

Prebuilt binaries are provided for macOS (Apple Silicon / Intel) and Linux (x86_64).

### Linux / WSL (install script)

Homebrew is the macOS route; on Linux and WSL use the install script instead.

```sh
curl -fsSL https://raw.githubusercontent.com/hatohato25/fuzgit/main/install.sh | sh
```

It downloads the release tarball for your machine, **verifies its SHA-256
checksum**, and installs `gz` into `~/.local/bin` — no `sudo`, nothing written
outside that directory. If `~/.local/bin` is not on your `PATH`, the script says
so and prints the line to add; it does not edit your shell profile for you.

Options:

```sh
# Install a specific release instead of the latest
curl -fsSL .../install.sh | sh -s -- --version v0.5.0

# Install somewhere else (may need sudo depending on the directory)
curl -fsSL .../install.sh | sh -s -- --bin-dir /usr/local/bin
```

`FUZGIT_VERSION` and `FUZGIT_BIN_DIR` do the same thing as the two flags.

Only **x86_64** is published for Linux today. On `aarch64` the script stops and
tells you to build from source rather than installing a binary that cannot run.

> Piping a script into a shell means running code you have not read. The script
> is [`install.sh`](install.sh) in this repository — read it first if you would
> rather, then run it locally.

### From source

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
gz log --action        # pick a commit, then pick what to do with it
```

Running `gz` with no arguments, or `gz --help`, lists the subcommands.

### Carrying on from the commit you found

`gz log` and `gz reflog` print a full hash and stop, which is what makes `$(gz log)` work.
Add `--action` and they show a second menu instead — show the commit, switch to it as a
detached HEAD, cherry-pick, revert, create a fixup commit, or (from `gz reflog`) reset the
current branch back to it after a `y/N` confirmation. Printing the hash is one of the menu
entries, so you can still get back to the piping workflow.

**Without `--action` nothing changes**: the default output is exactly what it has always
been, so `git show "$(gz log)"` keeps working. `gz reflog --restore <NAME>` is not deprecated
either — use `--restore` when the operation needs a name you have to type, and `--action`
for everything that does not. The two cannot be combined.

## Language

Messages, prompts, finder headers, and `--help` come in **English and Japanese**. The default is
English; switch to Japanese with:

```sh
git config --global fuzgit.lang ja   # persistent. --local sets it per repository
gz --lang ja branch                  # one-off, on any subcommand
```

The display language is resolved in this order, and the first layer that decides it wins.

| Priority | Source |
|---|---|
| 1 | `--lang <ja\|en\|auto>` (global option, available on every subcommand) |
| 2 | `FUZGIT_LANG` environment variable |
| 3 | `git config fuzgit.lang` (system / global / local / worktree all apply as usual) |
| 4 | `LC_ALL` → `LC_MESSAGES` → `LANGUAGE` → `LANG` |
| 5 | fallback: `en` |

Layers 1-3 are explicit instructions to fuzgit, so any value other than `ja` / `en` / `auto` stops
with an error. Layer 4 only describes the environment, so a value fuzgit cannot interpret
(including `C` and `POSIX`) is not an error — resolution just moves on to the fallback. `auto`
skips the remaining explicit layers and resolves from the environment. fuzgit has no configuration
file of its own; it borrows git's `fuzgit.lang` key, which is also readable outside a repository.

Two limits are worth knowing:

- **Messages from git itself are not guaranteed to be translated.** fuzgit tells the git commands
  it runs which language to speak, but whether a catalog exists depends on how git was built (NLS)
  and on the installed locale data. In particular **git upstream ships no Japanese catalog**, so
  git's own output stays English even when you pick `ja`.
- **Text that clap prints on its own (`Usage:`, `Options:`, `Commands:`, parser errors) stays in
  English**, because clap 4 has no localization hook. fuzgit's own descriptions in `--help` do
  switch.

## Highlights

Five of the 18 commands that best show what fuzgit is about.

### Search and pick

**[`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) — pick a branch and switch to it**

You don't need to remember the exact branch name; just filter and select. The preview shows the last
50 commits of the highlighted branch (`git log --oneline --decorate`). With `--all`, remote-tracking
branches are included as candidates, and selecting `origin/feature` creates a tracking local branch
through git's DWIM behavior.

```
$ gz branch --all
>  * main
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
`<directory name>  <remote>/<current branch>`, and the current repository starts out selected.
Multi-select with `Tab` to fetch several repositories at once. Repositories that can't be fetched are
not silently dropped — the number excluded is shown in the header.

```
$ gz fetch --siblings
The current repository is preselected. Tab: toggle the selection / Enter: fetch  |  1 excluded (no remote / bare)
>> mike   origin/main
   alpha  origin/main
   zulu   origin/main
```

The selected repositories are fetched **in parallel** — the wait is network round trips, and the
targets are separate repositories. Four run at a time by default; `git config fuzgit.fetchJobs <n>`
changes that, and `1` restores the fully serial behaviour. **The setting applies to
`gz fetch --siblings` only**; plain `gz fetch` and `gz pull` fetch once and have nothing
to parallelise.

Each repository's output is captured and printed as one block when it finishes, so the update tables
never interleave. The parallel phase cannot prompt you for anything, so **anything that needs a
password or passphrase is run again afterwards, one at a time, with the terminal attached** — that
second run is not a retry, just a different way of running it. If you set `core.sshCommand` in git
config, the parallel phase overrides it and every target falls through to that serial pass; the
result is still correct, you just don't get the speedup.

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
3 succeeded / 1 failed (failed: diverged)
```

Branches run one at a time here, and that is deliberate: the per-remote fetch is effectively a single
call (most repositories track one upstream remote), and integrating branches writes to a single
repository, where the index and ref locks would collide.

**Desktop notification when a long run finishes.** Both `gz fetch --siblings` and `gz pull` can tell
you when they are done, which helps when you have stepped away.

```sh
git config --global fuzgit.notify true
```

It is **off unless you turn it on**, and it stays quiet for runs shorter than ten seconds. The body
is the count only — no repository, branch or path names. Whether a banner actually appears depends on
your environment (macOS uses `osascript` and needs notifications allowed for your terminal; Linux
uses `notify-send`, which may not be installed). fuzgit never treats that as a failure, and the
tally is always written to stderr regardless, so **the notification is a convenience, never the only
way you learn the result**.

## Commands

| Subcommand | Description |
|---|---|
| [`gz branch`](https://hatohato25.github.io/fuzgit/docs.html#branch) | Pick a branch and switch to it (subcommands also create, delete, and tidy up) |
| [`gz log`](https://hatohato25.github.io/fuzgit/docs.html#log) | Trace commit history and print the full hash to stdout (`--action` to pick what to do next) |
| [`gz cherry-pick`](https://hatohato25.github.io/fuzgit/docs.html#cherry-pick) | Pick a commit and cherry-pick it |
| [`gz restore`](https://hatohato25.github.io/fuzgit/docs.html#restore) | Pick files to restore or unstage |
| [`gz add`](https://hatohato25.github.io/fuzgit/docs.html#add) | Pick unstaged and untracked files to stage |
| [`gz stash <subcommand>`](https://hatohato25.github.io/fuzgit/docs.html#stash) | Stash changes, then search stashes to apply or drop them |
| [`gz reflog`](https://hatohato25.github.io/fuzgit/docs.html#reflog) | Trace the HEAD reflog and recover lost commits (`--action` to pick what to do next) |
| [`gz commit`](https://hatohato25.github.io/fuzgit/docs.html#commit) | Pick changed files and commit only those |
| [`gz fixup`](https://hatohato25.github.io/fuzgit/docs.html#fixup) | Pick the commit to amend and create a fixup commit |
| [`gz merge`](https://hatohato25.github.io/fuzgit/docs.html#merge) | Pick a branch to merge (resume menu while one is in progress) |
| [`gz rebase`](https://hatohato25.github.io/fuzgit/docs.html#rebase) | Pick the rebase base (resume menu while one is in progress) |
| [`gz revert`](https://hatohato25.github.io/fuzgit/docs.html#revert) | Pick a commit to revert |
| [`gz status`](https://hatohato25.github.io/fuzgit/docs.html#status) | List changed files and act on the ones you pick (two-step selection) |
| [`gz diff`](https://hatohato25.github.io/fuzgit/docs.html#diff) | Pick what to compare and show the diff |
| [`gz fetch`](https://hatohato25.github.io/fuzgit/docs.html#fetch) | Choose what to fetch (`--siblings` fetches neighboring repositories too. **uses the network**) |
| [`gz pull`](https://hatohato25.github.io/fuzgit/docs.html#pull) | Pick branches and bring them up to their upstream at once (fast-forward only. **uses the network**) |
| [`gz worktree`](https://hatohato25.github.io/fuzgit/docs.html#worktree) | List and manage worktrees (`add <name>` creates it next to the repository and copies `.claude/` into it) |
| [`gz pr`](https://hatohato25.github.io/fuzgit/docs.html#pr) | Pick a GitHub pull request and check it out (`--worktree <name>` opens it in a review worktree. **needs the `gh` CLI. uses the network**) |

Options, how candidates are built, what the preview shows, and whether a confirmation prompt appears
are all described in the [documentation](https://hatohato25.github.io/fuzgit/docs.html).

### Pull requests, if you have the gh CLI

**[`gz pr`](https://hatohato25.github.io/fuzgit/docs.html#pr) — pick a pull request and check it out**

The one command that reaches outside git. It lists your open pull requests, you filter and
pick one, and it runs `gh pr checkout`. What you end up with is ordinary local git state: a
branch, its tracking config, and a checkout.

```
$ gz pr
Fetching the open pull requests from GitHub
>  #142  fix/login-redirect   octocat           Fix the redirect after login
   #139  feat/search-filters  contributor       Add filters to the search form
   #131  chore/bump-deps      app/dependabot    chore(deps): bump the actions group
```

The preview is the pull request body, and it is **already in memory** — the candidate list is
fetched once with the body riding along, so moving the cursor never touches the network.
That is the whole reason this command fits fuzgit: everywhere else, candidate lists and
previews are built from local data only, and `gz pr` breaks that rule exactly once.

`--checks` adds the review decision and the CI status to each line. It is off by default
because it is slow: measured against a repository with 30 open PRs, the default fetch takes
about 1.1s while `--checks` takes roughly three times that.

`--action` opens a second menu after you pick — show the pull request, show the diff, print
the number, print the URL — the same shape as `gz log --action`.

**`gz pr --worktree <name>` opens the pull request in its own worktree**, next to the
repository root, and then does what `gz worktree add` does afterwards: copies your
gitignored `.claude/` across and installs dependencies from the lockfile it finds. Reviewing
a pull request no longer disturbs the tree you are working in.

```sh
gz pr --worktree review-142              # check PR out into ../review-142, install deps
gz pr --worktree review-142 --no-install # skip the dependency install
```

**`gh` is not a required dependency.** fuzgit still assumes only `git`. Without `gh`,
`gz pr` stops with a message pointing at https://cli.github.com and every other command
keeps working. One thing to know: **`gh` speaks English only**, so its lines stay English
even with `--lang ja` — the same limitation as git's own messages.

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
