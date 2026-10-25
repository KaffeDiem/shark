# gpui-component 0.8.0, vendored

Source: the crates.io package `gpui-component` 0.8.0 (checksum
`4097e161cc81e86323b439664be0cdd4f7c56148e867ed6f67fd72b966b6c2d1`), the version
`gpui-kit` 0.8.0 pins. Licensed Apache-2.0 (`LICENSE-APACHE`). Shark uses it
through `[patch.crates-io]` in the root `Cargo.toml`.

Left out of the package: `.cargo-ok`, `.cargo_vcs_info.json`, `Cargo.toml.orig`.

## Patches

All in `src/diff/` (and `src/checkbox.rs`, one string in `locales/ui.yml`), tested in `src/diff/split_tests.rs` (new):

1. **Split ratio** (`state.rs`, `mod.rs`): `DiffState::split_ratio` /
   `set_split_ratio` (clamped 0.2–0.8, default 0.5). The original column is
   `content_width * ratio` and the modified one the rest; the content width is
   widened so the narrower column still fits the widest line.
2. **Draggable divider** (`mod.rs` `render_divider`, `state.rs`
   `begin/drag/end_divider_drag`): a 6 px col-resize strip at the column
   boundary of Split code rows. Dragging sets the ratio; releasing emits the new
   `DiffEvent::SplitRatioChanged`.
3. **Side headings** (`mod.rs`): `Diff::side_headings(Fn(&DiffFile, ..) ->
   (original, modified))`, a row under each expanded two-column file header in
   Split mode.
4. **Working-cell hook** (`mod.rs`): `Diff::render_working_cell(Fn(path, line,
   ..) -> Option<AnyElement>)` replaces the code of a modified-side line, keeping
   its gutter, marker and background. `code_line` takes the window for it.
5. **In-place file replacement** (`state.rs`): `DiffState::replace_file` swaps
   one file by path and splices the rows, keeping scrolling, collapse and the
   other files' state (`set_files` resets them) without measuring every line again (a
   longer line widens the rows when painted); `DiffState::show_line` expands
   the fold hiding a line and scrolls only as far as needed; `DiffState::scroll_top`
   reads the scroll position.
6. **Pinned file headers** (`mod.rs` `render_file_header`): rows are as wide as
   the longest line and scroll sideways, so a header's suffix sat at the far end
   of the content. The header bar now takes the body's painted width and is
   offset by the horizontal scroll, keeping prefix, name and suffix in view
   (`CodePresentation::viewport_width` / `scroll_x`). Tested by Shark's
   `card_header_pinned` UI test.
7. **Pinned annotations** (`mod.rs` `CodePresentation::pinned`, `annotation_elements`): line
   and file annotations (review threads, the composer) likewise keep to the visible part of
   their column, so only code scrolls sideways.
8. **File cards** (`mod.rs` list rows, `render_file_header`): each file's rows form one
   rounded, bordered card, 16 px in from the sides with 12 px between cards
   (`CARD_INSET`, `CARD_GAP`); the header has a tinted bar; the outer border is gone.
   `split_widths` is folded into `render_diff`.
9. **Repaint after a resize** (`mod.rs` body canvas): the paint that records a new body
   size asked for a new layout with `cx.notify()`, which GPUI drops while painting, so the
   rows kept the old width until something else redrew. It now defers the notify.
10. **Add-annotation button** (`mod.rs` `add_annotation_button`, `state.rs` `hovered`): drawn
    for the hovered line only (tracked in the state, not a group-hover style that rows not
    drawn again kept showing), one rem square over the change sign, clear of the code.
11. **Editing accent** (`mod.rs`): `Diff::accent_working(path)` gives that file's modified
    column a 2 px primary left border in Split mode.
12. **Unchecked checkbox** (`src/checkbox.rs`): its border uses the muted text color (70 %)
    instead of the input border, which was nearly invisible on dark themes.
13. **Text selection lines** (`state.rs`): `DiffState::text_selection_lines` gives the lines a
    mouse text selection over code touches, so an app can comment on them.
14. **Copy**: the hunk separator reads "↑ N unchanged lines" (`locales/ui.yml`
    `Diff.HiddenLines`).

The crate's own tests include files from the upstream monorepo
(`../../assets`, `../../../base`, `../../../../themes`, …) that the package
leaves out, so they don't build here as-is. To run the diff tests, mirror this
directory as `crates/gpui-component` with empty stand-ins for those files and
run `cargo test --lib --features test-support diff::` there (the feature lets the tests find
elements by id).

## Upgrading

Copy the new release over this directory, keep this file, and re-apply the
patches above (each is listed with its tests).
