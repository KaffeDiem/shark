# Research notes (Oct 2026)

Background for shark: a native app that runs coding agents across **multi-repo** workspaces. Items marked [?] are unverified.

## Conductor (conductor.build)
- Each workspace is one git worktree of **one** repo, under `~/conductor/workspaces/<repo>/<city>`. A workspace can start from a branch, PR, GitHub issue or Linear issue.
- Config lives in `.conductor/settings.toml`: setup/run/archive scripts, env vars, `branch_prefix`, and which files to copy. Copied files are selected by `.worktreeinclude` → `file_include_globs` → `.env*`, and only gitignored files are copied. Each workspace gets 10 ports starting at `CONDUCTOR_PORT`.
- Checkpoints are saved in private git refs before each turn. The diff viewer uses Pierre Diffs, with line comments that go back to the agent. PRs are created with `gh`, with an AI-written body and a CI checks tab.
- It bundles pinned `claude`/`codex` binaries. Stack is Tauri + Rust per a July 2025 founder thread [?]. Mac only. It has an MCP server and a REST API.
- **Multi-repo:** only partial. Since 0.25.6 you can "link" workspaces with `/add-dir`, but diffs, PRs and archiving stay per-workspace. https://www.conductor.build/docs/guides/repositories/linking-multiple-directories
- Docs: https://www.conductor.build/llms-full.txt · changelog: https://www.conductor.build/changelog

## Similar tools
| Tool | Stack | Multi-repo |
|---|---|---|
| Vibe Kanban (BloopAI) | Rust backend + React | **Yes**: one worktree per repo, unified diff, a PR per repo. Closest prior art. https://vibekanban.com/docs/workspaces/multi-repo-sessions |
| Crystal → Nimbalyst | Electron | No |
| Superset | Electron, xterm.js, node-pty | No |
| Claude Squad, uzi | Go + tmux | No |
| container-use (Dagger) | Go MCP server, containers | No |
| Sculptor (Imbue) | ? | No |
| Cursor cloud agents | cloud VMs | Yes, since May 2026 (web first) |
| Codex app | app-server [?] | Not documented |
| Claude Code Desktop | first party | Only via `--add-dir` |

Open-source GPUI apps in the same space, worth reading: **Arbor** (worktrees, terminals, diffs) and **OxiMux**. https://github.com/zed-industries/awesome-gpui

## UI stack: GPUI + gpui-kit
- The `gpui` crate on crates.io is stuck at 0.2.2. The current snapshot is `gpui-pre` 0.3.9, published weekly from Zed by Longbridge.
- **gpui-kit 0.8.0** (formerly gpui-component, Apache-2.0) re-exports it. Depend on `gpui-kit` alone. It needs Rust ≥ 1.95 (we pin 1.98.1) and full Xcode for Metal.
- Components we use: Input, TextView (streaming markdown), Diff (parses unified patches, split/unified, virtualized, syntect), Command palette, Dialog, Notification. There is no terminal component. https://gpui-kit.com/component
- Zed's own `editor`, `vim` and `terminal` crates are GPL-3.0. Use them as reference only.
- Keys use GPUI primitives: `actions!`, `KeyBinding::new("g g", Action, Some("Ctx"))`, `.key_context()`, `FocusHandle`. Multi-key sequences are native, with a 1 s pending timeout.
- For a terminal later: `alacritty_terminal 0.26` + `portable-pty 0.9`.

## Driving Claude Code
- There is no Rust SDK. The TS/Python Agent SDK wraps the CLI. Use `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages`.
- Event types: `system/init` (carries `session_id`), `stream_event` (text deltas), `assistant` (full content blocks), `user` (tool_result), `result`. Use `--resume <id>` to continue a session. SIGTERM, then resume, is safe.
- Headless `-p` denies tools that need approval, so the agent needs `bypassPermissions` (or a host permission prompt).
- **Multi-repo caveat:** with cwd = the workspace root, a repo's `CLAUDE.md` loads only after the agent touches that subdirectory, and its `.claude/settings.json`, hooks and `.mcp.json` are ignored. `--add-dir` grants file access but doesn't load CLAUDE.md unless `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1` is set. Shark writes a root `CLAUDE.md` that `@`-imports each repo's file. https://code.claude.com/docs/en/memory
- Codex (later): `codex app-server` speaks typed JSON-RPC over stdio (`generate-json-schema`). `sandboxPolicy.writableRoots` accepts several directories.
- Licensing: building on a user's claude.ai subscription needs Anthropic's approval for third-party products.

## Git worktree pitfalls
- A branch can't be checked out in two worktrees. Detect this and fail early.
- A sandboxed agent may fail to commit because the worktree's gitdir lives in `<main>/.git/worktrees/`, outside the workspace.
- Submodules are poorly supported. Hooks are shared across worktrees.
- Each worktree is a cold build: Xcode DerivedData is keyed by path, and `node_modules`/Pods are per tree. Fixes: APFS `cp -c` clones and a per-workspace `-derivedDataPath`.

## Multi-repo prior art to borrow
- Google `repo`: the same topic branch in every project, uploaded as one Gerrit "topic".
- `meta`, gita, myrepos: a manifest plus a command fanned out to each repo.
- Sourcegraph Batch Changes: a dashboard tracking each repo's PR, a good model for workspace PR status.
- VS Code `.code-workspace`: generate one per workspace for "open in editor".

## Linear (M2)
- `Issue.branchName` is the suggested branch name, so use it as the shared branch.
- Putting "Part of ENG-123" in each PR keeps the issue open until the last PR merges. `attachmentLinkGitHubPR` adds rich links.
- Auth: a personal API key or OAuth PKCE. Webhooks need a public URL, so a desktop app should poll for `assignee isMe` issues instead.
