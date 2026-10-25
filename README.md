# shark

A keyboard-first, native (GPUI) app for running Claude Code on features that span **several repos**.

One workspace = one feature = N git worktrees on a shared branch, e.g. `~/shark/workspaces/eng-123-login/{ios,backend}`. A single agent works across all of them. The diff view is combined, and PRs are cross-linked.

## Run
```sh
cargo run                                  # needs Xcode (Metal); Rust is pinned in rust-toolchain.toml
cargo test                                 # git/worktree, stream-json parser, PR linking
cargo test --features test-support         # + headless keyboard UI test
cargo test live_claude -- --ignored        # real claude CLI round trip (costs a few cents)
```
Needs `git`, `gh` (logged in) and `claude` on your login shell's PATH.

## Keys
| Where | Keys |
|---|---|
| anywhere | `cmd-k` palette · `cmd-n` new workspace · `ctrl-c` stop agent · `ctrl-w h/l` sidebar/main · `cmd-/` model menu · `ctrl-cmd-1…5` default/fable/opus/sonnet/haiku |
| sidebar, transcript | `j/k` `gg` `G` `ctrl-d/u` · `enter` open / toggle tool output · `i`/`a` → composer |
| composer | `enter` send · `shift-enter` newline · `esc` → normal mode · `@` add a file or folder (`repo/` to pick the repo) · `/` skills and commands at the start · in the menu `↑/↓` `enter`/`tab` `esc` |
| leader (normal mode) | `space space` palette · `space w n/d/D` new/archive/force-archive · `space r a` add repo · `space c/d` chat/diff · `space p r` create linked PRs · `space p o` open PRs · `space g g` next git step · `space g m` git menu · `space g f` fetch now · `space m m` model menu · `space m 1…5` pick model · `space o f/t` reveal in Finder / open in Terminal · `space y p/b` copy path / branch |
| new workspace | `enter` create (in branch) · `tab` branch ↔ repo filter · type to filter · `enter` tick match · `space` tick/untick highlighted · `cmd-enter` create · last-used repos start ticked |
| sidebar mouse | right-click a workspace: open, reveal in Finder / Terminal / copy path (per repo), copy branch, open PRs, archive |
| diff | `j/k/h/l` `gg` `G` `ctrl-d/u` scroll · `]c`/`[c` hunks · `}`/`{` files · `s` split/unified · `r` refresh |

## How it works
- **Workspace creation:**
  - For each repo, `git worktree add -b <branch> <ws>/<repo> origin/HEAD`, failing early if the branch is already checked out somewhere.
  - Gitignored files that match `.worktreeinclude` (default `.env*`) are copied into the new worktree.
  - A root `CLAUDE.md` is written that lists the repos and `@`-imports each repo's own CLAUDE.md.
- **Agent:**
  - One process per workspace: `claude -p --input-format stream-json --output-format stream-json`, with the workspace root as cwd.
  - Runs with `--permission-mode bypassPermissions`. Worktrees isolate branches, not the filesystem.
  - The transcript is stored in `~/Library/Application Support/shark/transcripts/`, and `--resume` continues the session after a restart.
- **Diff:** `git diff --merge-base <base>` in each worktree plus untracked files, with paths prefixed by repo name.
- **Git button** (header, top right; code in `src/sync/`): shows the next step toward a merged PR for all repos at once, e.g. "Commit and push · api" or "Fix checks · web".
  - Steps that need judgment (commit, resolve conflicts, fix failing checks, address review, sync a diverged branch) are prompts to the agent; text in the composer is added to the prompt. Mechanical ones (pull `--ff-only`, push, force push with lease, create PRs, update branch, ready for review, merge, archive) run directly.
  - Order: git errors → conflicts / unfinished rebase or merge → detached or wrong branch → uncommitted changes → no origin → rewritten or diverged history → behind remote → create or reopen PR → unpushed → PR conflicts → failing checks → changes requested → base moved → draft → waiting (GitHub catching up, checks running, review) → merge → archive. Actionable steps outrank waits across repos.
  - Ahead/behind is measured against `origin/<branch>`, never `@{u}`; new worktrees use `--no-track` so they don't track their base.
  - Sync: `git --no-optional-locks status` every 3 s for the shown workspace (so it never takes the agent's `index.lock`), plus after each agent turn and on window focus. `git fetch --prune` every 3 min. `gh pr list --head` every 60 s, every 10 s while checks run or GitHub computes mergeability, every 5 min after a gh error (the last good PR state stays on screen). Network calls never prompt and time out.
  - Before running, the step re-reads git and GitHub and refuses if the state moved on. Every step that writes waits for the agent to be idle. Merge and force push confirm first; merge uses each repo's allowed method with `--match-head-commit`, and stops at the first failure.
- **PRs:**
  - For each repo with commits: push, then `gh pr create --fill`.
  - Each PR body then gets a "Related PRs" section, which is idempotent on re-runs.
  - Refuses to run while any repo has uncommitted changes. Only an open PR is reused; after a merge, new commits get a new PR.

Background and prior art: [docs/research.md](docs/research.md). UI theme, design rules and checklist: [docs/design.md](docs/design.md).
