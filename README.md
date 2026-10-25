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
| anywhere | `cmd-k` palette · `cmd-n` new workspace · `ctrl-c` interrupt the agent's turn, again to stop the process (not in the editor) · `ctrl-w h/l` previous/next pane · `cmd-b` sidebar ↔ icon rail · `ctrl-cmd-v` Vim keys in the editor on/off · `cmd-/` model menu · `ctrl-cmd-1…5` default/fable/opus/sonnet/haiku · `cmd-1…9` open the Nth workspace · `cmd-j` next workspace that needs you · `cmd-p` open any file |
| sidebar | `j/k` `gg` `G` · `enter` open |
| transcript | `j/k` `gg` `G` `ctrl-d/u` · `enter`/`o` toggle tool output · `i`/`a` → composer |
| composer | `enter` send (while the agent works: queue) · `shift-enter` newline · `esc` → normal mode · `@` add a file or folder (`repo/` to pick the repo) · `/` skills and commands at the start · in the menu `↑/↓` `enter`/`tab` `esc` |
| leader (normal mode) | `space space` palette · `space w n/d/D` new/archive/force-archive · `space w a` add a repo to the workspace · `space w j` next workspace that needs you · `space r a` add repo · `space c/d` chat/diff · `space p r` create linked PRs · `space p d` create draft PRs · `space p o` open PRs · `space p k/j` move a repo earlier/later in the merge order · `space g g` next git step · `space g m` git menu · `space g f` fetch now · `space m m` model menu · `space m 1…5` pick model · `space o f/t` reveal in Finder / open in Terminal · `space y p/b` copy path / branch · `space b` sidebar rail · `space t` file tree · `space x` close file · `space f f` open any file (fuzzy) |
| new workspace | `enter` create (in branch or first message) · `tab` branch → repo filter → first message · type to filter · `enter` tick match · `space` tick/untick highlighted · `cmd-enter` create · last-used repos start ticked |
| sidebar mouse | right-click a workspace: open, reveal in Finder / Terminal / copy path (per repo), copy branch, open PRs, archive |
| diff | `j/k` `gg` `G` `ctrl-d/u` move the line cursor (kept across refreshes) · `h/l` scroll sideways · `]c`/`[c` changes · `}`/`{` files · `v` viewed / `o` open the cursor's file · `c` comment on the cursor line or selected lines (edit: `c` again; empty removes) · `shift-c` send all comments to the agent · `s` split/unified · `r` refresh · viewed checkbox in each file header |
| file tree | `j/k` `gg` `G` `ctrl-d/u` · `enter` show file / toggle folder · `h/l` collapse/expand · `v` viewed · `o` open in editor |
| editor | `cmd-s` save · `cmd-w` close · `esc` back to the tree · with Vim on: motions `hjkl w b e 0 ^ $ gg G { }` with counts, `i a I A o O` (`esc`/`ctrl-c` back to normal), `x dd yy cc D C p P J r ~`, `d/c/y/>/<` + motion, `v`/`V`, `u` / `ctrl-r`, `/ n N`, `:w`, and in Normal mode the leader keys (`space x`, `space f f`, …) |

## How it works
- **Workspace creation:**
  - For each repo, `git worktree add --no-track -b <branch> <ws>/<repo> origin/HEAD`. Every repo is checked first, so a refusal leaves nothing behind: the branch is already checked out somewhere, or a local branch of that name exists that origin doesn't have (usually left from an archived workspace; pick another name or rename it). Offline or without an origin, the local branch is used.
  - A branch that only origin has starts at `origin/<branch>` (still `--no-track`); one both have uses the local branch.
  - The dialog's optional first message is sent to the agent once the workspace is open.
  - **Add repo to workspace** (`space w a`) adds a worktree for another repo on the same branch the same way, rewrites the root `CLAUDE.md`, and the agent is told about it with the next message.
  - Gitignored files that match `.worktreeinclude` (default `.env*`) are copied into the new worktree.
  - A root `CLAUDE.md` is written that lists the repos and `@`-imports each repo's own CLAUDE.md.
- **Restart:** the open workspace and tab (Chat or Diff) are saved in `state.json` and reopened at launch.
- **Archive:** checks every repo up front (uncommitted or untracked files, locked worktrees, submodules), so it can't stop halfway. Force-archive removes dirty worktrees and submodules but not locked ones. Messages to a workspace are refused while it is being archived.
- **Agent:**
  - One process per workspace: `claude -p --input-format stream-json --output-format stream-json`, with the workspace root as cwd.
  - Runs with `--permission-mode bypassPermissions`. Worktrees isolate branches, not the filesystem.
  - `ctrl-c` sends an `interrupt` control request: claude ends the turn and the process stays warm with its background tasks. If the turn hasn't ended after 5 s, or on a second `ctrl-c`, the process gets SIGTERM; when background tasks would die with it, only the second `ctrl-c` sends it. With only background tasks left, `ctrl-c` stops the process.
  - A message sent while the agent works goes to claude's stdin right away and shows as queued; claude reads it at its next step, which can steer the running turn. `--replay-user-messages` makes claude echo each message when it reads it, which clears the label; the echo is what the transcript stores.
  - The transcript is stored in `~/Library/Application Support/shark/transcripts/` (tool output clipped to 20k characters, each tool-input string to 2k), and `--resume` continues the session after a restart. If claude no longer has the session, shark drops it and the next message starts a fresh one. The palette's "New conversation" drops it by hand. History is read and parsed off the UI thread and goes ahead of anything sent while it loads; a turn without a result (shark quit or claude crashed) is marked "interrupted".
