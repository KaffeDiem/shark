# Right-pane features

`Shark` hosts workspace navigation, chats, file editors, GitSync, persistence and
confirmation dialogs. A feature owns its GPUI entity, private implementation state,
rendering, controls, focus targets and local actions. It receives explicit context
and sends typed requests for work outside that boundary.

## Adding a feature

1. Add its concrete variant and metadata to `Feature` in `src/app/features.rs`, and
   place it in `Feature::ALL`. The rail, clamped keyboard navigation and feature tab
   pills use that ordered list. Keep accessibility labels, test IDs and shortcuts
   in the metadata; do not duplicate launcher or pill rendering.
2. Give the component its own entity and modules. Add concrete activation, content,
   controls, focus, Save and close dispatch in the host's feature methods. Expose
   operations and small status snapshots, rather than implementation fields.
3. Define its lifetime and persistence mapping explicitly. Runtime `Tab::Feature`
   and `Pane::Feature` do not change the existing serialized `TabId` variants,
   closed flags or restoration strings. Add storage and pending-write guards for
   retained entities, including those that are inactive.
4. Connect only the integration it needs through explicit snapshots and events.
   Keep workspace/editor coordination in the host. Tests should cover navigation,
   retention, stale asynchronous work and rejection of destructive operations.

## Differ and Git lifetimes

`src/app/differ.rs` owns one long-lived Differ entity. Its child modules own diff
loading and refresh generations, tree/filter/layout, inline editing, review UI,
comments, publication results, media and subscriptions. Switching workspaces resets
presentation and invalidates refreshes; comments and pending publication results
remain keyed by their originating workspace. Closing the tab keeps the entity,
comments and presentation, after the existing inline-edit discard guard runs.

Git remains lazy and cached per workspace. Closing its tab keeps drafts and pending
writes. Quit and archive guards inspect retained Git entities even when another
workspace or feature is visible. Corrupt draft files remain protected from writes.

## Integration and safety

`src/app/differ_host.rs` translates host state into `WorkspaceSnapshot`: workspace
metadata, effective GitSync comparison bases and PR context. Display preferences
start from persisted settings. Differ owns preference and viewed-mark changes and
emits persistence events; an older host snapshot must not overwrite a change before
its event runs. The host supplies Vim settings and the current editor path explicitly.

Differ emits workspace-qualified editor/chat navigation, edit, discard and
interaction requests. Git file events call `Differ::open_path`; Differ owns lookup,
selection and reveal behavior. Menus capture the workspace they were created for.

Combined editor/inline-edit confirmation stays in the host. Its snapshots identify
the workspace, editor entity, path, buffer content, dirty state and pending save.
After a dialog, identities and content must still match before discarding. Inline
save completion and overwrite confirmation also check the absolute path and input
entity, so identical relative paths in different workspaces cannot be confused.
Refresh completion requires both the originating workspace and current generation.

For review sends, Differ prepares a validated comment snapshot and intended
workspace/chat. The host checks that identity and asks Chat to accept the card.
Only acceptance queues the comments and starts optional publication. The host
updates the queue synchronously before GPUI delivers Chat's acknowledgement events.
Chat has no dependency on Differ. Publication completion updates the captured
workspace even if its tab is hidden or another workspace is active.

## Validation

The private-state UI fixtures live under `src/app/differ/ui_tests`; `app::ui_tests`
reexports them for the native rendering harness. Boundary tests cover stale refreshes,
same-path workspace switches, stale discard/overwrite dialogs and stale menus.
Existing review tests cover refused sends, immediate and deferred delivery,
offscreen publication and draft corruption. Git and mixed-tab tests cover rail
focus, clamped navigation, every pointer-drag pairing and inactive write guards.

```sh
cargo test --features test-support
cargo test --features test-support --test rendering
cargo fmt --check
```
