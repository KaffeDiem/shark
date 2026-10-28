# Language intelligence

The editor uses GPUI with a 4 MiB document limit. Language servers are shared by
project, backend, worktree, and toolchain identity. Tool setup and pipe I/O run away
from the UI thread. Project dependencies and compiler SDKs remain prerequisites.
This guide covers usage, tool selection, validation, and known limitations.
Packaged-app release checks are listed below.

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
  toolchain, one `rustup component add` at a time. Linked/custom toolchains use
  the analyzer they ship. Without rustup, the sysroot's analyzer or one on PATH
  (Homebrew's `rust-analyzer` formula) is used. rustup older than 1.28 is refused
  because it installs toolchains implicitly. Shark never upgrades the compiler.
* **Python:** Pyrefly supplies intelligence and Ruff supplies formatting/fixes. If
  Ruff cannot be installed or started, the checker still runs and the status
  tooltip names the failure.
  `pyrightconfig.json` or `[tool.pyright]` selects managed Pyright instead of
  converting the project. The nearest explicit checker configuration wins. A local
  `.venv`, `venv`, or activated virtual environment supplies the interpreter.
* **TypeScript/JavaScript:** an installed `node_modules/typescript` SDK governs
  selection. Classic SDKs use the pinned wrapper with the exact project's
  `lib/tsserver.js`; native SDKs run their own `bin/tsc --lsp --stdio`. A declared
  but missing SDK produces a repair message rather than changing compiler
  semantics. Yarn Plug'n'Play projects use the `.yarn/sdks/typescript` editor SDK
  (`yarn dlx @yarnpkg/sdks base`). Unconfigured projects use the pinned native artifact. Managed
  Node/npm supplies the wrapper and automatic type acquisition runtime.
* **Swift:** resolve SourceKit-LSP through the selected Xcode/Swift `xcrun`
  toolchain. A nearer SwiftPM package remains a package even when an Xcode project
  sits above it. Preserve existing custom `buildServer.json`. For new Xcode
  setups, a unique workspace or project selects SweetPad's standalone BSP;
  ambiguous containers use a persisted application-support selection. Only
  Shark-owned BSP configuration is regenerated after root or tool changes. A
  Shark-generated `buildServer.json` is added to the repository's local `info/exclude`.
* **Go:** find `go.work` before individual modules, respecting `GOWORK` (`off` or a
  file path) and `GOTOOLCHAIN` from the environment or `go env -w`.
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
executes a protocol smoke test, and emits an archive and checksum. The published
release asset is that unsigned build; app packaging signs the bundled copy and
recomputes its checksum after signing. The gopls pin is `GOPLS_VERSION` in
`src/lsp/tools.rs`; the Go pin is `GO_VERSION` in `scripts/build-gopls.sh`. The
release workflow builds and checks third-party tools in a job whose token cannot
write to the repository.

## Validation

`scripts/lsp-smoke.py` creates a temporary project and executes initialize → open
→ unsaved edit → completion/definition → diagnostics → close → shutdown. It
checks that the on-disk source remains unchanged. Ruff instead exercises its
supported formatting and diagnostics capabilities. Requests and process waits
are bounded; failures kill and reap the process group.

`scripts/check-language-tools.py` downloads and checks the exact manifest
artifacts with their managed runtime, plus the selected Rust/Swift toolchain
servers and SweetPad BSP. The release workflow runs this gate before signing or
publishing, including a strict zero-exit shutdown assertion.

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

The dated [performance baseline](linter-performance.md) documents the benchmark
workloads, commands, and measurement limits. It does not establish packaged-app
input-to-display latency or busy-indexer performance.

## Packaged-app release checks

Standalone artifact smokes and headless regressions do not replace these checks:

1. Verify all supported language backends through the final packaged app. The
   standalone artifact smokes and scoped native app checks do not prove
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

## Known limitations

- **Completion resolution and memory:** GPUI 0.8.0 exposes its completion menu,
  but not an accepted-item callback covering both keyboard and mouse acceptance.
  Shark therefore resolves the first 16 data-carrying completion items
  concurrently, waits at most about 50 ms, and presents the rest unresolved.
  Additional edits of resolved items, including imports, are folded into the
  primary edit so acceptance remains one undoable operation. Accepting an
  unresolved item resolves it then; its imports land as a second undo step, and
  only when they precede the accepted text and the lines above it are unchanged.
  A folded edit contains the text between its first and last affected ranges;
  many import candidates near the top of a large file can therefore consume
  memory proportional to candidate count multiplied by that text span. The
  four-MiB document limit does not bound aggregate completion-menu memory.
  Measure this workload and eager-resolution latency on named hardware before
  release. The next editor milestone should add an upstream GPUI accepted-item
  hook, then resolve and stage only the selected item's edits.
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
