# Language intelligence integration and release gates

This change retains GPUI and its 4 MiB editor limit. Language servers are shared by
project, backend, worktree, and toolchain identity. Tool setup and pipe I/O run away
from the UI thread. Project dependencies and compiler SDKs remain prerequisites.
This document records the implementation and evidence; it is not a claim that the
polished release or performance gates are complete.

## Using it

Open a Rust, Python, TypeScript/JavaScript, Swift, or Go source file in a workspace.
The editor starts its language session and reports Starting, Preparing index,
Ready, or Setup needed. Retry/restart uses the same selected backend.

| Action | Key |
|---|---|
| Format document | Option-Shift-F |
| Rename symbol | F2 |
| Find references | Shift-F12 |
| Document symbols | Command-Shift-O |
| Signature help | Command-Shift-Space |
| Navigate back / forward | Control-minus / Control-Shift-minus |

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
| Native TypeScript 7.0.2 macOS arm64 | Feature requests pass; strict shutdown gate fails |

Native TypeScript 7.0.2 rejects a `shutdown` request containing `params: null`;
omitting `params` is accepted. Shark's `lsp-server` serialization already omits
null params. With the accepted shutdown/exit sequence, this exact artifact still
exited with status 1 and `context canceled`. The smoke test intentionally retains
its zero-exit assertion. This is an observed release blocker, not an inferred
problem from an unrelated issue report. No checker fallback is performed.

`scripts/check-language-tools.py` downloads and checks the exact manifest
artifacts, exercises them with their managed runtime, and also checks the selected
Rust/Swift toolchain servers and SweetPad BSP. The release workflow runs this gate
before signing or publishing. The observed native TypeScript shutdown failure
intentionally blocks a release; the zero-exit assertion must not be weakened to
hide it.

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

## Gates still required before a polished release

The full headless regression run passed 443 tests (15 live-service tests ignored).
After the final integration fixes, the focused runs passed 33 LSP/protocol/setup
tests, 13 editor tests, and 2 workspace workflow tests. These counts overlap.
The workflow checks include multiple results, alias/tab reuse, history selection
and scroll, external-source read-only behavior, completion/import/snippet undo,
Vim undo/redo, atomic multi-file edits, dirty diff conflicts, and concurrent changes.
Formatting and whitespace checks also pass. No changes were published.

1. Resolve the native TypeScript shutdown failure and rerun all pinned artifacts
   using the final packaged app and final client capabilities. These standalone
   artifact smokes do not prove GPUI end-to-end behavior.
2. Exercise an empty-cache Finder launch, interrupted downloads during extraction
   and activation, moved worktrees with custom and Shark-owned BSP, ambiguous
   Xcode containers, real Xcode build/index readiness, and Go multi-module
   projects. Unit fixtures cover selection/cache invariants but do not replace
   these end-to-end runs.
3. Exercise the interactive editor workflows with Vim and concurrent agent writes:
   multiple definitions, back/forward selection and scroll, completion snippets
   plus imports, rename/format across open and closed files, dirty diff conflicts,
   restarts, and external dependency sources. Headless tests cover a subset.
4. Measure input-to-paint latency, cold readiness, warm completion/definition,
   and total process-tree memory with LSP disabled, idle, and indexing on named
   hardware. No performance or “fastest server” claim is justified by these runs.

Debugging, extension hosting, terminal integration, global cross-file undo, and a
general-purpose package manager remain out of scope. Ty remains an optional
benchmark candidate rather than another automatic backend.

## Implementation limits of the integration proof

The current implementation establishes the first integration milestone. It does
not establish that the polished five-language release passes every gate above.

- **Completion resolution and memory:** GPUI 0.8.0 exposes its completion menu,
  but not an accepted-item callback covering both keyboard and mouse acceptance.
  Shark therefore resolves data-carrying completion items in batches of at most
  eight requests before presenting the menu. Additional edits, including imports,
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
