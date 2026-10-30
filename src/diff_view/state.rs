use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    sync::Arc,
};

use gpui_kit::base::{
    TextSelection, TextSelectionContentKey, TextSelectionEvent, TextSelectionHandle,
    TextSelectionSnapshot,
};
use gpui_kit::{
    App, AppContext as _, Context, ElementId, EventEmitter, FocusHandle, Focusable, ListAlignment,
    ListOffset, ListState, Pixels, ScrollHandle, SharedString, Size, Subscription, Task, Window,
    WindowId, point, px,
};

use super::{
    DiffAnnotation, DiffFile, DiffLinePosition, DiffLineRange, DiffMode, DiffSide,
    presentation::{FilePresentation, PresentationOptions, SyntaxHighlighters},
    rows::{
        CONTEXT_LINES, DisplayRow, EXPANSION_LINES, FileRows, changed_groups, project_rows,
        source_rows,
    },
    selection::SelectionGeometry,
};

/// Which part of a collapsed unchanged range a disclosure reveals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FoldExpansion {
    /// The lines just above the following source.
    Up,
    /// The lines just below the preceding source.
    Down,
    All,
}

/// Notifications from the comparison surface.
#[derive(Clone, Debug)]
pub enum DiffEvent {
    /// The user finished dragging the Split divider; read the ratio with
    /// [`DiffState::split_ratio`].
    SplitRatioChanged(f32),
}

/// Inputs that determine row heights and content width.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutKey {
    rem: Pixels,
    font_size: Pixels,
    font_family: SharedString,
    mode: DiffMode,
}

impl LayoutKey {
    pub(crate) fn new(
        rem: Pixels,
        font_size: Pixels,
        font_family: SharedString,
        mode: DiffMode,
    ) -> Self {
        Self {
            rem,
            font_size,
            font_family,
            mode,
        }
    }
}

/// The selected source lines of one file, resolved for membership tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectedLines {
    /// Lines numbered `lines` on one side.
    Side {
        file: usize,
        side: DiffSide,
        lines: Range<usize>,
    },
    /// Patch lines in `patch` order, across both sides.
    Patch { file: usize, patch: Range<usize> },
}

impl SelectedLines {
    pub(crate) fn contains(
        &self,
        files: &[DiffFile],
        file: usize,
        side: DiffSide,
        ix: usize,
    ) -> bool {
        match self {
            Self::Side {
                file: selected,
                side: selected_side,
                lines,
            } => {
                *selected == file
                    && *selected_side == side
                    && lines.contains(&files[file].line_number(side, ix))
            }
            Self::Patch {
                file: selected,
                patch,
            } => *selected == file && patch.contains(&files[file].patch_ix(side, ix)),
        }
    }

    /// The last selected line, where a selection's add-annotation button sits.
    pub(crate) fn is_last(
        &self,
        files: &[DiffFile],
        file: usize,
        side: DiffSide,
        ix: usize,
    ) -> bool {
        match self {
            Self::Side {
                file: selected,
                side: selected_side,
                lines,
            } => {
                *selected == file
                    && *selected_side == side
                    && files[file].line_number(side, ix) + 1 == lines.end
            }
            Self::Patch {
                file: selected,
                patch,
            } => *selected == file && files[file].patch_ix(side, ix) + 1 == patch.end,
        }
    }
}

/// Retained focus, viewport, context expansion and selection for a
/// [`super::Diff`] showing one or more files of a patch.
///
/// Create once in the owning view. Mutations notify observers; the application
/// owns the patch and chooses when to install new files. Syntax and inline
/// emphasis are prepared on a background thread and appear once ready.
pub struct DiffState {
    files: Vec<DiffFile>,
    /// Each path's index in `files`; the first file wins a duplicated path.
    file_ixs: HashMap<SharedString, usize>,
    mode: DiffMode,
    context_lines: Option<usize>,
    expanded: Vec<Vec<Range<usize>>>,
    collapsed: HashSet<SharedString>,
    file_rows: Vec<FileRows>,
    header_rows: Vec<usize>,
    rows: Rc<Vec<DisplayRow>>,
    change_rows: Vec<usize>,
    list: ListState,
    horizontal_scroll: ScrollHandle,
    focus: FocusHandle,
    selection: TextSelectionHandle,
    geometry: Rc<RefCell<SelectionGeometry>>,
    selected_lines: Option<DiffLineRange>,
    selection_anchor: Option<DiffLinePosition>,
    selection_cursor: Option<DiffLinePosition>,
    selecting: bool,
    presentations: Vec<Option<Arc<FilePresentation>>>,
    /// Files replaced since their presentation was prepared; it is drawn until a new one is.
    stale_presentations: HashSet<usize>,
    viewport: Size<Pixels>,
    layout: Option<LayoutKey>,
    content_width: Pixels,
    /// Set when a replaced file may have changed the longest line, so the next render measures it.
    pub(crate) measure_width: bool,
    row_height: Option<Pixels>,
    /// The list width the row-height hint was last applied at.
    hinted_width: Option<Pixels>,
    split_ratio: f32,
    /// The pointer x and ratio where a divider drag started.
    divider_drag: Option<(Pixels, f32)>,
    /// The line under the pointer, which shows the add-annotation button.
    pub(crate) hovered: Option<DiffLinePosition>,
    annotations: HashSet<(Option<DiffLinePosition>, SharedString, ElementId)>,
    window: Option<WindowId>,
    /// Bumped when files change, so a preparation
    /// started for earlier ones never installs its results.
    presentation_generation: usize,
    preparing: Option<usize>,
    _presentation: Task<()>,
    _selection_subscription: Subscription,
}

