# Linter user-flow and implementation QA — 2026-10-28

Scope: the shared language-intelligence feature introduced on the `linter`
branch (`origin/master..linter`), plus the editor/jump-bar changes. Existing uncommitted work was
preserved. Two independent three-agent teams reviewed user stories and then code
correctness, lifecycle, safety, reuse, and maintainability. Findings were checked
against callers and the installed GPUI implementation before fixes.

## User stories and fixes

| User task | Confirmed failure | Fix and regression evidence |
|---|---|---|
| Understand Python errors and lint warnings together | Concatenating checker and Ruff diagnostics produced an unordered GPUI diagnostic tree; earlier warnings disappeared from hover lookup. | Sort merged diagnostics by source range. A real-buffer test publishes unsorted primary diagnostics and an earlier companion warning, then looks up all three. |
| Fix the warning under the caret | An empty selection at the first character omitted the diagnostic from the code-action context. | Extend only the local lookup span; preserve the original server request range. Test first-character, interior, selection, outside, and Unicode coordinate conversion. |
| Recover after a server failure | A successful restart retained the old editor synchronization error and still displayed Setup needed. | Clear the old error on session-generation change. The fake-server restart regression checks the recovered status and document synchronization. |
| Repair an environment and press Retry | Retry reused the originally discovered interpreter/toolchain. Creating `.venv` or changing configuration did not update selection. | Rediscover affected editors and regroup sessions using the existing project identity mechanism. Test shared-editor retry, buffer preservation, and changed interpreter/session identity. |
| Continue editing while an agent renames/deletes folders | A descendant disappearing during a polling scan became a persistent setup error. | Ignore descendant NotFound races while preserving root and real I/O failures. Test disappearing traversal and deletion detection. |
| Return to a previous definition | A failed Back/Forward consumed the target and changed history despite remaining in the same file. | Commit history changes only after the target opens successfully. Test failed Back, restored target, and retry. |
| Keep working while a definition loads | A delayed navigation could replace a subsequently selected tab and steal focus. | Check the initiating workspace, editor, tab, and navigation request before applying the result. Test switching tabs before completion. |
| Browse a folder, then switch workspace | A stale breadcrumb menu could open the same relative path in a different workspace. | Check workspace identity before displaying and accepting the menu. Test a pending menu across workspace switching. |
| Fill adjacent empty completion placeholders | Typing into the first empty stop removed the next stop as though it were nested; typing in the next field could expand the previous field. | Preserve adjacent boundaries when removing nested stops and mapping edits. Test repeated typing, Unicode in the second field, returning to the first, and existing nested/mirrored placeholders. |
| Format or rename through a symlinked source file | Preflight found the aliased buffer, but application lost its identity, rejecting it as closed or creating another buffer. | Carry the matched open path through validation and application. Test editing the existing alias buffer without duplication or disk writes. |

These changes reuse the existing session cache, editor buffers, preflight, and
navigation paths. No new dependencies, compiler upgrades, automatic backend
fallbacks, or filesystem edit operations were added.

## Initial validation

The initial full headless run reported 449 passed, 2 failed, and 15 ignored.
The failures were the nested-project discovery test and the inline-edit row-height
test. Discovery tests now share the existing environment-test lock, preventing
concurrent PATH mutations from changing Cargo resolution or session-key assertions.
The current typography changes also constrain inline input height to the code row.
The Clippy-blocking non-octal permission literal in an existing unreadable-file test
was corrected without changing its permissions. `cargo clippy --features test-support
--all-targets` and `cargo check --all-targets` pass with existing non-blocking warnings.
Final checks on the compiled test binary:

| Check | Result |
|---|---|
| LSP protocol, discovery, installer, watcher, and provider tests (`lsp::`, one test thread) | 37 passed |
| App/editor language workflows (`intelligence_tests`, one test thread) | 15 passed |
| Snippet boundary, Unicode/mirror, and nested-placeholder tests | 3 passed |
| Clippy, all targets with test support | Passed with non-blocking warnings |
| Normal build, all targets without test support | Passed with non-blocking warnings |
| Rust formatting and whitespace checks | Passed |

The final full-suite attempt was stopped after 97 passing tests and one failure in
`bounded_gh_honors_override_and_preserves_explicit_programs`: the generated fake
`gh` script timed out before startup. The isolated test reproduced the same
five-second timeout. An independent Python reproduction, without Shark, showed a
fresh `#!/bin/sh` script taking over 20 seconds to execute directly, while
`/bin/sh script` and `/bin/sh -c 'echo explicit'` each completed in 0.01 seconds.
Some Rust test executables also stalled in `_dyld_start` before their test harness
started. This is a host execution problem; **the full suite is not claimed green**.
Only this QA run's stalled full-suite processes were stopped. No timeout or
assertion was weakened, and no other session's processes were stopped.

Retry coverage deliberately separates shared-editor detachment/cache invalidation
from actual project rediscovery; tests do not download/install a replacement
server. The artifact gate below separately executes the managed server binaries.

