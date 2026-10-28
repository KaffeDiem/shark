# Git feature and feature-refactor QA

QA date: 2026-10-28. Reviewed `905b752..1fe6751` on `git-feature`.
Four tracks: GitHub API boundary, Git interaction/drafts, feature-refactor
regressions, and integrated user-story/rendering validation. Three independent
agents worked alongside the coordinating agent. The original QA changed only
tests and this report. The follow-up fixes below also change production code.
No live PR was mutated.

## Latest test verification

The documented `cargo test --features test-support` command now passes with its
normal parallel runner: **405 passed, 0 failed, 15 opt-in tests ignored** (263.32
seconds). Native Metal rendering, `cargo build`, `cargo fmt --check`, and
`git diff --check` also pass. The initial serial rerun passed 404 tests before
the new isolation regression was added.

Investigation found that GitSync's bare `gh` calls bypassed `SHARK_GH`, so mocked
workspace tests could wait on real network requests. The shared bounded runner
now honors the existing override for bare `gh`, while preserving explicit
executables, error prefixes, and timeout behavior. A regression verifies those
properties. The PR fixtures now distinguish branch-specific queries, and their
environment guard restores the previous override.

Stack-test cleanup now detaches the chat reader before process teardown, reusing
the existing event-injection helper to prevent a real process thread from waking
the deterministic scheduler during cleanup. No timeout was increased and no test
was newly ignored.

The formerly failing stack and API tests passed in the complete run. After the
full run, the two PR fixtures were refined to return their actual parent PR for
`--head logic`; both affected tests passed again (4.65 and 1.38 seconds). The
earlier Python mock timeout did not recur, so no speculative Python/timeout fix
was added.

Logs: `/tmp/shark-verify-final-tests.log`, `/tmp/shark-verify-render.log`,
`/tmp/shark-verify-build.log`, and `/tmp/shark-verify-stack-*.log`.
The earlier failures below are retained as historical investigation evidence,
not the current test status. The ignored opt-in tests still require separate live
services or other explicit setup.

## Follow-up fixes

All four confirmed findings are fixed in the working tree:

| Finding | Fix | Focused verification |
| --- | --- | --- |
| Q1: comment focus | Deferred activation preserves the focused comment or suggestion; otherwise inline-editor/diff focus behavior is unchanged | Enabled regression verifies typing and coexistence with an inline editor; 9 boundary tests and `inline_edit_round2` pass |
| Q2: PR URL casing | Compare response owner/repository case-insensitively; preserve requested target/draft keys and strict host, PR-number and node-ID checks | Enabled casing regression plus wrong-identity cases; 12 API tests pass, 1 live test ignored |
| Q3: native test compilation | Fully qualify the eleven builtin test attributes and remove their conflicting imports | Native Metal harness runs directly from this working tree |
| Q4: inaccessible local drafts | Local drafts view reuses the existing composer and save worker without requiring remote details or permission; posting still requires the matching permitted destination | Offline recovery covers descriptions, comments, deleted-thread replies, copying, editing, discarding, baseline retention and cross-PR no-write guards; revoked-permission test covers all three draft kinds |

The three original failing regression tests are now enabled. Local recovery adds
no storage schema migration or dependency. The original audit evidence below is
retained as history; its descriptions of failing behavior refer to the pre-fix
revision.

Follow-up validation: the focused Git suite passed **27 tests**, with one opt-in
live test ignored. The expanded permission-loss regression also passed separately.
The previously rejected lowercase live URL now passes the read-only GitHub test
(1.46 seconds). `cargo build` and formatting checks pass. The unmodified native
rendering command passes on the working tree, including Local drafts at 800/1440 px
in dark/light themes; the narrow dark screenshot was visually inspected.

The complete follow-up serial run finished: **402 passed, 2 failed, 15 ignored**
in 800.15 seconds. All enabled regressions for Q1–Q4 passed. The two remaining
failures are recorded without suppressing them:

- `stacked_workspaces` timed out waiting for GitSync to attach parent PR metadata
  at `src/app/ui_tests.rs:8112`; its isolated retry also failed. This wait failed
  before the fixes as well. Stack/polling code is unchanged and none of the four
  modified behaviors is active in that workflow. The precise cause remains
  unresolved; the full suite must not be described as green.
- `ambiguous_writes_are_never_retried_and_resolution_is_idempotent` exceeded the
  existing 30-second subprocess deadline during its initial mock `gh` read,
  before any mutation/retry assertions. Its isolated rerun passed in 7.30 seconds.
  No identity assertion failed. Resource pressure is plausible, but the exact
  scheduling cause was not established.

Retry logs: `/tmp/shark-git-fixes-stacked-retry.log` and
`/tmp/shark-git-fixes-ambiguous-retry.log`. No test was newly ignored and no timeout
was increased to hide these failures.

Follow-up logs: `/tmp/shark-git-fixes-full-tests.log`,
`/tmp/shark-git-fixes-render-final.log`, `/tmp/shark-git-fixes-live-read.log`, and
`/tmp/shark-git-fixes-build.log`. Screenshots: `/tmp/shark-git-fixes-render/`.