/// Longest line, in bytes, compared for inline changes and given syntax emphasis.
const PRESENTATION_OPTIONS: PresentationOptions = PresentationOptions {
    inline_max_line_length: 1000,
    syntax_max_line_length: 1000,
};

impl EventEmitter<DiffEvent> for DiffState {}
impl Focusable for DiffState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DiffState {
    /// Creates a unified viewer with three unchanged lines around each change.
    pub fn new(files: impl IntoIterator<Item = DiffFile>, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        let selection = TextSelectionHandle::new("", cx);
        let geometry = Rc::new(RefCell::new(SelectionGeometry::default()));
        let weak = cx.entity().downgrade();
        selection.resolve_content_key_with(
            {
                let weak = weak.clone();
                let geometry = geometry.clone();
                move |point, cx| {
                    let state = weak.upgrade()?;
                    let entity = Some(state.read(cx).selection.entity_id());
                    let snapshot = state.read(cx).local_snapshot(cx);
                    let side = snapshot
                        .and_then(|snapshot| snapshot.anchor().content_key())
                        .map(|key| decode_key(key.value()).1);
                    geometry
                        .borrow()
                        .key_at(point, side)
                        .map(TextSelectionContentKey::new)
                        .or_else(|| {
                            snapshot
                                .filter(|snapshot| snapshot.cursor().entity_id() == entity)
                                .and_then(|snapshot| snapshot.cursor().content_key())
                        })
                }
            },
            cx,
        );
        selection.copy_with(
            {
                let weak = weak.clone();
                move |cx| {
                    weak.upgrade()
                        .map_or_else(String::new, |state| state.read(cx).selected_text(cx))
                }
            },
            cx,
        );
        selection.focus_with(
            {
                let focus = focus.clone();
                move |window, cx| window.focus(&focus, cx)
            },
            cx,
        );
        let _selection_subscription = selection.subscribe(
            move |event, cx| {
                let _ = weak.update(cx, |state, cx| {
                    if !state.selection.has_local_selection(cx) {
                        match event {
                            TextSelectionEvent::SelectionChanged(Some(_)) => {
                                state.selected_lines = None;
                                // A new text gesture ends the previous gutter anchor.
                                state.selection_anchor = None;
                                state.selection_cursor = None;
                            }
                            // A replacement gutter gesture has local selection and
                            // skips this branch. Keep its anchor for Shift-click.
                            TextSelectionEvent::Cleared => state.selected_lines = None,
                            _ => {}
                        }
                    }
                    cx.notify();
                });
            },
            cx,
        );
        let files = files.into_iter().collect::<Vec<_>>();
        let mut state = Self {
            expanded: vec![Vec::new(); files.len()],
            presentations: vec![None; files.len()],
            file_ixs: file_ixs(&files),
            files,
            mode: DiffMode::Unified,
            context_lines: Some(CONTEXT_LINES),
            collapsed: HashSet::new(),
            file_rows: Vec::new(),
            header_rows: Vec::new(),
            rows: Rc::default(),
            change_rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(0.)),
            horizontal_scroll: ScrollHandle::new(),
            focus,
            selection,
            geometry,
            selected_lines: None,
            selection_anchor: None,
            selection_cursor: None,
            selecting: false,
            stale_presentations: HashSet::new(),
            viewport: Size::default(),
            layout: None,
            content_width: px(0.),
            measure_width: false,
            row_height: None,
            hinted_width: None,
            split_ratio: 0.5,
            divider_drag: None,
            hovered: None,
            annotations: HashSet::new(),
            window: None,
            presentation_generation: 0,
            preparing: None,
            _presentation: Task::ready(()),
            _selection_subscription,
        };
        state.rebuild(true);
        state
    }

    /// Sets the initial display mode. Default is [`DiffMode::Unified`].
    pub fn with_mode(mut self, mode: DiffMode) -> Self {
        self.mode = mode;
        self.rebuild(true);
        self
    }

    /// The files in patch order.
    pub fn files(&self) -> &[DiffFile] {
        &self.files
    }
    pub fn mode(&self) -> DiffMode {
        self.mode
    }
    pub fn selected_lines(&self) -> Option<DiffLineRange> {
        self.selected_lines.clone()
    }
    /// Whether the file at `path` shows only its header.
    pub fn is_file_collapsed(&self, path: &str) -> bool {
        self.collapsed.contains(path)
    }

    pub(crate) fn rows(&self) -> &Rc<Vec<DisplayRow>> {
        &self.rows
    }
    pub(crate) fn list(&self) -> &ListState {
        &self.list
    }
    pub(crate) fn horizontal_scroll(&self) -> &ScrollHandle {
        &self.horizontal_scroll
    }
    pub(crate) fn selection(&self) -> &TextSelectionHandle {
        &self.selection
    }
    pub(crate) fn geometry(&self) -> &Rc<RefCell<SelectionGeometry>> {
        &self.geometry
    }
    pub(crate) fn presentations(&self) -> &[Option<Arc<FilePresentation>>] {
        &self.presentations
    }
    /// The body's size as of the last paint.
    pub(crate) fn viewport(&self) -> Size<Pixels> {
        self.viewport
    }
    /// The width rows need to show their longest line.
    pub(crate) fn content_width(&self) -> Pixels {
        self.content_width
    }
    pub(crate) fn set_content_width(&mut self, width: Pixels) {
        self.content_width = width;
        self.measure_width = false;
    }
    /// Records the painted body size and widest painted row. Returns whether
    /// either changed, so the frame must be laid out again.
    pub(crate) fn record_paint(&mut self, viewport: Size<Pixels>, painted_width: Pixels) -> bool {
        let wider = painted_width > self.content_width;
        if wider {
            self.content_width = painted_width;
        }
        let resized = self.viewport != viewport;
        self.viewport = viewport;
        wider || resized
    }

    /// The share of the width the original column takes in Split mode. Default is 0.5.
    pub fn split_ratio(&self) -> f32 {
        self.split_ratio
    }

    /// Sets the Split column ratio, clamped to 0.2–0.8, without emitting an event.
    pub fn set_split_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let ratio = ratio.clamp(0.2, 0.8);
        if self.split_ratio == ratio {
            return;
        }
        self.split_ratio = ratio;
        cx.notify();
    }

    pub(crate) fn begin_divider_drag(&mut self, x: Pixels) {
        self.divider_drag = Some((x, self.split_ratio));
    }

    /// Moves the divider to pointer `x` while a drag is active, over rows `width` wide.
    pub(crate) fn drag_divider(&mut self, x: Pixels, width: Pixels, cx: &mut Context<Self>) {
        if let Some((start, ratio)) = self.divider_drag
            && width > px(0.)
        {
            self.set_split_ratio(ratio + (x - start) / width, cx);
        }
    }

    /// Finishes a line-number or divider gesture, which may end anywhere.
    pub(crate) fn end_gesture(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        if self.divider_drag.take().is_some() {
            cx.emit(DiffEvent::SplitRatioChanged(self.split_ratio));
        }
    }

    /// Replaces the file with the same [`DiffFile::path`] in place, keeping
    /// scrolling, collapse, disclosures and selection, so an owner can show
    /// live edits. Disclosures and selection are clipped to the new file.
    pub fn replace_file(&mut self, file: DiffFile, cx: &mut Context<Self>) {
        let Some(ix) = self.file_ix(file.path()) else {
            return;
        };
        let path = file.path().clone();
        let pairs = file.pairs().len();
        let old = std::mem::replace(&mut self.files[ix], file);
        // Opened folds past the first changed pair move with their lines.
        let first = (old.pairs().iter())
            .zip(self.files[ix].pairs())
            .take_while(|(old, new)| old == new)
            .count();
        let delta = pairs as isize - old.pairs().len() as isize;
        let shift = |at: usize, moved: bool| {
            if moved {
                at.saturating_add_signed(delta)
            } else {
                at
            }
        };
        for range in &mut self.expanded[ix] {
            let (start, end) = (
                shift(range.start, range.start >= first),
                shift(range.end, range.end > first),
            );
            *range = start.min(pairs)..end.min(pairs);
        }
        self.expanded[ix].retain(|range| !range.is_empty());
        if let Some(range) = self.selected_lines.clone()
            && *range.path() == path
        {
            let clipped = self.clip_range(&range);
            if clipped.is_none() {
                self.selection_anchor = None;
                self.selection_cursor = None;
                self.selection.set_local_selection(false, cx);
            }
            self.selected_lines = clipped;
        }
        self.geometry.borrow_mut().clear();
        // Cancel a preparation of the old file; the others prepare again if pending. The old
        // presentation is drawn until the new one is ready, so colors don't flash per keystroke.
        self.cancel_presentation();
        self.stale_presentations.insert(ix);
        // No re-measure of every row (this runs per keystroke while editing); the next render
        // measures the longest line again, so the horizontal scroll range can shrink too.
        self.measure_width = true;
        // Spliced, so the list keeps its scroll offset and unchanged rows their heights. A
        // splice moves a top row inside the change to the change's start; keep it in place.
        let top = self.list.logical_scroll_top();
        if self.rebuild(false).contains(&top.item_ix) {
            self.list.scroll_to(ListOffset {
                item_ix: top.item_ix.min(self.rows.len() - 1),
                ..top
            });
        }
        self.ensure_presentation(cx);
        cx.notify();
    }

    /// The first visible row and how far it is scrolled.
    #[cfg(test)]
    pub fn scroll_top(&self) -> ListOffset {
        self.list.logical_scroll_top()
    }

    /// Reveals a source line, expanding its file or the fold hiding it, and
    /// scrolls only as far as needed to show it.
    pub fn show_line(&mut self, position: DiffLinePosition, cx: &mut Context<Self>) {
        if self.collapsed.remove(position.path().as_str()) {
            self.rebuild_at_anchor();
        }
        if let Some(row) = self.expand_line(&position) {
            self.reveal_row(row);
            cx.notify();
        }
    }

    /// Replaces the files and resets expansion, selection and viewport.
    /// Collapsed files stay collapsed when their path is still present.
    pub fn set_files(&mut self, files: impl IntoIterator<Item = DiffFile>, cx: &mut Context<Self>) {
        if let Some(window) = self.window
            && self.selection_is_local(cx)
        {
            TextSelection::clear_for_window(window, cx);
        }
        self.files = files.into_iter().collect();
        self.file_ixs = file_ixs(&self.files);
        self.collapsed
            .retain(|path| self.file_ixs.contains_key(path));
        self.expanded = vec![Vec::new(); self.files.len()];
        self.reset_presentation();
        self.layout = None;
        self.annotations.clear();
        self.selection.set_local_selection(false, cx);
        self.selection.set_fallback_copy_text("", cx);
        self.selected_lines = None;
        self.selection_anchor = None;
        self.selection_cursor = None;
        self.selecting = false;
        self.geometry.borrow_mut().clear();
        self.rebuild(true);
        self.horizontal_scroll.set_offset(point(px(0.), px(0.)));
        self.ensure_presentation(cx);
        cx.notify();
    }

    /// Changes the display mode while preserving source-line selection and a
    /// nearby source row.
    pub fn set_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.rebuild_at_anchor();
        self.horizontal_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Sets surrounding context. `None` shows every supplied patch line.
    #[cfg(test)]
    pub fn set_context_lines(&mut self, lines: Option<usize>, cx: &mut Context<Self>) {
        if self.context_lines == lines {
            return;
        }
        self.context_lines = lines;
        self.expanded.iter_mut().for_each(Vec::clear);
        self.rebuild_at_anchor();
        cx.notify();
    }

    /// Reveals all unchanged source while preserving the configured context count.
    #[cfg(test)]
    pub fn expand_unchanged(&mut self, cx: &mut Context<Self>) {
        for (expanded, file) in self.expanded.iter_mut().zip(&self.files) {
            *expanded = vec![0..file.pairs().len()];
        }
        self.rebuild_at_anchor();
        cx.notify();
    }

    /// Restores the configured context disclosures.
    #[cfg(test)]
    pub fn collapse_unchanged(&mut self, cx: &mut Context<Self>) {
        self.expanded.iter_mut().for_each(Vec::clear);
        self.rebuild_at_anchor();
        cx.notify();
    }

    /// Collapses the file at `path` to its header, or expands it, without
    /// emitting a user event.
    pub fn set_file_collapsed(&mut self, path: &str, collapsed: bool, cx: &mut Context<Self>) {
        let Some(file) = self.file_ix(path) else {
            return;
        };
        let path = self.files[file].path().clone();
        let changed = if collapsed {
            self.collapsed.insert(path)
        } else {
            self.collapsed.remove(&path)
        };
        if changed {
            self.rebuild_at_anchor();
            cx.notify();
        }
    }

    /// Synchronizes source-line selection without emitting a user event.
    /// Clips to supplied patch lines; a range containing none clears selection.
    pub fn set_selected_lines(&mut self, range: Option<DiffLineRange>, cx: &mut Context<Self>) {
        let range = range.and_then(|range| self.clip_range(&range));
        if self.selected_lines == range {
            return;
        }
        self.selection_anchor = range.as_ref().map(DiffLineRange::start_position);
        self.selection_cursor = range.as_ref().map(DiffLineRange::end_position);
        self.selection.set_local_selection(range.is_some(), cx);
        self.selected_lines = range;
        cx.notify();
    }

    /// Returns selected source without gutters, diff markers or annotation content.
    pub fn selected_text(&self, cx: &App) -> String {
        if self.selection.has_local_selection(cx)
            && let Some(selected) = self.selected_span()
        {
            return match selected {
                SelectedLines::Side { file, side, lines } => {
                    self.files[file].text_for_lines(side, lines.start, lines.end - 1)
                }
                SelectedLines::Patch { file, patch } => {
                    let file = &self.files[file];
                    let mut text = String::new();
                    for line in &file.patch_lines()[patch] {
                        // An unchanged line reads the same on both sides.
                        let (side, ix) = match line.line(DiffSide::Modified) {
                            Some(ix) => (DiffSide::Modified, ix),
                            None => (
                                DiffSide::Original,
                                line.line(DiffSide::Original).unwrap_or(0),
                            ),
                        };
                        // A side's unterminated final line can precede the
                        // other side's lines in patch order.
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&file.source(side)[file.lines(side)[ix].source()]);
                    }
                    text
                }
            };
        }
        let Some((file, side, range)) =
            super::selection::selected_source_range(&self.selection, cx)
        else {
            return String::new();
        };
        self.files
            .get(file)
            .and_then(|file| file.source(side).get(range))
            .unwrap_or("")
            .to_owned()
    }

    /// The lines a text selection made by dragging over code touches, on its one side.
    pub fn text_selection_lines(&self, cx: &App) -> Option<DiffLineRange> {
        let (file, side, range) = super::selection::selected_source_range(&self.selection, cx)?;
        source_lines(self.files.get(file)?, side, range)
    }

    /// Reveals and scrolls to a source line, expanding its file and hidden
    /// context if needed.
    pub fn scroll_to_line(&mut self, position: DiffLinePosition, cx: &mut Context<Self>) {
        if self.line_index(&position).is_none() {
            return;
        }
        if self.collapsed.remove(position.path().as_str()) {
            self.rebuild_at_anchor();
        }
        if self.expand_line(&position).is_some() {
            self.reveal_line(&position);
            cx.notify();
        }
    }

    /// Scrolls to the header of the file at `path`, a [`DiffFile::path`].
    pub fn scroll_to_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(item_ix) = self
            .file_ix(path)
            .and_then(|file| self.header_rows.get(file).copied())
        else {
            return;
        };
        self.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    /// Moves to the next changed group after the current viewport, across files,
    /// wrapping to the first once the list is scrolled to its end.
    pub fn next_change(&mut self, cx: &mut Context<Self>) {
        self.move_change(true, cx);
    }
    /// Moves to the previous changed group before the current viewport, across files.
    pub fn previous_change(&mut self, cx: &mut Context<Self>) {
        self.move_change(false, cx);
    }

    /// Reveals part or all of a collapsed unchanged range.
    pub(crate) fn expand_fold(
        &mut self,
        file: usize,
        pairs: Range<usize>,
        expansion: FoldExpansion,
        cx: &mut Context<Self>,
    ) {
        let lines = EXPANSION_LINES;
        let revealed = match expansion {
            FoldExpansion::All => pairs,
            FoldExpansion::Up => pairs.end.saturating_sub(lines).max(pairs.start)..pairs.end,
            FoldExpansion::Down => pairs.start..(pairs.start + lines).min(pairs.end),
        };
        if let Some(expanded) = self.expanded.get_mut(file) {
            expanded.push(revealed);
            self.rebuild_at_anchor();
            cx.notify();
        }
    }

    /// Toggles a file from its header.
    pub(crate) fn toggle_file_collapsed(&mut self, file: usize, cx: &mut Context<Self>) {
        let path = self.files[file].path().clone();
        let collapsed = !self.collapsed.contains(&path);
        self.set_file_collapsed(&path, collapsed, cx);
    }

    /// Starts a line-number selection gesture, extending the current anchor in
    /// the same file when `extend` is set.
    pub(crate) fn begin_line_selection(
        &mut self,
        position: DiffLinePosition,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.line_index(&position).is_none() {
            return;
        }
        let anchor = self
            .selection_anchor
            .clone()
            .filter(|anchor| extend && self.can_extend(anchor, &position))
            .unwrap_or_else(|| position.clone());
        TextSelection::clear(window, cx);
        window.focus(&self.focus, cx);
        self.selecting = true;
        self.select_user_range(anchor, position, cx);
    }

    /// Extends an active line-number gesture to `position` in the same file.
    pub(crate) fn drag_line_selection(
        &mut self,
        position: DiffLinePosition,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self.selection_anchor.clone() else {
            return;
        };
        if !self.selecting
            || !self.can_extend(&anchor, &position)
            || self.selection_cursor.as_ref() == Some(&position)
            || self.line_index(&position).is_none()
        {
            return;
        }
        self.select_user_range(anchor, position, cx);
    }

    /// Whether a selection anchored at `anchor` may extend to `position`:
    /// within one file, and across sides only in Unified mode, whose rows
    /// follow patch order.
    fn can_extend(&self, anchor: &DiffLinePosition, position: &DiffLinePosition) -> bool {
        anchor.path() == position.path()
            && (anchor.side() == position.side() || self.mode == DiffMode::Unified)
    }

    /// Selects one line, or extends to it, as a complete gesture.
    #[cfg(test)]
    pub(crate) fn click_line(
        &mut self,
        position: DiffLinePosition,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_line_selection(position, extend, window, cx);
        self.selecting = false;
    }

    pub(crate) fn keyboard_select(
        &mut self,
        direction: isize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((file, side)) = self.active_side(cx) else {
            return;
        };
        let count = self.files[file].lines_count(side);
        let ix = self
            .selection_cursor
            .as_ref()
            .filter(|position| {
                position.side() == side && self.file_ix(position.path()) == Some(file)
            })
            .and_then(|position| self.line_index(position))
            .map(|ix| ix.saturating_add_signed(direction).min(count - 1))
            .unwrap_or(0);
        let position = self.files[file].position(side, ix);
        let anchor = self
            .selection_anchor
            .clone()
            .filter(|anchor| extend && anchor.path() == position.path())
            .unwrap_or_else(|| position.clone());
        TextSelection::clear(window, cx);
        window.focus(&self.focus, cx);
        self.select_user_range(anchor, position.clone(), cx);
        if let Some(row) = self.expand_line(&position) {
            self.reveal_row(row);
        }
    }

    pub(crate) fn select_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((file, side)) = self.active_side(cx) else {
            return;
        };
        let count = self.files[file].lines_count(side);
        TextSelection::clear(window, cx);
        window.focus(&self.focus, cx);
        self.select_user_range(
            self.files[file].position(side, 0),
            self.files[file].position(side, count - 1),
            cx,
        );
    }

    /// The selected lines, resolved for membership tests.
    pub(crate) fn selected_span(&self) -> Option<SelectedLines> {
        let range = self.selected_lines.as_ref()?;
        let file = self.file_ix(range.path())?;
        if range.is_single_side() {
            let (start, end) = range.ordered();
            return Some(SelectedLines::Side {
                file,
                side: range.side(),
                lines: start..end + 1,
            });
        }
        let document = &self.files[file];
        let start = document.patch_ix(
            range.side(),
            document.line_index(range.side(), range.start())?,
        );
        let end = document.patch_ix(
            range.end_side(),
            document.line_index(range.end_side(), range.end())?,
        );
        Some(SelectedLines::Patch {
            file,
            patch: start.min(end)..start.max(end) + 1,
        })
    }

    /// Records the window that renders this state, for clearing its text selection.
    pub(crate) fn set_window(&mut self, window: WindowId) {
        self.window = Some(window);
    }

    /// Updates width and row-height measurement when fonts or layout change.
    /// Returns whether content width must be measured again.
    pub(crate) fn update_layout(&mut self, key: LayoutKey, row_height: Pixels) -> bool {
        if self.layout.as_ref() == Some(&key) {
            return false;
        }
        if self.layout.is_some() {
            self.list.remeasure();
        }
        self.layout = Some(key);
        self.row_height = Some(row_height);
        self.apply_row_height_hint();
        true
    }

    /// Re-measures rows whose annotations were added, removed or replaced.
    /// Visible rows are re-measured every frame, so content changes inside an
    /// annotation need no notification.
    pub(crate) fn sync_annotations(&mut self, annotations: &[DiffAnnotation]) {
        let current = annotations
            .iter()
            .map(|annotation| {
                (
                    annotation.position().cloned(),
                    annotation.path().clone(),
                    annotation.id().clone(),
                )
            })
            .collect::<HashSet<_>>();
        if current == self.annotations {
            return;
        }
        let rows = current
            .symmetric_difference(&self.annotations)
            .filter_map(|(position, path, _)| match position {
                Some(position) => self.source_row(position),
                None => self
                    .file_ix(path)
                    .and_then(|file| self.header_rows.get(file).copied()),
            })
            .collect::<Vec<_>>();
        for row in rows {
            self.list.remeasure_items(row..row + 1);
        }
        self.annotations = current;
    }

    fn file_ix(&self, path: &str) -> Option<usize> {
        self.file_ixs.get(path).copied()
    }

    fn line_index(&self, position: &DiffLinePosition) -> Option<usize> {
        self.files[self.file_ix(position.path())?].line_index(position.side(), position.line())
    }

    /// Clips a range to supplied lines, normalizing a single-side range.
    fn clip_range(&self, range: &DiffLineRange) -> Option<DiffLineRange> {
        let file = &self.files[self.file_ix(range.path())?];
        let first_at_or_after = |side: DiffSide, line: usize| {
            let lines = file.lines(side);
            lines
                .get(lines.partition_point(|source| source.line_number() < line))
                .map(|source| source.line_number())
        };
        let last_at_or_before = |side: DiffSide, line: usize| {
            let lines = file.lines(side);
            lines
                .partition_point(|source| source.line_number() <= line)
                .checked_sub(1)
                .map(|ix| lines[ix].line_number())
        };
        if range.is_single_side() {
            let (start, end) = range.ordered();
            let start = first_at_or_after(range.side(), start)?;
            let end = last_at_or_before(range.side(), end)?;
            return (start <= end)
                .then(|| DiffLineRange::new(range.path().clone(), range.side(), start, end));
        }
        let start = first_at_or_after(range.side(), range.start())
            .or_else(|| last_at_or_before(range.side(), range.start()))?;
        let end = last_at_or_before(range.end_side(), range.end())
            .or_else(|| first_at_or_after(range.end_side(), range.end()))?;
        Some(
            DiffLineRange::new(range.path().clone(), range.side(), start, end)
                .with_end_side(range.end_side()),
        )
    }

    /// The text selection's snapshot, if it is anchored in this state.
    fn local_snapshot(&self, cx: &App) -> Option<TextSelectionSnapshot> {
        let entity = Some(self.selection.entity_id());
        (self.selection.snapshot(cx)).filter(|snapshot| snapshot.anchor().entity_id() == entity)
    }

    fn selection_is_local(&self, cx: &App) -> bool {
        self.local_snapshot(cx).is_some()
    }

    /// The file and side that keyboard selection and Select All act on.
    fn active_side(&self, cx: &App) -> Option<(usize, DiffSide)> {
        let (file, side) = self
            .selection_cursor
            .as_ref()
            .and_then(|cursor| Some((self.file_ix(cursor.path())?, Some(cursor.side()))))
            .filter(|_| self.selected_lines.is_some())
            .or_else(|| {
                super::selection::selected_source_range(&self.selection, cx)
                    .map(|(file, side, _)| (file, Some(side)))
            })
            .or_else(|| {
                let top = self.list.logical_scroll_top().item_ix;
                self.rows.get(top).map(|row| (row.file(), None))
            })?;
        let document = self.files.get(file)?;
        let side = side.unwrap_or(if document.lines_count(DiffSide::Modified) > 0 {
            DiffSide::Modified
        } else {
            DiffSide::Original
        });
        (document.lines_count(side) > 0).then_some((file, side))
    }

    fn expand_line(&mut self, position: &DiffLinePosition) -> Option<usize> {
        let row = self.source_row(position)?;
        if let Some(DisplayRow::Fold { file, pairs }) = self.rows.get(row) {
            let (file, pairs) = (*file, pairs.clone());
            self.expanded[file].push(pairs);
            self.rebuild_at_anchor();
        }
        self.source_row(position)
    }

    /// Selects from `anchor` to `cursor` in one file.
    fn select_user_range(
        &mut self,
        anchor: DiffLinePosition,
        cursor: DiffLinePosition,
        cx: &mut Context<Self>,
    ) {
        self.selected_lines = Some(range_between(&anchor, &cursor));
        self.selection_anchor = Some(anchor);
        self.selection_cursor = Some(cursor);
        self.selection.set_local_selection(true, cx);
        cx.notify();
    }

    fn move_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top().item_ix;
        let groups = &self.change_rows;
        let target = if forward {
            // Scrolled to the end, the list clamps any later change below the
            // top, so the top never passes it and the key would stop there.
            // The last row's bounds answer even when the rows above the
            // viewport were never measured, as after a jump to the end.
            let at_end = self
                .list
                .item_count()
                .checked_sub(1)
                .and_then(|last| self.list.bounds_for_item(last))
                .is_some_and(|last| last.bottom() <= self.list.viewport_bounds().bottom());
            groups
                .get(groups.partition_point(|ix| *ix <= top))
                .copied()
                .filter(|_| !at_end)
                .or(groups.first().copied())
        } else {
            groups
                .partition_point(|ix| *ix < top)
                .checked_sub(1)
                .and_then(|ix| groups.get(ix))
                .copied()
                .or(groups.last().copied())
        };
        if let Some(ix) = target {
            self.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            });
            cx.notify();
        }
    }

    /// The source line shown by a row, looking past headers within its file.
    fn row_position(&self, ix: usize) -> Option<DiffLinePosition> {
        let row = self.rows.get(ix)?;
        let file = row.file();
        let pair_position = |original: Option<usize>, modified: Option<usize>| {
            modified
                .map(|ix| self.files[file].position(DiffSide::Modified, ix))
                .or_else(|| original.map(|ix| self.files[file].position(DiffSide::Original, ix)))
        };
        match row {
            DisplayRow::Code {
                original, modified, ..
            } => pair_position(*original, *modified),
            DisplayRow::Fold { pairs, .. } => self.files[file]
                .pairs()
                .get(pairs.start)
                .and_then(|pair| pair_position(pair.original(), pair.modified())),
            DisplayRow::File(_) | DisplayRow::Notice(_) | DisplayRow::Hunk { .. } => self
                .rows
                .get(ix + 1)
                .filter(|next| next.file() == file)
                .and_then(|_| self.row_position(ix + 1)),
        }
    }

    pub(crate) fn source_row(&self, position: &DiffLinePosition) -> Option<usize> {
        let rows = self.file_rows.get(self.file_ix(position.path())?)?;
        let rows = match position.side() {
            DiffSide::Original => &rows.original,
            DiffSide::Modified => &rows.modified,
        };
        rows.get(self.line_index(position)?).copied().flatten()
    }

    /// Scrolls `row` into view unless it is already. `scroll_to_reveal_item` alone scrolls
    /// back to a partly hidden top row even when `row` is in view below it.
    fn reveal_row(&self, row: usize) {
        let viewport = self.list.viewport_bounds();
        let visible = self.list.bounds_for_item(row).is_some_and(|bounds| {
            bounds.top() >= viewport.top() && bounds.bottom() <= viewport.bottom()
        });
        if !visible {
            self.list.scroll_to_reveal_item(row);
        }
    }

    fn reveal_line(&self, position: &DiffLinePosition) {
        if let Some(item_ix) = self.source_row(position) {
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(0.),
            });
        }
    }

    /// Rebuilds rows and restores the source line at the top of the viewport.
    fn rebuild_at_anchor(&mut self) {
        let anchor = self.row_position(self.list.logical_scroll_top().item_ix);
        self.rebuild(false);
        if let Some(anchor) = anchor {
            self.reveal_line(&anchor);
        }
    }

    /// Projects rows. Without `reset`, only the changed middle is spliced so
    /// measured heights of unchanged rows survive disclosure changes. Returns the old rows
    /// replaced.
    fn rebuild(&mut self, reset: bool) -> Range<usize> {
        let rows = project_rows(
            &self.files,
            &self.collapsed,
            self.mode,
            self.context_lines,
            &self.expanded,
        );
        self.file_rows = source_rows(&self.files, &rows);
        self.change_rows = changed_groups(&rows);
        self.header_rows = rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| matches!(row, DisplayRow::File(_)).then_some(ix))
            .collect();
        let spliced = if reset {
            self.list.reset(rows.len());
            0..self.rows.len()
        } else {
            let old = &self.rows;
            let prefix = old.iter().zip(&rows).take_while(|(a, b)| a == b).count();
            let suffix = old
                .iter()
                .rev()
                .zip(rows.iter().rev())
                .take(old.len().min(rows.len()) - prefix)
                .take_while(|(a, b)| a == b)
                .count();
            self.list
                .splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
            prefix..old.len() - suffix
        };
        self.rows = Rc::new(rows);
        self.apply_row_height_hint();
        spliced
    }

    /// Applies the row-height hint again once the list has laid out at a new
    /// width. Returns whether it did, so the frame must be laid out again.
    ///
    /// A list that lays out at a new width forgets every row height, hints
    /// included, which happens on its first layout too. The patch then
    /// measures only the rows rendered so far, and scrolling stops once the
    /// last of them reaches the bottom of the viewport. Call this after the
    /// list's prepaint, so the hint is back before the next wheel event.
    pub(crate) fn keep_row_height_hint(&mut self) -> bool {
        let width = self.list.viewport_bounds().size.width;
        if self.row_height.is_none() || self.hinted_width == Some(width) {
            return false;
        }
        self.hinted_width = Some(width);
        self.apply_row_height_hint();
        true
    }

    /// Gives unmeasured rows a code-row height so the scrollbar is sized for
    /// the whole patch before every row has been rendered.
    fn apply_row_height_hint(&self) {
        if let Some(height) = self.row_height {
            let _ = self.list.clone().with_uniform_item_height(height);
        }
    }

    /// Discards prepared emphasis after files change.
    fn reset_presentation(&mut self) {
        self.presentations = vec![None; self.files.len()];
        self.stale_presentations.clear();
        self.cancel_presentation();
    }

    /// Stops a preparation in flight, so it never installs its results.
    fn cancel_presentation(&mut self) {
        self.presentation_generation += 1;
        self.preparing = None;
        self._presentation = Task::ready(());
    }

    /// Prepares syntax and inline emphasis for files that lack it, one file at
    /// a time on the background executor, reusing highlighters per language.
    /// Starts on first render, so builders applied after `new` take effect.
    pub(crate) fn ensure_presentation(&mut self, cx: &mut Context<Self>) {
        let generation = self.presentation_generation;
        if self.preparing == Some(generation) {
            return;
        }
        let pending = self
            .presentations
            .iter()
            .enumerate()
            .filter(|(ix, presentation)| {
                presentation.is_none() || self.stale_presentations.contains(ix)
            })
            .map(|(ix, _)| (ix, self.files[ix].clone()))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        self.preparing = Some(generation);
        self._presentation = cx.spawn(async move |this, cx| {
            let mut highlighters = Some(SyntaxHighlighters::default());
            for (ix, file) in pending {
                let mut moved = highlighters.take();
                let (presentation, returned) = cx
                    .background_spawn(async move {
                        let presentation = FilePresentation::prepare(
                            &file,
                            PRESENTATION_OPTIONS,
                            moved.get_or_insert_default(),
                        );
                        (presentation, moved)
                    })
                    .await;
                highlighters = returned;
                let current = this.update(cx, |state, cx| {
                    if state.presentation_generation != generation {
                        return false;
                    }
                    if let Some(slot) = state.presentations.get_mut(ix) {
                        *slot = Some(Arc::new(presentation));
                        state.stale_presentations.remove(&ix);
                    }
                    cx.notify();
                    true
                });
                if !matches!(current, Ok(true)) {
                    return;
                }
            }
        });
    }
}

