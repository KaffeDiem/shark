# Language intelligence integration and release gates

This change retains GPUI and its 4 MiB editor limit. Language servers are shared by
project, backend, worktree, and toolchain identity. Tool setup and pipe I/O run away
from the UI thread. Project dependencies and compiler SDKs remain prerequisites.
This document records the implementation and evidence; it is not a claim that the
polished release or performance gates are complete.

## Using it

Open a Rust, Python, TypeScript/JavaScript, Swift, or Go source file in a workspace.
The editor starts its language session and reports Starting, Preparing index,
Ready, or Setup needed. While the server starts or reports `$/progress`, the header
shows a spinner with the newest work and its percentage ("Indexing 42%"; the server's
message is in the tooltip) and a progress bar under the header, indeterminate until a
percentage arrives. Setup needed shows the problem with a Retry button; Retry
rediscovers the project. Error and warning `window/showMessage` notices from a server
appear as notifications.

A project's files share one server. The server outlives the replacement of its only
tab (tree clicks, same-tab definitions, Back) and stops when the last file using it
closes. Switching workspaces parks the outgoing workspace's servers for five minutes;
returning within that window resumes them warm.

The header is a jump bar: back/forward buttons, the file path, then the symbols around
the cursor. Click a folder to list its files and folders, or a symbol to open the file's outline as an indented tree with that
symbol selected. Command-click follows a definition in the current tab, replacing a
clean file (back restores it); Command-Shift-click opens a new tab. Picking a file from
a folder works the same way. A file with unsaved edits is never replaced. The mouse's
back/forward buttons walk the same history; quick repeated presses step one entry each,
and an entry whose file was deleted is skipped.

Definitions outside the workspace, and inside installed dependencies (`node_modules`,
`site-packages`), open read-only with the server that found them, so hover and
definitions keep working there. Back returns the replaced file to its tab position.

Format document sends the buffer's own indentation (tabs, or its most common indent
step) as the formatting options; servers such as tsserver reindent to them.

| Action | Key |
|---|---|
| Format document | Option-Shift-F |
| Go to definition | F12 (Vim: `g d`) |
| Rename symbol | F2 (starts with the symbol selected) |
| Find references | Shift-F12 |
| Document symbols (outline tree) | Command-Shift-O (Vim: `g s`) |
| Signature help | Command-Shift-Space |
| Navigate back / forward | Control-minus / Control-underscore, that is Control-Shift-minus (Vim: `ctrl-o` / `ctrl-i`) |

Completion, hover, definitions, code actions, and diagnostic overlays use GPUI's
existing editor interfaces. Language edits remain unsaved until the normal checked
save; unsupported filesystem operations are rejected.

## Project and tool selection

Discovery walks from the opened file toward its repository worktree boundary;
Cargo resolves its actual workspace through `cargo locate-project --workspace`.
Symlinks leaving that boundary are rejected for editable documents. Separate
worktrees cannot share a server, even when their project names match.

* **Rust:** use the effective rustup toolchain with automatic compiler installation
  disabled. Add `rust-analyzer` and `rust-src` only to that already installed
  toolchain. Non-rustup toolchains must supply their own analyzer and matching
  sources. Shark never upgrades the compiler.
* **Python:** Pyrefly supplies intelligence and Ruff supplies formatting/fixes.
  `pyrightconfig.json` or `[tool.pyright]` selects managed Pyright instead of
  converting the project. The nearest explicit checker configuration wins. A local
  `.venv`, `venv`, or activated virtual environment supplies the interpreter.
* **TypeScript/JavaScript:** an installed `node_modules/typescript` SDK governs
  selection. Classic SDKs use the pinned wrapper with the exact project's
  `lib/tsserver.js`; native SDKs run their own `bin/tsc --lsp --stdio`. A declared
  but missing SDK produces a repair message rather than changing compiler
  semantics. Unconfigured projects use the pinned native artifact. Managed
  Node/npm supplies the wrapper and automatic type acquisition runtime.
* **Swift:** resolve SourceKit-LSP through the selected Xcode/Swift `xcrun`
  toolchain. A nearer SwiftPM package remains a package even when an Xcode project
  sits above it. Preserve existing custom `buildServer.json`. For new Xcode
  setups, a unique workspace or project selects SweetPad's standalone BSP;
  ambiguous containers use a persisted application-support selection. Only
  Shark-owned BSP configuration is regenerated after root or tool changes.
* **Go:** find `go.work` before individual modules, respecting `GOWORK=off`.
  Toolchain lookup honors project files but disables compiler downloads. Use the
  gopls binary built and bundled by Shark's release workflow.

