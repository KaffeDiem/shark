# Task features and host boundaries

An agent defines reusable instructions and defaults; each task owns its isolated
work and review state. Diff, Git and Artifacts are task tabs. Home, All tasks and agent
pages are overview destinations, not another task or a shared agent worktree.
See the [manual](manual.md) for the user workflow.

`Shark` hosts navigation, chats, file editors, GitSync, persistence and
confirmation dialogs. A feature owns its GPUI entity, private implementation state,
rendering, controls, focus targets and local actions. It receives explicit context
and sends typed requests for work outside that boundary.

Feature implementations live in `src/features/differ/`, `src/features/git/` and `src/features/artifacts/`.
`src/app/features.rs` owns the rail, tabs, activation and focus dispatch;
`src/app/differ_host.rs` handles Differ's task integration. Shared styling,
dialogs and clipboard helpers live in `src/ui.rs`. Features import their
dependencies explicitly and do not depend on the `app` module.

Shared contracts live below their consumers: `review_protocol` owns review cards and
addressed-comment reports; `models` owns model labels and effort rules; `ui` owns
Markdown/link rendering and shared model/command menus. Feature-specific acceptance,
rendering state and delivery remain with the feature. Review line geometry comes from
the validated `DiffFile` patch model; narrowing to GitHub's context is review policy.

`pr` owns GitHub target identity and discovery. Publication and anchor verification take
an explicit `Target` captured from the discovered PR URL, including host and repository;
the working directory is not the publication target. GitSync consumes this shared policy
and owns its header/menu workflow.

## Adding a feature

1. Add its concrete variant to `Feature` in `src/features/mod.rs`, then add metadata
   and an entry in `Feature::ALL` in `src/app/features.rs`. The rail, clamped
   keyboard navigation and feature tab pills use that ordered list. Keep accessibility labels, test IDs and shortcuts
   in the metadata; do not duplicate launcher or pill rendering.
2. Give the component its own entity and modules under `src/features/`. Add concrete
   activation, content, controls, focus, Save and close dispatch in the host's
   feature methods. Expose
   operations and small status snapshots, rather than implementation fields.
3. Define its lifetime and persistence mapping explicitly. Runtime `Tab::Feature`
   and `Pane::Feature` do not change the existing serialized `TabId` variants,
   closed flags or restoration strings. Add storage and pending-write guards for
   retained entities, including those that are inactive.
4. Connect only the integration it needs through explicit snapshots and events.
   Keep task/editor coordination in the host. Tests should cover navigation,
   retention, stale asynchronous work and rejection of destructive operations.

## Differ, Git and Artifacts lifetimes

`src/features/differ/mod.rs` owns one long-lived Differ entity. Its child modules
own diff loading and refresh generations, tree/filter/layout, inline editing, review UI,
comments, publication results, media and subscriptions. Switching tasks resets
presentation and invalidates refreshes; comments and pending publication results
remain keyed by their originating task. Closing the tab keeps the entity,
comments and presentation, after the existing inline-edit discard guard runs.

Git remains lazy and cached per task. The host gives it the same effective
comparison bases as Differ (`Shark::effective_task`) through
`GitFeature::set_task`; GitSync changes reach it through `Shark::configure_git`.
Its Conversation and Commits views keep their state while the other is shown. Closing
its tab keeps drafts and pending writes. Quit and archive guards inspect retained Git entities even when another
task or feature is visible. Corrupt draft files remain protected from writes.

Artifacts lists and previews the task's `.context` folder. It is lazy and re-lists
on show, task switch, finished turns and a short poll while visible. It holds no unsaved
data, so it needs no storage, pending-write or archive guards. Closing its tab keeps
nothing that must be saved.

## Integration and safety

`src/app/differ_host.rs` translates host state into `TaskSnapshot`: task
metadata, effective GitSync comparison bases and PR context. Display preferences
start from persisted settings. Differ owns preference and viewed-mark changes and
emits persistence events; an older host snapshot must not overwrite a change before
its event runs. The host supplies Vim settings and the current editor path explicitly.

Differ emits task-qualified editor/chat navigation, edit, discard and
interaction requests. Git file events call `Differ::open_path`; Differ owns lookup,
selection and reveal behavior. Menus capture the task they were created for.

Overview navigation retains the open task and its dirty editors. Chat and Git
mutations on agent or All tasks pages require opening a task; task-row actions
resolve their explicit visible target.

Combined editor/inline-edit confirmation stays in the host. Its snapshots identify
the task, editor entity, path, buffer content, dirty state and pending save.
After a dialog, identities and content must still match before discarding. Inline
save completion and overwrite confirmation also check the absolute path and input
entity, so identical relative paths in different tasks cannot be confused.
Refresh completion requires both the originating task and current generation.

For review sends, Differ prepares a validated comment snapshot and intended
task/chat. The host checks that identity and asks Chat to accept the card.
Only acceptance queues the comments and starts optional publication. The host
updates the queue synchronously before GPUI delivers Chat's acknowledgement events.
Chat has no dependency on Differ. Publication completion updates the captured
task even if its tab is hidden or another task is active.

Plan storage retains save errors and protects files that failed to load. Retry, quit,
archive and chat-close guards include plan errors, including plans in inactive chats.
The stored plan and transcript formats remain unchanged.

The host discovers and caches language sessions; `FileEditor` owns detach, discovery
state and attachment transitions. It exposes read access and concrete operations rather
than mutable lifecycle fields. Harness adapters likewise own question normalization and
reply encoding; Chat uses the running backend for live events and the selected model
for the next process. Transcript record construction stays in `transcript`.

## Validation

App integration fixtures live under `src/app/ui_tests`; the native rendering
harness uses the same module. Differ exposes private-state access only through
its test-only `test_support` module. Boundary tests cover stale refreshes,
same-path task switches, stale discard/overwrite dialogs and stale menus.
Existing review tests cover refused sends, immediate and deferred delivery,
offscreen publication and draft corruption. Git and mixed-tab tests cover rail
focus, clamped navigation, task/tab drag boundaries and inactive write guards.

```sh
cargo test --features test-support -- --test-threads=1
cargo test --features test-support --test rendering -- --test-threads=1
cargo fmt --check
```
