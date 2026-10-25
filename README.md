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
| anywhere | `cmd-k` palette · `cmd-n` new workspace · `cmd-1…9` open the Nth workspace (hold `cmd` to see the numbers) · `ctrl-c` stop agent · `ctrl-w h/l` previous/next pane · `cmd-b` sidebar ↔ icon rail · `ctrl-cmd-v` Vim keys in the editor on/off · `cmd-/` model menu · `ctrl-cmd-1…5` default/fable/opus/sonnet/haiku |
| sidebar, transcript | `j/k` `gg` `G` `ctrl-d/u` · `enter` open / toggle tool output · `i`/`a` → composer |
| composer | `enter` send (queues while the agent runs) · `cmd-enter` send now, steering the running turn · `↑` on the first line → queue · `shift-enter` newline · `esc` → normal mode · `@` add a file or folder (`repo/` to pick the repo) · `/` skills and commands at the start · in the menu `↑/↓` `enter`/`tab` `esc` |
| queue | `j/k` `↑/↓` `gg` `G` · `enter` send now · `e` edit (`enter` puts it back) · `d`/`backspace` delete · `shift-k/j` or `alt-↑/↓` move up/down · `esc`/`i`, or `↓` past the last row → composer |
| leader (normal mode) | `space space` palette · `space q` queue · `space w n/d/D` new/archive/force-archive · `space r a` add repo · `space c/d` chat/diff · `space p r` create linked PRs · `space p o` open PRs · `space g g` next git step · `space g m` git menu · `space g f` fetch now · `space m m` model menu · `space m 1…5` pick model · `space o f/t` reveal in Finder / open in Terminal · `space y p/b` copy path / branch · `space b` sidebar rail · `space t` file tree · `space x` close file |
| new workspace | `enter` create (in branch) · `tab` branch ↔ repo filter · type to filter · `enter` tick match · `space` tick/untick highlighted · `cmd-enter` create · last-used repos start ticked |
| sidebar mouse | right-click a workspace: open, reveal in Finder / Terminal / copy path (per repo), copy branch, open PRs, archive |
| diff | `j/k/h/l` `gg` `G` `ctrl-d/u` scroll · `]c`/`[c` hunks · `}`/`{` files · `s` split/unified · `r` refresh · viewed checkbox in each file header |
| file tree | `j/k` `gg` `G` `ctrl-d/u` · `enter` show file / toggle folder · `h/l` collapse/expand · `v` viewed (on a folder: every file in it) · `o` open in editor |
| editor | `cmd-s` save · `cmd-w` close · `esc` back to the tree · with Vim on: motions `hjkl w b e 0 ^ $ gg G { }` with counts, `i a I A o O`, `x dd yy cc D C p P J r ~`, `d/c/y/>/<` + motion, `v`/`V`, `u` / `ctrl-r`, `/ n N`, `:w` |

## How it works
- **Workspace creation:**
  - For each repo, `git worktree add -b <branch> <ws>/<repo> origin/HEAD`, failing early if the branch is already checked out somewhere.
  - Gitignored files that match `.worktreeinclude` (default `.env*`) are copied into the new worktree.
  - A root `CLAUDE.md` is written that lists the repos and `@`-imports each repo's own CLAUDE.md.
- **Agent:**
  - One process per workspace: `claude -p --input-format stream-json --output-format stream-json`, with the workspace root as cwd.
  - Runs with `--permission-mode bypassPermissions`. Worktrees isolate branches, not the filesystem.
  - **Queue:** a message sent while a turn runs waits in a queue above the composer, top first. The top one goes out when a turn ends without error; a stop or an error leaves the queue alone, and so does editing one of its messages. Send now writes the message to claude's stdin mid-turn: claude reads it at its next tool call, or as a new turn if the reply has no more tool calls. The queue is in memory only.
  - The transcript is stored in `~/Library/Application Support/shark/transcripts/`, and `--resume` continues the session after a restart.
- **Diff:** one `git diff --merge-base <base>` per worktree, with paths prefixed by repo name. Untracked files are included by marking them intent-to-add in a throwaway copy of the index (`GIT_INDEX_FILE`), so the real index and the agent's `index.lock` are never touched.
- **Files tree and viewed:** the changed files show as a tree to the right of the diff (and of an open file), folders first, single-child folders joined (`src/ui`). Marking a file viewed stores a fingerprint of its patch in `state.json`; when the patch changes the file is unviewed again and gets a dot.
- **Editor:** `o` opens a changed file next to the chat and shrinks the sidebar to an icon rail. Saves are refused if the file changed on disk since it was read (the agent writes the same worktree); the file is re-read every 2 s and a clean buffer follows the disk. The Vim toggle is saved in `state.json`.
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
