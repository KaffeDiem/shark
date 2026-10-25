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

## Editors & Mermaid (Oct 2026)
Goal: Mermaid in markdown, a live-preview markdown editor (`# ` becomes a heading) for a notes/plans pane, and a better code editor. Stay native: no webview, no Zed GPL crates.

### What gpui-kit 0.8 already gives us
- **Markdown:** parsed by markdown-rs (mdast) with GFM and math. Math is parsed but not typeset.
  - Custom blocks: `MarkdownPlugin` (`is_block`, `parse`, `render`) can claim a ```` ```mermaid ```` fence. Copy `FrontmatterPlugin` in `gpui-component/src/text/frontmatter.rs`.
  - Documents up to 4 KiB parse synchronously on the UI thread (`MAX_SYNC_FULL_REPLACE_BYTES`, `gpui-base/src/text/state.rs`). `parse()` must only capture the source. Do the expensive work on a background task with a cache.
- **SVG:** `img(Arc::new(Image::from_bytes(ImageFormat::Svg, bytes)))` renders through resvg 0.46. It is cached by a hash of the bytes. `svg()` tints everything one colour, which suits icons, not diagrams.
- **Code editor:** `EditorState`/`Editor`.
  - Rope buffer, about 200K lines, folding, search and replace, multi-cursor.
  - LSP provider hooks: completion, hover, definition, code actions, semantic tokens.
  - Highlighting via the `syntect` or `tree-sitter-<lang>` features, including `tree-sitter-markdown(-inline)`.
  - There is no vim mode, but the movement actions are exported, so we can add a keymap.
- **Decorations** only change colour, weight, italic, background, underline and strikethrough. They can't change font size or hide markers, so a Typora-style editor can't be built on `EditorState`.
- **`gpui_kit::base::text::editor_host`:** we keep our own document and it renders mdast blocks and returns caret geometry. We own caret, IME and undo. This is the fallback for building a live-preview editor ourselves.
- **WebView:** none in gpui-kit. `gpui-webview` 0.8 is a separate crate. It draws over GPUI popovers and has no keyboard or focus bridge.

### Mermaid
- **Pick: merman** (https://github.com/Latias94/merman). MIT/Apache, very active, 37 diagram types.
  - Zed switched to it in May 2026 (zed#57644).
  - Use `default-features = false, features = ["svg", "layout-cytoscape", "all-diagrams"]` (or individual `diagram-*` features; with defaults off, no diagram types are compiled in). The default `elk` layout is EPL-2.0.
  - Use its resvg-safe SVG mode, which avoids `foreignObject`.
- **Zed's `mermaid_render` is GPL.** It does theme CSS, strips `foreignObject`, caps the pixmap size (zed#56468), and measures labels with gpui's text system (zed#65192). Reimplement these only when a diagram looks wrong.
- **Rejected:**
  - mermaid-rs-renderer: faster, but less accurate. Zed dropped it.
  - mermaid.js in QuickJS/deno: needs a DOM shim.
  - mmdc: ships Chromium.
  - Hidden WKWebView: possible later, for full fidelity.

### Live-preview markdown editor (notes/plans pane)
- **Candidate: zorite-editor 0.11** (https://github.com/packetThrower/zorite, `crates/zorite-editor`). MIT, depends on `gpui-pre ^0.3`, not on gpui-component.
  - Hides markers except near the caret.
  - Headings, lists, tasks, tables as a grid, IME and undo.
  - The app supplies the Mermaid, math and code-highlight renderers.
  - Risks: one maintainer, a few hundred downloads, a `String` buffer, no vim. Its gpui-pre version must match ours [?]. Treat it as something we might fork.
- **If we build our own** on `editor_host` + tree-sitter-md, use these as references: Velotype (Apache-2.0, https://github.com/manyougz/velotype) and Markion (MIT, https://github.com/willmove/markion).
- **Never link Zed's `editor`, `markdown`, `rope`, `text` or `vim` crates.** They are GPL-3.0.
- **Webview is the last resort** (CodeMirror 6 + `@replit/codemirror-vim`). Problems: z-order, focus bridging, about 50 MB per view [?].

### Roadmap (not started)
1. **Mermaid in chat.**
   - Add `src/mermaid.rs`. Its plugin claims `lang == "mermaid"` fences only once they are closed.
   - Cache rendered diagrams by source hash, as Pending, Svg or Err. Render on the background executor.
   - When a render finishes, update the cache and call `cx.refresh_windows()`. `MarkdownPlugin::render` has no handle to the view to notify.
   - Show the last good image while waiting, fall back to the code block on error, and cap the raster size.
   - Register it in `src/chat.rs` via `TextView::plugin`.
2. **Composer becomes an `EditorState`** with tree-sitter markdown: styled source, list continuation on Enter. `auto_grow` only exists on `TextareaState`, so content-based height is extra work.
3. **Notes/plans pane:** try zorite-editor, feeding it the same Mermaid cache. If that doesn't work out, build our own on `editor_host`.
4. **Code view:** `Editor` with a few tree-sitter grammars and a vim normal-mode keymap. LSP later.

Tests for the Mermaid work:
- Closed vs. unclosed fence, cache hit, error fallback.
- A headless UI test that a mermaid block renders an image.
- `cargo run` with a diagram streamed in.