/// The range from `anchor` to `cursor`, normalized when both are on one side.
fn range_between(anchor: &DiffLinePosition, cursor: &DiffLinePosition) -> DiffLineRange {
    if anchor.side() == cursor.side() {
        DiffLineRange::new(
            cursor.path().clone(),
            cursor.side(),
            anchor.line().min(cursor.line()),
            anchor.line().max(cursor.line()),
        )
    } else {
        DiffLineRange::new(
            cursor.path().clone(),
            anchor.side(),
            anchor.line(),
            cursor.line(),
        )
        .with_end_side(cursor.side())
    }
}

fn file_ixs(files: &[DiffFile]) -> HashMap<SharedString, usize> {
    let mut ixs = HashMap::with_capacity(files.len());
    for (ix, file) in files.iter().enumerate() {
        ixs.entry(file.path().clone()).or_insert(ix);
    }
    ixs
}

const SIDE_BIT: u64 = 1 << 40;
const OFFSET_MASK: u64 = SIDE_BIT - 1;

/// Packs a source offset into a selection key, ordered by file, side and offset.
pub(crate) fn encode_key(file: usize, side: DiffSide, offset: usize) -> u64 {
    ((file as u64) << 41)
        | if side == DiffSide::Modified {
            SIDE_BIT
        } else {
            0
        }
        | (offset as u64 & OFFSET_MASK)
}
pub(crate) fn decode_key(key: u64) -> (usize, DiffSide, usize) {
    (
        (key >> 41) as usize,
        if key & SIDE_BIT == 0 {
            DiffSide::Original
        } else {
            DiffSide::Modified
        },
        (key & OFFSET_MASK) as usize,
    )
}

/// The lines of `side` that source bytes `range` touch.
pub(crate) fn source_lines(
    document: &DiffFile,
    side: DiffSide,
    range: Range<usize>,
) -> Option<DiffLineRange> {
    let lines = document.lines(side);
    let at = |byte: usize| lines.partition_point(|line| line.source().end <= byte);
    let (first, last) = (
        at(range.start),
        at(range.end.saturating_sub(1).max(range.start)),
    );
    (last < lines.len()).then(|| {
        DiffLineRange::new(
            document.path().clone(),
            side,
            lines[first].line_number(),
            lines[last].line_number(),
        )
    })
}
