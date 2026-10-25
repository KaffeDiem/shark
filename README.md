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
| anywhere | `cmd-k` palette · `cmd-n` new workspace · `ctrl-c` stop agent · `ctrl-w h/l` sidebar/main |
| sidebar, transcript | `j/k` `gg` `G` `ctrl-d/u` · `enter` open / toggle tool output · `i`/`a` → composer |
| composer | `enter` send · `shift-enter` newline · `esc` → normal mode |
| leader (normal mode) | `space space` palette · `space w n/d/D` new/archive/force-archive · `space r a` add repo · `space c/d` chat/diff · `space p r` create linked PRs · `space p o` open PRs |
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
- **PRs:**
  - For each repo with commits: push, then `gh pr create --fill`.
  - Each PR body then gets a "Related PRs" section, which is idempotent on re-runs.
  - Refuses to run while any repo has uncommitted changes.

Background and prior art: [docs/research.md](docs/research.md). UI theme, design rules and checklist: [docs/design.md](docs/design.md).
