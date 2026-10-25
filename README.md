# Shark

A fast, native macOS app for running Claude Code on features that span several repos.

<img src="docs/images/hero.png" width="800" alt="A Shark workspace with two repos: sidebar, combined diff with repo-prefixed paths, and the changed-files tree">

## Fast and native
Shark is built on [GPUI](https://www.gpui.rs), Zed's GPU-rendered UI framework, running on Metal. It is not an Electron app. Everything has a key: `cmd-k` opens the palette, lists and diffs move with Vim keys, and `space` starts leader commands. The UI stays responsive while agents work. Chat history loads off the UI thread, git status runs with `--no-optional-locks` so it never takes the agent's `index.lock`, and `ctrl-c` interrupts a turn while the claude process stays warm.

<img src="docs/images/palette.png" width="800" alt="The cmd-k command palette">

## Version-controlled plans
In plan mode (`shift-tab`) Claude reads and plans but edits nothing. Each plan it proposes is saved as a new version. The plan pane shows what changed since the previous version: added words in green, removed words struck through in red, changed code blocks as diffs. Reply in the chat to revise the plan, step through versions with `h`/`l`, and press `a` to approve and build.

<img src="docs/images/plan-versions.png" width="800" alt="The plan pane showing a revised plan with added and removed words marked">

## Multi-repo workspaces
One workspace = one feature = one git worktree per repo on a shared branch, e.g. `~/shark/workspaces/eng-123-login/{ios,backend}`. A single agent works across all of them. The diff is combined, PRs are cross-linked and merge in an order you set, and one git button shows the next step for every repo: commit, push, fix checks, address review, merge.

<img src="docs/images/multi-repo.png" width="800" alt="The new-workspace dialog with several repos ticked">

## Install
```sh
brew tap KaffeDiem/shark https://github.com/KaffeDiem/shark
brew install --cask KaffeDiem/shark/shark
```
Or [Download Shark 0.1.1](https://github.com/KaffeDiem/shark/releases/download/v0.1.1/Shark-0.1.1.dmg) (Apple Silicon), open it and drag Shark to Applications. The app isn't notarized: on first launch, open it, then allow it in System Settings → Privacy & Security → Open Anyway, or run `xattr -dr com.apple.quarantine /Applications/Shark.app`.

Needs `git`, `gh` (logged in) and `claude` on your login shell's PATH. For Codex models (`codex`, Sol, Astra), also `codex`, logged in with `codex login` (a ChatGPT plan works). Codex chats have no plan mode or side questions.

## Quick start
1. `cmd-n`, tick the repos, name the branch, and optionally type a first message. `enter` creates the workspace.
2. `shift-tab` for plan mode. Revise the plan in the chat, then `a` in the plan pane to build.
3. `space c`/`space d` switch between chat and diff. `c` on a diff line leaves a comment, and `shift-c` sends all comments to the agent.
4. `space p r` creates linked PRs. The git button (top right) takes it from there.

## Keys
| Where | Keys |
|---|---|
| anywhere | `cmd-k` palette · `cmd-,` settings · `cmd-n` new workspace · `cmd-t` new chat · `cmd-w` close chat · `cmd-shift-t` reopen closed chat · `ctrl-tab`/`ctrl-shift-tab` or `cmd-shift-]`/`[` next/previous chat · `cmd-1…9` open the Nth workspace (hold `cmd` to see the numbers) · `cmd-j` next workspace that needs you · `cmd-p` open any file · `ctrl-c` interrupt the shown chat's turn, again to stop the process (not in the editor) · `ctrl-w h/l` previous/next pane · `cmd-b` sidebar ↔ icon rail · `ctrl-cmd-v` Vim keys in the editor on/off · `cmd-/` model menu · `ctrl-cmd-1…8` default/fable/opus/sonnet/haiku/codex/codex:gpt-6.1-sol/codex:gpt-6-astra · `cmd-q` quit · `cmd-h` hide · `cmd-m` minimize |
| sidebar, transcript | `j/k` `gg` `G` · `ctrl-d/u` in the transcript · `enter` open / toggle tool output (`o` too in the transcript) · `i`/`a` → composer |
| sidebar sections | `space s n` new section (named in place, `enter` saves, `esc` cancels) · `m` / `space s m` move workspace to a section · `h`/`l` fold/unfold · `enter` on a header toggles it · `space s r` rename · `space s i` icon · `space s d` delete (its workspaces stay) |
| composer | `enter` send (queues while the agent runs) · `cmd-enter` send now, steering the running turn · `cmd-shift-enter` ask as a side question · `↑` on the first line → queue · `shift-enter` newline · `esc` → normal mode · `@` add a file or folder (`repo/` to pick the repo) · drop files from Finder anywhere on the chat (a dashed overlay shows while they hover), or `cmd-v` copied files or an image, to attach them · `/` skills and commands at the start · in the menu `↑/↓` `enter`/`tab` `esc` |
| queue | `j/k` `↑/↓` `gg` `G` · `enter` send now · `e` edit (`enter` puts it back) · `d`/`backspace` delete · `shift-k/j` or `alt-↑/↓` move up/down · `esc`/`i`, or `↓` past the last row → composer |
| side question | `esc` dismiss · `c` copy the answer · `enter` put the answer into the composer |
| leader (normal mode) | `space space` palette · `space ?` side question · `g t`/`g T` next/previous chat · `space ,` settings · `space q` queue · `space w n/d/D` new/archive/force-archive · `space w a` add a repo to the workspace · `space w j` next workspace that needs you · `space r a` add repo · `space c/d` chat/diff · `space p r` create linked PRs · `space p d` create draft PRs · `space p o` open PRs · `space p k/j` move a repo earlier/later in the merge order · `space g g` next git step · `space g m` git menu · `space g f` fetch now · `space m m` model menu · `space m 1…8` pick model · `space o f/t` reveal in Finder / open in Terminal · `space y p/b` copy path / branch · `space b` sidebar rail · `space t` file tree · `space x` close file · `space f f` open any file (fuzzy) |
| new workspace | `enter` create (in branch or first message) · `tab` branch → repo filter → first message · type to filter · `enter` tick match · `space` tick/untick highlighted · `cmd-enter` create · last-used repos start ticked |
| sidebar mouse | right-click a workspace: open, move to section, reveal in Finder / Terminal / copy path (per repo), copy branch, open PRs, archive · drag a workspace onto a section header to move it in (onto Workspaces to ungroup), or onto another workspace to take its place · drag a section header onto another to reorder sections · hover Workspaces for **+** new section, or right-click it or the empty space below · click a section header to fold it; right-click it to rename, change icon, delete · click a repo: new workspace with it ticked · hover Repos for **+** add repo, or drag folders from Finder onto the sidebar |
| plan mode | `shift-tab` (composer, transcript, queue, plan) or `space l l` plan mode on/off · `space l v` show/hide the plan pane · `space l a` approve and build · `space l n` approve and build in a new chat · in the pane `j/k` `gg` `G` `ctrl-d/u` scroll · `h/l` older/newer version · `]c`/`[c` next/previous change · `d` changes on/off · `a` approve · `n` build in a new chat · `i` → composer · `enter` on a transcript **Plan** row opens that version |
| settings | `ctrl-tab` Appearance ↔ Prompts |
| diff | `j/k` `gg` `G` `ctrl-d/u` move the line cursor (kept across refreshes) · `h/l` scroll sideways · `]c`/`[c` changes · `}`/`{` files · `v` viewed / `o` open the cursor's file · `c` comment on the cursor line or selected lines (edit: `c` again; empty removes) · `shift-c` send all comments to the agent · `s` split/unified · `r` refresh · `p` play/pause the file's video · viewed checkbox in each file header |
| file tree | `j/k` `gg` `G` `ctrl-d/u` · `enter` show file / toggle folder · `h/l` collapse/expand · `v` viewed (on a folder: every file in it) · `o` open in editor |
| editor | `cmd-s` save · `cmd-w` close · `esc` back to the tree · with Vim on: motions `hjkl w b e 0 ^ $ gg G { }` with counts, `i a I A o O` (`esc`/`ctrl-c` back to normal), `x dd yy cc D C p P J r ~`, `d/c/y/>/<` + motion, `v`/`V`, `u` / `ctrl-r`, `/ n N`, `:w`, and in Normal mode the leader keys (`space x`, `space f f`, …) |

## How it works
- **Workspace creation:**
  - For each repo, `git worktree add --no-track -b <branch> <ws>/<repo> origin/HEAD`. Every repo is checked first, so a refusal leaves nothing behind: the branch is already checked out somewhere, or a local branch of that name exists that origin doesn't have (usually left from an archived workspace; pick another name or rename it). Offline or without an origin, the local branch is used.
  - A branch that only origin has starts at `origin/<branch>` (still `--no-track`); one both have uses the local branch.
  - The dialog's optional first message is sent to the agent once the workspace is open.
  - **Add repo to workspace** (`space w a`) adds a worktree for another repo on the same branch the same way, rewrites the root `CLAUDE.md`, and the agent is told about it with the next message.
  - Gitignored files that match `.worktreeinclude` (default `.env*`) are copied into the new worktree.
  - A root `CLAUDE.md` is written that lists the repos and `@`-imports each repo's own CLAUDE.md.
- **Menu bar:** the Shark, File, Edit, View and Window menus run the same actions as the keys and the palette.
- **Restart:** the open workspace and tab (Chat or Diff) are saved in `state.json` and reopened at launch, on the chat tab last shown.
- **Archive:** checks every repo up front (uncommitted or untracked files, locked worktrees, submodules), so it can't stop halfway. Force-archive removes dirty worktrees and submodules but not locked ones. Messages to any of the workspace's chats are refused while it is being archived, and it refuses while any chat runs.
- **Chats:** a workspace has one or more chat tabs, each its own claude process, session, model, queue and transcript; they can run at once in the same worktrees. The tab title is the first line of its first message.
  - `cmd-w` on a running chat asks before stopping it. A closed chat that had a session can come back with `cmd-shift-t` or "Reopen closed chat…" (the 20 newest are kept); closing the last tab leaves a fresh one.
  - Git steps that write wait for every chat to be idle, and judgment steps prompt the shown chat.
- **Attachments:** files dropped on the chat or pasted into the composer are copied to `<ws>/.context/attachments/` and mentioned as a chip (`@".context/attachments/x.png"`, quoted so names with spaces resolve). A file already in the workspace is mentioned in place; a folder outside it is mentioned by absolute path, not copied. Same name and bytes reuses the copy; same name, other bytes gets ` (1)`. The U+202F in macOS screenshot names becomes a space, and HEIC is converted to JPEG with `sips`. Archive removes them with the workspace.
- **Plan mode** (`shift-tab`, or the **Plan** button in the composer): the chat's claude switches to `plan` with a `set_permission_mode` control request (a new process switches as it starts), so it reads and plans but edits nothing. claude runs with `--permission-prompt-tool stdio`, so its `ExitPlanMode` call asks shark instead of failing: shark keeps the plan as the next version (in `plans/<chat>.json` next to the transcripts) and denies the call, telling claude the user is reviewing it, so the turn ends. Reply in the chat to revise; each revision is a new version. The plan pane shows the newest version with what changed since the one before marked in place: added words green, removed words struck through in red, a changed code block as a `diff` block. **Approve and build** switches back to `bypassPermissions` and tells claude to implement the plan. **Build in new chat** instead opens a new chat tab whose first message is the plan alone, so the build starts without the planning back-and-forth in context; the planning chat keeps its history, with the plan marked approved. Either way the plan pane closes; `space l v` brings it back. Other tools that ask (`AskUserQuestion`, `EnterPlanMode`, anything needing approval) are denied with a reason, as headless claude did before.
- **Side question** (`cmd-shift-enter` on a draft, or `space ?`): a one-off `claude -p --resume <session> --fork-session --no-session-persistence --tools "" --strict-mcp-config` answers from the chat's context without tools, in a card above the composer. Nothing reaches the chat, its transcript or its session, and it works while the chat runs. Only the latest answer is kept.
- **Agent:**
  - One process per chat: `claude -p --input-format stream-json --output-format stream-json`, with the workspace root as cwd.
  - Runs with `--permission-mode bypassPermissions`. Worktrees isolate branches, not the filesystem.
  - `ctrl-c` sends an `interrupt` control request: claude ends the turn and the process stays warm with its background tasks. If the turn hasn't ended after 5 s, or on a second `ctrl-c`, the process gets SIGTERM; when background tasks would die with it, only the second `ctrl-c` sends it. With only background tasks left, `ctrl-c` stops the process.
  - **Queue:** a message sent while a turn runs waits in a queue above the composer, top first. The top one goes out when a turn ends without error; a stop or an error leaves the queue alone, and so does editing one of its messages. Send now writes the message to claude's stdin mid-turn: claude reads it at its next tool call, or as a new turn if the reply has no more tool calls. The queue is in memory only.
  - Each chat's transcript is stored in `~/Library/Application Support/shark/transcripts/<chat>.jsonl` (the first chat is named after the workspace; tool output clipped to 20k characters, each tool-input string to 2k), and `--resume` continues the session after a restart. If claude no longer has a chat's session, shark drops it and that chat's next message starts a fresh one. The palette's "New conversation" drops the shown chat's session by hand. History is read and parsed off the UI thread and goes ahead of anything sent while it loads; a turn without a result (shark quit or claude crashed) is marked "interrupted".
- **Diff:** one `git diff --merge-base <base>` per worktree, with paths prefixed by repo name. Untracked files are included by marking them intent-to-add in a throwaway copy of the index (`GIT_INDEX_FILE`), so the real index and the agent's `index.lock` are never touched.
- **Images and videos in the diff** (`src/app/media.rs`): a changed image or video gets a before/after row under its header, loaded only once the row is on screen and not while the file is folded. Both sides are named by the blob hash on the patch's `index` line (git hashes working-tree files too) and copied to `~/Library/Caches/shark/media/<hash>.<ext>` (old side via `git cat-file`), so nothing is decoded twice or goes stale; copies unused for two weeks are pruned at launch. The new side's copy is checked against its hash (`git hash-object --path`), so a file rewritten before its row loads isn't cached under the old one. PNG, JPEG, GIF (animated), WebP, BMP, ICO and SVG up to 1200 px are drawn as they are; bigger ones and HEIC, AVIF, TIFF and PSD are downscaled to 1200 px with `sips`, and big SVGs are drawn down with resvg. Empty files and Git LFS pointers say so instead of failing to decode. gpui keeps decoded images in memory and on the GPU until dropped, so the diff's image cache keeps the 32 most recently drawn. MP4, MOV and M4V get a poster frame (`qlmanage -t`, 5 s deadline, a failure is remembered) and play inline, muted, on click or `p`: AVFoundation decodes NV12 frames that go to gpui's `surface()` without a copy, the window redraws only on a new frame, one video plays at a time and it pauses when scrolled away. A video stored rotated (portrait phone video) plays through an `AVVideoComposition` that turns it upright; one that yields no frame within 5 s (audio only, an undecodable codec, a broken file) says it can't be played. WebM, MKV and AVI (which AVFoundation can't open) and images open in the default app on click.
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
- **Attention:** a sidebar row shows a check when a turn finished while the workspace wasn't on screen (cleared when you open it) or in one of its chat tabs that isn't shown (cleared when you show that tab), and an alert with the step when the next git step is yours: conflicts, failing checks, a git error, a review or comments to address, or ready to merge. A folded section's icon turns amber while a workspace in it needs you. The Dock badge counts such workspaces.
  - Workspaces that aren't shown get a slow check (`git status` plus `gh pr list`, no fetch): one every 20 s in turn, each at most every 5 min, sooner after its turn ends, never while one of its chats works or it is being archived. A gh failure keeps the last answer; after failures in two workspaces in a row, nothing is checked for 5 min.
  - A finished turn (off screen, or with the window in the background) and a workspace off screen that starts needing you raise a macOS notification through `osascript` (`UNUserNotificationCenter` needs an app bundle, which `cargo run` isn't), so it's listed under Script Editor in System Settings → Notifications.
- **Settings** (`cmd-,`; `src/settings.rs`): changes apply and save to `state.json` at once.
  - Appearance: mode (System/Light/Dark), a theme for light mode and one for dark mode (Linear, Roraima, Everest, Sequoia, Sakura), interface and code font (any installed font).
  - Prompts: every prompt the git button sends to the agent (`src/prompts.rs`) can be edited; `{repos}` and `{branch}` are filled in. Only edits are saved, and "Reset to default" drops the edit.
- **PRs:**
  - For each repo with commits: push, then `gh pr create --fill`. Text in the composer titles new PRs: the first line is the title and the rest the body (`--title`/`--body` override what `--fill` fills in); a dialog shows the title first, and the composer is cleared once the PRs exist.
  - "Create draft PRs" (`space p d`) opens them with `--draft`; the git button then offers "Ready for review".
  - Each PR body then gets a "Related PRs" section, which is idempotent on re-runs.
  - Refuses to run while any repo has uncommitted changes. Only an open PR is reused; after a merge, new commits get a new PR.

## Development
```sh
cargo run                                  # needs Xcode (Metal); Rust is pinned in rust-toolchain.toml
cargo test                                 # git/worktree, stream-json parser, PR linking
cargo test --features test-support         # + headless keyboard UI test
cargo test live_claude -- --ignored        # real claude CLI round trip (costs a few cents)
cargo test live_codex -- --ignored         # the same through codex app-server
```

Releases: merge a PR with the `release` label. CI (which builds every merge to `master`) builds the DMG (`make release`), publishes a GitHub release, and commits the new version to `Cargo.toml`, `Casks/shark.rb` and the Install section above. The version is the one in `Cargo.toml`, or its next patch if that is already released.

Background and prior art: [docs/research.md](docs/research.md). UI theme, design rules and checklist: [docs/design.md](docs/design.md).

## License
GPL-3.0 with an attribution term: copies and derived works must keep the copyright notice and credit "Shark by Kasper Munch Jensen". See [LICENSE](LICENSE).