The actual-artifact gate (`python3 scripts/check-language-tools.py`) passed for
Pyrefly, Ruff, Pyright, classic TypeScript, rust-analyzer, SourceKit-LSP, gopls,
and SweetPad BSP. Fixtures exercise initialization, unsaved edits,
completion/definition (or Ruff formatting), diagnostics, close, and shutdown;
they verify that source files on disk remain unchanged. SweetPad separately
exercises BSP generation, initialization, build targets, and shutdown.

**Initial result (resolved by the follow-up below):** pinned native TypeScript 7.0.2 failed its
strict shutdown gate again. This run emitted `context canceled` and a panic in
`lspwatcher.Watcher.WatchFiles`, exiting with status 2. Other feature requests
completed. The zero-exit assertion and release-blocking behavior remain intact.
The follow-up found a transport finalization issue and fixed it without changing
the pin, backend, or strict zero-exit assertion.

## Follow-up: native UI, artifacts, host execution, and performance

A second three-agent team independently investigated host execution/full-suite
reliability, real TypeScript shutdown, and performance. Native UI checks used a
separate development app bundle launched through macOS LaunchServices, with its
own state directory, tool cache, and temporary Git repository. The source copy
changed only the state-home path and executable name for isolation; temporary
protocol tracing was confined to that copy. The user’s app and projects were not
modified by these checks. This was not a signed release-package certification.

Four additional product bugs were fixed:

- **Finish TypeScript shutdown:** after flushing the final `exit` notification,
  the client now ends its writer loop and closes server stdin. The smoke harness
  does the same. Native TypeScript now exits successfully; all nine actual-artifact
  gates pass, as do 40 repeated native TypeScript runs (30 with Shark’s exact
  client capabilities). A fake-peer test verifies shutdown → exit → EOF. Pins,
  timeouts, and zero-exit assertions are unchanged.
- **Keep editing after dismissing a menu:** GPUI propagates Escape after closing
  completion/quick-fix menus. Shark now consumes that first Escape before its
  leave-editor handler. A rendered regression covers both menus with Vim on/off;
  the first Escape preserves focus/text, and a later Escape can leave the editor.
  The native quick-fix flow was also retested successfully.

- **Open the file outline from the keyboard:** the global Command-Shift-O
  workspace-picker shortcut matched inside the editor and shadowed its outline
  shortcut. Excluding `FileEditor` from that global binding preserves workspace
  switching elsewhere and enables the documented outline command. A full-app
  key-dispatch regression verifies outline selection and navigation Back/Forward.

- **Populate Python outlines:** Shark parsed hierarchical symbols but did not
  advertise that capability. With its exact initialization payload, actual
  Pyrefly returned null in three runs; enabling hierarchical document-symbol
  support returned the expected function and variables in all three runs.
  The shared client now advertises the support it already implements. The final
  native rebuild opened the three-entry outline, navigated to the chosen function,
  and restored the original cursor with the Back button.

Native Python checks verified empty-cache managed Pyrefly/Ruff setup to Ready,
quick-fix/organize-imports, formatting an unsaved buffer, explicit Save, restart,
automatic completion presentation, cross-file rename, and four-result references
with navigation to the selected definition. Rename updated both open buffers;
the protocol trace corroborated two edits in each file. Language edits stayed
unsaved until Save. Pyrefly intentionally offers fewer type diagnostics in
unconfigured projects; adding `[tool.pyrefly]` to this test fixture enabled its
full checking policy. No production configuration policy was changed. A separate
concurrent-write check left one unsaved edit in the editor, changed the fixture
on disk, and pressed Save: the app offered Reload/Keep mine. Choosing Reload
preserved the external writer’s version and replaced the dirty buffer correctly.

The host’s standalone executable startup probe recovered without changing OS
settings: five new scripts started in 26–123 ms. The first recovered full suite
reported 459 passed, one failed, and 15 ignored. Its unrelated PR-cache lookup
test then passed in isolation and ten repeated runs; its assertion now reports
the unexpected lookup result rather than unwrapping it. No timeout was loosened.

The final full suite passed **463 tests, zero failures, 16 ignored** in 196.17
seconds. It includes the shutdown and Escape fixes, the outline shortcut
regression, and the hierarchical-symbol capability assertion. The focused
keyboard/workspace suites also passed all 22 tests; protocol tests passed 12/12.
Formatting, all-target compilation, Clippy, and whitespace checks pass with
existing warnings. The new ignored performance test accounts for the additional
ignored test. Two offline benchmark-harness tests also pass.

The [performance report](linter-performance.md) records reproducible editor CPU
frame timing, real Python-server cold/warm requests, server RSS, and one native
app-plus-servers idle snapshot on the named hardware. It distinguishes those
measurements from display latency and peak indexing memory.

## Coverage limits

Headless GPUI tests exercise real buffers, selections, key bindings, undo, tabs,
and controlled server messages. Artifact smokes run actual binaries separately.
The native development-app checks above cover Python workflows and an empty
cache. They do not cover every language in a signed packaged app, interruption
during extraction/activation, real Xcode build/index readiness, all Vim/concurrent
writer permutations, GPU presentation latency, or peak indexing memory. The
remaining release gates are listed in `language-intelligence.md`. Live-service
tests remain opt-in. Back/Forward keyboard dispatch passes the rendered test;
the native Back button passes, but the automation tool’s Control-minus input did
not produce a native navigation action, so native keyboard behavior is not
claimed verified.
