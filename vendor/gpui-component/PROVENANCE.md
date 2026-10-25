# gpui-component 0.8.0, vendored

Source: the crates.io package `gpui-component` 0.8.0 (checksum
`4097e161cc81e86323b439664be0cdd4f7c56148e867ed6f67fd72b966b6c2d1`), the version
`gpui-kit` 0.8.0 pins. Licensed Apache-2.0 (`LICENSE-APACHE`). Shark uses it
through `[patch.crates-io]` in the root `Cargo.toml`.

Left out of the package: `.cargo-ok`, `.cargo_vcs_info.json`, `Cargo.toml.orig`.

## Patches

All in `src/diff/`, tested in `src/diff/split_tests.rs` (new):

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
   other files' state (`set_files` resets them); `DiffState::show_line` expands
   the fold hiding a line and scrolls only as far as needed.

The crate's own tests include files from the upstream monorepo
(`../../assets`, `../../../base`, `../../../../themes`, …) that the package
leaves out, so they don't build here as-is. To run the diff tests, mirror this
directory as `crates/gpui-component` with empty stand-ins for those files and
run `cargo test --lib diff::` there.

## Upgrading

Copy the new release over this directory, keep this file, and re-apply the
patches above (each is listed with its tests).