SweetPad CLI 0.1.15's `bsp init` accepts a project or workspace; its BSP serves the
container's targets and does not accept a scheme/destination selection. Schemes
and destinations are build-time choices. This integration does not trigger an
Xcode build. Xcode sessions retain Preparing index with a build repair message;
project-wide navigation readiness has not been verified. See the
[SweetPad autocomplete documentation](https://sweetpad.hyzyla.dev/docs/cli/autocomplete/)
and [CLI BSP implementation](https://github.com/sweetpad-dev/sweetpad/blob/cli-v0.1.15/sweetpad-cli/src/cli/commands/bsp.rs).

## Fixed artifact provenance

`src/lsp/tool-manifest.json` contains immutable versions, Apple Silicon artifact
URLs, SHA-256 hashes, and executable paths. Installation downloads over HTTPS to
unique staging directories, verifies the archive checksum and paths, extracts,
checks the executable path, and atomically renames the completed version.
Existing version directories are retained. Cached verified versions can be reused
offline; failed stages are removed. No npm install scripts or project
configuration migration commands run.

| Tool | Pin | Provenance |
|---|---|---|
| Pyrefly | 1.4.0 | [Published release and asset digest](https://github.com/facebook/pyrefly/releases/tag/1.4.0) |
| Ruff | 0.17.3 | [Published release and asset digest](https://github.com/astral-sh/ruff/releases/tag/0.17.3) |
| Native TypeScript | 7.0.2 | [Exact platform package](https://registry.npmjs.org/@typescript/typescript-darwin-arm64/7.0.2) |
| TypeScript language server | 6.0.2 | [Exact package](https://registry.npmjs.org/typescript-language-server/6.0.2) |
| Pyright | 1.1.414 | [Exact package](https://registry.npmjs.org/pyright/1.1.414) |
| Node/npm | Node 22.23.4 distribution | [Official checksums](https://nodejs.org/dist/v22.23.4/SHASUMS256.txt) |
| SweetPad CLI | 0.1.15 | [Published release and asset digest](https://github.com/sweetpad-dev/sweetpad/releases/tag/cli-v0.1.15) |
| gopls | 0.24.0, built by Go 1.27.3 | [Pinned module](https://github.com/golang/tools/tree/gopls/v0.24.0/gopls) |

The npm artifact SHA-256 values were calculated from the exact published tarballs;
GitHub asset hashes and Node's hash were checked against published metadata.
Rust and SourceKit-LSP deliberately follow the selected compiler toolchain.
The gopls build script checks its exact Go version, builds with `-trimpath`,
executes a protocol smoke test, and emits an archive and checksum. App packaging
signs the bundled binary and recomputes its checksum after signing.

## Actual artifact execution, 2026-10-28

Hardware: Apple M4 Max, Mac16,6, 36 GiB memory, macOS 27.0.1 (26A434).
These are functional smoke runs, **not latency or memory benchmarks**.

`scripts/lsp-smoke.py` creates a temporary project and executes initialize → open
→ unsaved edit → completion/definition → diagnostics → close → shutdown. It
checks that the on-disk source remains unchanged. Ruff instead exercises its
supported formatting and diagnostics capabilities. Requests and process waits
are bounded; failures kill and reap the process group.

| Actual artifact | Result |
|---|---|
| Pyrefly 1.4.0 macOS arm64 | Pass |
| Ruff 0.17.3 macOS arm64 | Pass: formatting and diagnostics |
| Pyright 1.1.414 with managed Node 22.23.4 | Pass |
| Wrapper 6.0.2 with project SDK TypeScript 6.0.3 and managed Node 22.23.4 | Pass |
| rust-analyzer 1.93.1 (01f6ddf7) and 1.98.1 (48a229ce), each with matching installed Rust and rust-src | Pass |
| Selected Xcode SourceKit-LSP, Swift 6.4, SwiftPM fixture | Pass |
| Locally built gopls 0.24.0 with Go 1.27.3 | Pass |
| SweetPad CLI 0.1.15, minimal real Xcode project | Pass: generate BSP, initialize, build targets, shutdown |
| Native TypeScript 7.0.2 macOS arm64 | Pass, including strict zero-exit shutdown |

Native TypeScript 7.0.2 rejects a `shutdown` request containing `params: null`;
omitting `params` is accepted. Shark's `lsp-server` serialization already omits
null params. Keeping stdin open after sending `exit` exposed a shutdown failure
(`context canceled`, and a watcher panic in one run). The client writer now closes
stdin after writing and flushing the final `exit` frame; the smoke harness does
the same. The pending notification → shutdown response → exit → EOF order has a
fake-server regression. Forty repeated native TypeScript runs passed: 30 with
Shark's client capabilities and 10 with the smoke harness capabilities. The
zero-exit assertion, bounded shutdown, pinned artifact, and compiler selection
remain unchanged; no checker fallback is performed.

`scripts/check-language-tools.py` downloads and checks the exact manifest
artifacts, exercises them with their managed runtime, and also checks the selected
Rust/Swift toolchain servers and SweetPad BSP. All nine artifact cases passed after
the transport fix. The release workflow runs this gate before signing or
publishing, retaining the strict zero-exit assertion.

Examples:

```sh
python3 scripts/lsp-smoke.py python -- /path/to/pyrefly lsp
python3 scripts/lsp-smoke.py ruff -- /path/to/ruff server
python3 scripts/lsp-smoke.py typescript -- /path/to/tsc --lsp --stdio
scripts/build-gopls.sh
python3 scripts/check-language-tools.py
cargo test --features test-support lsp::
```

For a classic TypeScript SDK, set `SHARK_LSP_SMOKE_INIT` to a JSON object with
`tsserver.path` pointing to that SDK's `lib/tsserver.js`, and run the wrapper with
the managed Node binary.

## Follow-up QA

The [linter QA report](linter-qa.md) records the independent user-flow and
correctness reviews, regression results, and native app follow-up. Native checks
verified empty-cache Python tool setup, quick-fix dismissal retaining editor
focus, rename across two files, and Save/Reload after a concurrent disk change.
QA also repaired the outline shortcut and advertised hierarchical-symbol support
already implemented by the outline parser.
The [performance report](linter-performance.md) records headless editor CPU-frame
timings and actual Python server readiness, warm requests, and process-tree RSS,
with their measurement limits. These scoped measurements do not establish
packaged-app input-to-display latency or busy-indexer performance.

## Gates still required before a polished release

See the [QA report](linter-qa.md) for the final full-suite and focused regression
results. The workflow checks include multiple results, alias/tab reuse, history
selection and scroll, external-source read-only behavior, completion/import/snippet undo,
Vim undo/redo, atomic multi-file edits, dirty diff conflicts, and concurrent changes.
Formatting and whitespace checks also pass. No changes were published.

1. Verify all supported language backends through the final packaged app. The
   nine standalone artifact smokes and the scoped native app checks do not prove
   every GPUI end-to-end language workflow.
2. Exercise an empty-cache Finder launch, interrupted downloads during extraction
   and activation, moved worktrees with custom and Shark-owned BSP, ambiguous
   Xcode containers, real Xcode build/index readiness, and Go multi-module
   projects. Unit fixtures cover selection/cache invariants but do not replace
   these end-to-end runs.
3. Exercise the interactive editor workflows with Vim and concurrent agent writes:
   multiple definitions, back/forward selection and scroll, completion snippets
   plus imports, rename/format across open and closed files, dirty diff conflicts,
   restarts, and external dependency sources. Headless tests cover a subset.
4. Extend the scoped [performance measurements](linter-performance.md) to
   packaged-app input-to-paint latency, cold readiness, warm completion/definition,
   and total process-tree memory with LSP disabled, idle, and actively indexing
   across supported languages. No overall performance or “fastest server” claim
   is justified by the existing runs.

Debugging, extension hosting, terminal integration, global cross-file undo, and a
general-purpose package manager remain out of scope. Ty remains an optional
benchmark candidate rather than another automatic backend.

## Implementation limits of the integration proof

The current implementation establishes the first integration milestone. It does
not establish that the polished five-language release passes every gate above.

- **Completion resolution and memory:** GPUI 0.8.0 exposes its completion menu,
  but not an accepted-item callback covering both keyboard and mouse acceptance.
  Shark therefore resolves the first 16 data-carrying completion items
  concurrently, waits at most about 50 ms, and presents the rest unresolved; an
  unresolved item is inserted without its auto-import. Additional edits, including imports,
  are folded into the primary edit so acceptance remains one undoable operation.
  A folded edit contains the text between its first and last affected ranges;
  many import candidates near the top of a large file can therefore consume
  memory proportional to candidate count multiplied by that text span. The
  four-MiB document limit does not bound aggregate completion-menu memory.
  Measure this workload and eager-resolution latency on named hardware before
  release. The next editor milestone should add an upstream GPUI accepted-item
  hook, then resolve and stage only the selected item's edits; no dependency fork
  or replacement editor is included in this proof.
- **Snippets:** numbered tabstops, placeholder defaults, choices, and mirrored
  fields use GPUI selections, including multiple cursors for mirrors. Snippet
  variables and regular-expression transforms are unsupported; items requiring
  them are omitted rather than inserting snippet syntax into the document.
- **Quick fixes:** workspace text edits are preflighted and left dirty. Command-only
  fixes and fixes requiring a server command are unsupported. Unsupported or
  stale selected fixes report an error. Resource create/delete/rename operations
  and confirmation-annotated edits are rejected before any buffer changes.
- **Conservative stale-edit rejection:** any editor change after a rename or quick
  fix request invalidates its edit guard, even when that change is in an unrelated
  document. Unversioned unsolicited server edits are rejected. This can require
  retrying an action; it prevents an older response from interpreting ranges
  against newer unsaved contents.