- **Diff:** one `git diff --merge-base <base>` per worktree, with paths prefixed by repo name. Untracked files are included by marking them intent-to-add in a throwaway copy of the index (`GIT_INDEX_FILE`), so the real index and the agent's `index.lock` are never touched.
- **Files tree and viewed:** the changed files show as a tree to the right of the diff (and of an open file), folders first, single-child folders joined (`src/ui`). Marking a file viewed stores a fingerprint of its patch in `state.json`; when the patch changes the file is unviewed again and gets a dot.
- **Editor:** `o` opens a changed file next to the chat and shrinks the sidebar to an icon rail. Saves are refused if the file changed on disk since it was read (the agent writes the same worktree); the file is re-read every 2 s and a clean buffer follows the disk. The Vim toggle is saved in `state.json`. "Open file…" (`space f f`) fuzzy-picks any tracked or untracked file of the workspace's repos (`git ls-files`), not only changed ones.
- **Diff cursor and comments:** the diff keeps its scroll offset private and resets it on every refresh, so `j/k` move a line cursor over shark's own model of the diff's rows and scroll by putting a row at the top; a refresh puts the same line back. Mouse scrolling drops the model until the next `gg`, `G`, `}`/`{` or tree jump. Comments show under their line, are kept per workspace in `~/Library/Application Support/shark/comments/`, and `shift-c` sends them as one prompt (`path:line > comment`, `(old)` for removed lines) and clears them; while the agent runs they stay.
- **Git button** (header, top right; code in `src/sync/`): shows the next step toward a merged PR for all repos at once, e.g. "Commit and push · api" or "Fix checks · web".
  - Steps that need judgment (commit, resolve conflicts, fix failing checks, address review, sync a diverged branch) are prompts to the agent; text in the composer is added to the prompt. Mechanical ones (pull `--ff-only`, push, force push with lease, create PRs, update branch, ready for review, merge, archive) run directly.
  - Order: git errors → conflicts / unfinished rebase or merge → detached or wrong branch → uncommitted changes → no origin → rewritten or diverged history → behind remote → create or reopen PR → unpushed → PR conflicts → failing checks → changes requested → new comment-only reviews (by anyone but the PR's author) → base moved → draft → waiting (GitHub catching up, checks running, review, re-review) → merge → archive. Actionable steps outrank waits across repos.
  - Ahead/behind is measured against `origin/<branch>`, never `@{u}`; new worktrees use `--no-track` so they don't track their base.
  - Sync: `git --no-optional-locks status` every 3 s for the shown workspace (so it never takes the agent's `index.lock`), plus after each agent turn and on window focus. `git fetch --prune` every 3 min. `gh pr list --head` every 60 s, every 10 s while checks run or GitHub computes mergeability, every 5 min after a gh error (the last good PR state stays on screen). Network calls never prompt and time out.
  - Before running, the step re-reads git, and GitHub for steps that act on it, and refuses if the state moved on (agent prompts and pull/push use the PR state on screen, so they don't wait for gh). Every step that writes waits for the agent to be idle. Merge and force push confirm first; merge uses each repo's allowed method with `--match-head-commit`, and stops at the first failure.
  - Merge order: repos merge in the order set with `space p k/j` (or "Merge order…" in the git menu), saved per workspace in `state.json`; repos never moved follow in the order they were added.
  - Branch cleanup: "Toggle deleting remote branches after merge" (palette or git menu, off by default, saved in `state.json`) adds `--delete-branch` to the merge. Run from the linked worktree, gh then deletes only the remote branch (it skips a local branch checked out in the current worktree); gh refuses `--delete-branch` on repos with a merge queue. Archive (not force-archive) then deletes the local branch in each source repo whose work landed in a merged PR or that has no commits of its own, and only while the branch still points at the commit found merged.
- **Attention:** a sidebar row shows a check when a turn finished while the workspace wasn't on screen (cleared when you open it), and an alert with the step when the next git step is yours: conflicts, failing checks, a git error, a review or comments to address, or ready to merge. The Dock badge counts such workspaces.
  - Workspaces that aren't shown get a slow check (`git status` plus `gh pr list`, no fetch): one every 20 s in turn, each at most every 5 min, sooner after its turn ends, never while its agent works or it is being archived. A gh failure keeps the last answer; after failures in two workspaces in a row, nothing is checked for 5 min.
  - A finished turn (off screen, or with the window in the background) and a workspace off screen that starts needing you raise a macOS notification through `osascript` (`UNUserNotificationCenter` needs an app bundle, which `cargo run` isn't), so it's listed under Script Editor in System Settings → Notifications.
- **PRs:**
  - For each repo with commits: push, then `gh pr create --fill`. Text in the composer titles new PRs: the first line is the title and the rest the body (`--title`/`--body` override what `--fill` fills in); a dialog shows the title first, and the composer is cleared once the PRs exist.
  - "Create draft PRs" (`space p d`) opens them with `--draft`; the git button then offers "Ready for review".
  - Each PR body then gets a "Related PRs" section, which is idempotent on re-runs.
  - Refuses to run while any repo has uncommitted changes. Only an open PR is reused; after a merge, new commits get a new PR.

Background and prior art: [docs/research.md](docs/research.md). UI theme, design rules and checklist: [docs/design.md](docs/design.md).