## Initial assessment (before fixes)

The implementation substantially meets its documented **PR conversation** scope:
read descriptions and reviews, edit descriptions, comment, reply, resolve threads,
and retain local drafts across navigation. Existing workspace controls still own
push, PR creation, readiness, merge, and archive.

Do not sign off the refactor unchanged: a file-menu comment loses keyboard focus,
equivalent PR URLs can fail identity validation, and the documented native test
target does not compile. Draft recovery also needs a product decision: losing
remote access currently prevents accessing the local draft through Shark.

It is not a complete replacement for GitHub's review UI. Approve/request-changes,
reviewer management, and detailed checks are not offered by the new artifact.
Those are product-scope gaps, not claims that the refactor removed working features.

## Original confirmed findings

### Q1 — P2: file-menu Comment loses input focus (new refactor regression)

**Story:** Open a changed file, choose Comment, and immediately type the review.

`src/features/differ/diff.rs:1267` emits an asynchronous activation request, then
opens/focuses the comment input. When the request reaches the host,
`src/features/differ/mod.rs:305` activates Differ and focuses the diff, replacing
the intended textarea focus. Input reaches Diff shortcuts instead of the comment.
Before extraction, activation ran synchronously before the composer opened.

The new `qa_file_menu_comment_focuses_the_comment_input` test fails at its focus
assertion. Fix the activation/focus ordering; preserve the composer as the final
focus target. Keep the workspace-identity checks.

### Q2 — P2: equivalent PR URLs are rejected (new Git feature)

**Story:** Open an isolated review checkout using a valid GitHub PR URL.

`Target::from_url` normalizes the hostname but preserves owner/repository casing
(`src/pr/git_feature.rs:40`). `details_from` compares the resulting target
case-sensitively (`src/pr/git_feature.rs:281`). A canonical API URL can therefore
be rejected as a different pull request.

Live read-only reproduction: `https://github.com/kaffediem/shark/pull/60` failed
with “GitHub returned a different pull request”; the canonical
`https://github.com/KaffeDiem/shark/pull/60` passed in 1.70 seconds. Refreshing the
same explicit URL cannot repair this. The local
`metadata_accepts_canonical_repository_casing` test captures the mismatch.
Ordinary branch discovery usually receives a canonical URL already; the affected
path is a persisted/noncanonical explicit review URL, including casing changes
after the checkout's identity was saved.

Compare owner/repository identity without case sensitivity while retaining host,
PR-number, and node-ID checks. Consider draft-key consistency when normalizing.

### Q3 — P2: native rendering validation no longer compiles (refactor regression)

**Story:** Validate the refactored feature UI using the documented command.

`cargo test --features test-support --test rendering` fails with eleven E0659
ambiguous `#[test]` errors in moved Differ modules: `inline_edit.rs`, `media.rs`,
`review.rs`, and `review_share.rs`. The builtin test attribute and the
`gpui_kit::*` export conflict under `tests/rendering.rs`'s `include!` harness.
The explicit-import change in `1fe6751` introduced the conflicting resolution.

Qualifying those eleven test attributes in an isolated temporary copy allowed the
native harness to pass. This demonstrates a test-target compile defect; it does
not establish a production build failure. The workaround is not in this branch.

### Q4 — P2 usability gap: remote permissions hide local drafts

**Story:** Recover or copy an unfinished local comment after access changes.

Save a local comment, choose Keep draft, lock the PR and revoke update permission,
then refresh. The Resume comment control disappears and `start_comment` refuses
to reopen the draft. The text remains in `git-drafts/<workspace>.json`, but Shark
offers no way to read, copy, edit, or discard it.

The local-access paths share remote-write permission gates in
`src/features/git/mod.rs:368` and `:1412`; description drafts use analogous gates.
Source inspection also shows that an offline restart cannot open local drafts
until PR details load successfully. Data deletion was not observed.

Allow local draft recovery independently of current permission to post. Keep
remote mutation validation at submission. The new acceptance test is
`qa_git_saved_draft_accessible_after_permissions_revoked`.

## User-story coverage

| User wants to… | Assessment | Evidence or limitation |
| --- | --- | --- |
| Open Git once, close/reopen, reorder tabs, restore the workspace | Covered | UI lifecycle and persistence tests |
| Work across repositories without mixing drafts or writes | Covered | Exact PR/thread keys, repository/workspace switching and inactive-write tests |
| Read PR description, conversation, reviews and inline threads | Covered | API/UI fixtures, read-only live PR load, native rendering |
| Read a large Markdown message, search and copy its complete source | Covered locally | UTF-8 preview/full-source tests; visual inspection; no live large-PR latency guarantee |
| Edit title/body without overwriting an already changed description | Covered with API limitation | Stale-baseline detection, explicit replacement, restart tests; read/write race remains because there is no CAS |
| Post a comment or reply to a specific thread | Covered | Real subprocess boundary with mock `gh`, literal shell-like text, empty-input checks |
| Resolve/reopen only permitted threads | Covered in mocks | Fresh permission/identity validation, independent reopen permission, idempotent resolution |
| Retry after a network failure without losing work or duplicating writes | Covered with user responsibility | Draft retention and no automatic mutation retry; user must inspect GitHub after an ambiguous write |
| Recover drafts after disk errors or corrupt storage | Covered for persistence | Serialized saves, retry, protected original file, quit/archive guards |
| Recover drafts while offline or after losing permission | Fixed Q4 | Local drafts permits read/copy/edit/discard; remote posting remains gated |
| Jump from an inline thread to current code | Partial by design | Matching Diff file opens; missing/outdated code retains GitHub context; no exact thread-line navigation event |
| Use keyboard navigation without stealing input keystrokes | Covered in tested scenarios | Arrows/Vim/rail/large-list tests pass; Q1 focus regression is fixed |
| Keep existing Differ edits/reviews safe across workspace switches | Covered in tested scenarios | Stale refresh/menu/dialog, same-path buffer, publication, draft, and persistence tests |
| Finish a formal review with Approve or Request changes | Product gap | New artifact reads review states but does not submit these decisions |
| Push, create PR, mark ready, merge, archive | Available elsewhere | Existing workspace Git controls; no duplicate controls required in this artifact |

Staging/hunk staging, a commit-history browser, stash management, and a general
branch-management client are not part of the documented new artifact. Decide
explicitly whether the label “Git” is intended to promise those workflows. For
Shark's existing agent-oriented workflow, their absence alone is not a regression.

## Original QA verification and retained evidence

The initial focused Git run passed 22 tests with one opt-in test ignored. The
expanded API run passed 11 tests with two ignored. The six existing refactor
boundary tests passed. New passing checks cover pagination cursor failures,
deleted/duplicate nodes, PR node identity drift, distinct reopen permissions,
keyboard navigation after posting, stale discard dialogs, and viewed-mark event
ordering. New tests only were added in three existing test files.
All three new negative acceptance tests were executed and failed at their intended
assertions. `cargo build`, `cargo fmt --check`, and `git diff --check` passed.

The broad serial run exercised the application UI, including the 2,001-file /
50,000-line stress test. It reproduced Q1 and an intermittent existing
`stacked_workspaces` timeout. The user interrupted that run before its final
summary was collected, so this report does not claim an uninterrupted full-suite
pass. After resuming, the remaining partition (`--skip app::ui_tests`) completed:
**298 passed, 0 failed, 16 ignored** in 78.32 seconds. The last workspace-review
catalog test was also rerun successfully at 100, 500, and 2,000 workspaces.
These counts overlap the focused checks above and must not be added together.

The stacked-workspace test passed on the current revision in an isolated rerun
(11.13 seconds). On `905b752`, it first failed with a GPUI deterministic-scheduler
error, then passed (3.70 seconds). That baseline failure traces to the unchanged
agent reader thread at `src/agent.rs:481` waking a local task through
`send_blocking(Ev::Exited(...))`. This establishes preexisting test instability;
it does not explain the earlier UI timeout conclusively. No deterministic
stacked-workspace product regression is claimed.

Resumption logs are retained at `/tmp/shark-git-qa-remaining-tests.log`,
`/tmp/shark-git-qa-catalog.log`, and `/tmp/shark-git-qa-stacked-*.log`.

The three original failing acceptance tests were initially retained with explicit
QA ignore reasons. The follow-up fixes removed those ignores. Verify individually:

```sh
cargo test --features test-support qa_file_menu_comment_focuses_the_comment_input
cargo test --features test-support metadata_accepts_canonical_repository_casing
cargo test --features test-support qa_git_saved_draft_accessible_after_permissions_revoked
```

The native harness passed in the isolated copy after only test-attribute
qualification. It produced dark/light renders at 800/1440 px, including Git
overview, composer, activity, and full-text views, plus existing Differ/chrome
coverage. Representative screenshots were visually inspected. Temporary evidence:
`/tmp/shark-git-qa-render/`.

## Limits and follow-up validation

- The documented 2,000-thread single-subprocess benchmark supplies all threads in
  one mock response, although production requests `first:100`. It measures local
  payload handling, not realistic GitHub latency: roughly twenty page requests
  would be needed, plus extra requests for long reply chains. Add a paginated
  scale fixture before making end-to-end latency claims.
- Live mutation behavior was not exercised; comments, edits, replies and thread
  state changes use mock `gh`. One live read attempt hit a TLS handshake timeout;
  a later canonical read succeeded.
- Real GitHub Enterprise, archived-repository permissions, VoiceOver, and minimum
  window heights were not validated. Permission inference for locked/archived
  repositories warrants a separate real-account check; no unauthorized write was
  demonstrated.
- `.github/workflows/test.yml` currently checks formatting but does not run these
  behavioral suites. That is existing infrastructure, not a newly introduced
  failure.

Formal review decisions and other product-scope gaps remain unchanged; they were
not part of the confirmed defects addressed by these fixes.
