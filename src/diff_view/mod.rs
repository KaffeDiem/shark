//! Readonly, virtualized patch display inspired by Diffs from Pierre.
//! Parse an externally supplied patch into [`DiffFile`]s, retain one
//! [`DiffState`] for all of them in the owner, and build [`Diff`] during rendering.
//!
//! This module is `src/diff/` of gpui-component 0.8.0 (crates.io, the version gpui-kit 0.8.0
//! pins; https://github.com/longbridge/gpui-kit), Copyright 2024 - 2026 Longbridge, licensed
//! under the Apache License 2.0 (`LICENSE-APACHE` here). Shark modified it; the changes:
//!
//! - Paths point at gpui-kit's public API (`gpui_kit::component`, `gpui_kit::base`); the
//!   actions live in the `shark_diff` namespace (gpui-component still registers its own
//!   `diff::*`); [`init`] binds the keys and Shark calls it from `bind_keys`. The `Diff.*`
//!   locale strings are English literals in `strings.rs`.
//! - Split ratio and draggable divider: `DiffState::split_ratio`/`set_split_ratio` (clamped
//!   0.2–0.8) and a 6 px col-resize strip between the columns; releasing emits
//!   `DiffEvent::SplitRatioChanged`.
//! - `Diff::side_headings` (a row under each expanded Split file header),
//!   `Diff::render_working_cell` (replaces the code of a modified-side line, keeping gutter and
//!   background), `Diff::accent_working` (2 px primary left border on that file's modified
//!   column) and `Diff::annotation_hints` (muted text in the other column's gap beside a line
//!   annotation).
//! - `DiffState::replace_file` swaps one file by path and splices its rows, keeping scroll,
//!   folds and the other files' state, and the old syntax/inline emphasis until the new one is
//!   prepared, so colors don't flash per keystroke. `show_line` expands the fold hiding a line
//!   and scrolls only when the row isn't fully in view; `scroll_top` reads the scroll position;
//!   `text_selection_lines` maps a mouse text selection to the lines it touches.
//! - Only code scrolls sideways: rows take the view's width and each line's code is shifted by
//!   the horizontal scroll and clipped to its column, which reaches as far as the narrower
//!   column needs; selection hit tests use the clipped bounds. Annotations fill their column or
//!   row and don't scroll.
//! - Each file's rows form a rounded, bordered card (`CARD_INSET`, `CARD_GAP`, `LIST_PAD`) with
//!   a tinted header bar; spacing follows Shark's design in whole pixels.
//! - A body resize found while painting defers its `notify` (GPUI drops one made during paint).
//! - The add-annotation button is drawn for the hovered line only, tracked in the state.
//! - The hunk separator reads "↑ N unchanged lines".

// A library's API: Shark uses part of it, and the tests reach for the rest. Upstream's style
// trips some of Shark's clippy lints; left as upstream wrote it.
#![allow(dead_code, unused_imports)]
#![allow(
    clippy::single_range_in_vec_init,
    clippy::type_complexity,
    clippy::collapsible_match,
    clippy::match_like_matches_macro
)]

#[macro_use]
mod strings;

mod conflict;
mod document;
mod parser;
#[cfg(test)]
mod parser_tests;
mod presentation;
mod selection;
mod state;
mod syntax;

pub use conflict::{DiffConflict, DiffConflictResolution};
pub use document::{DiffFile, DiffFileStatus, DiffLinePosition, DiffLineRange, DiffSide};
pub use parser::DiffParseError;
pub use presentation::DiffInlineUnit;
pub use state::{DiffEvent, DiffState};

use std::{collections::HashMap, rc::Rc, sync::Arc};

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::{
    AnyElement, App, Axis, ClickEvent, ClipboardItem, Context, ElementId, Entity, Focusable as _,
    HighlightStyle, Hsla, InteractiveElement as _, IntoElement, KeyBinding, ListOffset,
    MouseButton, MouseMoveEvent, ParentElement as _, Pixels, RenderOnce, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, TextRun, Window, div, list, point,
    prelude::FluentBuilder as _, px,
};

use actions::*;
use conflict::ConflictPart;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Copy, SelectAll},
    scroll::{ScrollableMask, Scrollbar},
    v_flex,
};
use presentation::FilePresentation;
use selection::{CodeText, SelectionLayer};
use state::{DisplayRow, FoldExpansion, LayoutKey, SelectedLines};

// Private: bindings serve the focused code body only.
mod actions {
    gpui_kit::actions!(
        shark_diff,
        [
            ScrollUp,
            ScrollDown,
            ScrollLeft,
            ScrollRight,
            PageUp,
            PageDown,
            First,
            Last,
            ExtendUp,
            ExtendDown
        ]
    );
}

/// gpui-component's `init` plus [`init`], what upstream's tests got from `crate::init`.
#[cfg(all(test, feature = "test-support"))]
fn test_init(cx: &mut App) {
    gpui_kit::component::init(cx);
    init(cx);
}

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", ScrollUp, Some("Diff")),
        KeyBinding::new("down", ScrollDown, Some("Diff")),
        KeyBinding::new("left", ScrollLeft, Some("Diff")),
        KeyBinding::new("right", ScrollRight, Some("Diff")),
        KeyBinding::new("pageup", PageUp, Some("Diff")),
        KeyBinding::new("pagedown", PageDown, Some("Diff")),
        KeyBinding::new("home", First, Some("Diff")),
        KeyBinding::new("end", Last, Some("Diff")),
        KeyBinding::new("shift-up", ExtendUp, Some("Diff")),
        KeyBinding::new("shift-down", ExtendDown, Some("Diff")),
        KeyBinding::new("secondary-c", Copy, Some("Diff")),
        KeyBinding::new("secondary-a", SelectAll, Some("Diff")),
    ]);
}

/// How a [`Diff`] presents the two sides of each file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiffMode {
    /// The original and modified source side by side.
    Split,
    /// One column, with deleted lines before added lines.
    #[default]
    Unified,
}

/// Which part of a row is emphasized while the pointer is over it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiffHoverHighlight {
    #[default]
    None,
    /// The whole line.
    Line,
    /// The line-number gutter.
    LineNumber,
    /// The line and its line number.
    Both,
}

/// How the start of each hunk is marked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiffHunkSeparator {
    /// The `@@` header, including the enclosing scope Git reports.
    #[default]
    Metadata,
    /// The count of lines the patch does not supply before the hunk.
    LineInfo,
    /// A thin divider.
    Simple,
}

/// How changed lines are marked beside the code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiffChangeIndicator {
    /// `+` and `−` signs.
    #[default]
    Signs,
    /// A colored bar.
    Bars,
    /// No marker.
    None,
}

/// Application-owned content attached to a source line or to a whole file.
///
/// The stable ID belongs to the annotation itself, while the target uses the
/// file path and, for a line, its side and one-based source line, independent
/// of folding.
#[derive(Clone)]
pub struct DiffAnnotation {
    id: ElementId,
    path: SharedString,
    position: Option<DiffLinePosition>,
}

impl DiffAnnotation {
    /// Attaches content beneath a source line.
    pub fn line(id: impl Into<ElementId>, position: DiffLinePosition) -> Self {
        Self {
            id: id.into(),
            path: position.path().clone(),
            position: Some(position),
        }
    }
    /// Attaches content beneath the header of the file at `path`.
    pub fn file(id: impl Into<ElementId>, path: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            position: None,
        }
    }
    pub fn id(&self) -> &ElementId {
        &self.id
    }
    /// The [`DiffFile::path`] of the annotated file.
    pub fn path(&self) -> &SharedString {
        &self.path
    }
    /// The annotated line, or `None` for a file annotation.
    pub fn position(&self) -> Option<&DiffLinePosition> {
        self.position.as_ref()
    }
}

type FileRenderer = Rc<dyn Fn(&DiffFile, &mut Window, &mut App) -> AnyElement>;
type AnnotationRenderer = Rc<dyn Fn(&DiffAnnotation, &mut Window, &mut App) -> AnyElement>;
type RangeHandler = Rc<dyn Fn(&DiffLineRange, &mut Window, &mut App)>;
type ClickHandler = Rc<dyn Fn(&DiffLinePosition, &ClickEvent, &mut Window, &mut App)>;
type HoverHandler = Rc<dyn Fn(Option<&DiffLinePosition>, &mut Window, &mut App)>;
type SideHeadings = Rc<dyn Fn(&DiffFile, &mut Window, &mut App) -> (SharedString, SharedString)>;
type WorkingCell = Rc<dyn Fn(&SharedString, usize, &mut Window, &mut App) -> Option<AnyElement>>;

#[derive(Default)]
struct HeaderSlots {
    content: Option<FileRenderer>,
    prefix: Option<FileRenderer>,
    title_suffix: Option<FileRenderer>,
    suffix: Option<FileRenderer>,
}

#[derive(Default)]
struct Annotations {
    items: Vec<DiffAnnotation>,
    lines: HashMap<DiffLinePosition, Vec<usize>>,
    files: HashMap<SharedString, Vec<usize>>,
}

/// A themed readonly code-review surface in split or unified mode.
///
/// All files of the supplied state share one virtualized list, each introduced
/// by its header. The component owns its scrolling, so give it a bounded
/// height. Long lines scroll horizontally unless [`Diff::soft_wrap`] is set;
/// file headers and review annotations are explicit slots, and application
/// commands remain owned by the application.
#[derive(IntoElement)]
pub struct Diff {
    state: Entity<DiffState>,
    style: StyleRefinement,
    line_number: bool,
    syntax_highlight: bool,
    header_visible: bool,
    hover_highlight: DiffHoverHighlight,
    hunk_separator: DiffHunkSeparator,
    change_indicator: DiffChangeIndicator,
    change_background: bool,
    soft_wrap: bool,
    header: HeaderSlots,
    annotations: Rc<Annotations>,
    annotation_content: Option<AnnotationRenderer>,
    on_add_annotation: Option<RangeHandler>,
    on_line_click: Option<ClickHandler>,
    on_line_hover: Option<HoverHandler>,
    side_headings: Option<SideHeadings>,
    working_cell: Option<WorkingCell>,
    accent: Option<SharedString>,
    annotation_hints: Option<[SharedString; 2]>,
}

fn file_renderer<F, E>(render: F) -> FileRenderer
where
    F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
    E: IntoElement,
{
    Rc::new(move |file, window, cx| render(file, window, cx).into_any_element())
}

impl Diff {
    pub fn new(state: &Entity<DiffState>) -> Self {
        Self {
            state: state.clone(),
            style: StyleRefinement::default(),
            line_number: true,
            syntax_highlight: true,
            header_visible: true,
            hover_highlight: DiffHoverHighlight::default(),
            hunk_separator: DiffHunkSeparator::default(),
            change_indicator: DiffChangeIndicator::default(),
            change_background: true,
            soft_wrap: false,
            header: HeaderSlots::default(),
            annotations: Rc::default(),
            annotation_content: None,
            on_add_annotation: None,
            on_line_click: None,
            on_line_hover: None,
            side_headings: None,
            working_cell: None,
            accent: None,
            annotation_hints: None,
        }
    }
    /// Shows the original and modified line-number lanes. Default is true.
    pub fn line_number(mut self, value: bool) -> Self {
        self.line_number = value;
        self
    }
    /// Emphasizes syntax once the state has prepared it. Default is true.
    pub fn syntax_highlight(mut self, value: bool) -> Self {
        self.syntax_highlight = value;
        self
    }
    /// Shows a header above each file. Default is true.
    pub fn header_visible(mut self, visible: bool) -> Self {
        self.header_visible = visible;
        self
    }
    /// Emphasizes the line, its number, or both under the pointer. Default is none.
    pub fn hover_highlight(mut self, highlight: DiffHoverHighlight) -> Self {
        self.hover_highlight = highlight;
        self
    }
    /// Marks the start of each hunk. Default is [`DiffHunkSeparator::Metadata`].
    pub fn hunk_separator(mut self, separator: DiffHunkSeparator) -> Self {
        self.hunk_separator = separator;
        self
    }
    /// Marks changed lines beside the code. Default is [`DiffChangeIndicator::Signs`].
    pub fn change_indicator(mut self, indicator: DiffChangeIndicator) -> Self {
        self.change_indicator = indicator;
        self
    }
    /// Tints changed lines. Default is true. Inline changes stay emphasized.
    pub fn change_background(mut self, value: bool) -> Self {
        self.change_background = value;
        self
    }
    /// Wraps long lines at the column width instead of scrolling horizontally.
    /// Default is false.
    pub fn soft_wrap(mut self, value: bool) -> Self {
        self.soft_wrap = value;
        self
    }
    /// Supplies annotations with stable identities and targets.
    ///
    /// Rows re-measure when annotations are added, removed or replaced, and
    /// whenever they are visible, so annotation content may change height freely.
    pub fn annotations(mut self, annotations: impl IntoIterator<Item = DiffAnnotation>) -> Self {
        let mut index = Annotations {
            items: annotations.into_iter().collect(),
            ..Annotations::default()
        };
        for (ix, annotation) in index.items.iter().enumerate() {
            match &annotation.position {
                Some(position) => index.lines.entry(position.clone()).or_default().push(ix),
                None => index
                    .files
                    .entry(annotation.path.clone())
                    .or_default()
                    .push(ix),
            }
        }
        self.annotations = Rc::new(index);
        self
    }
    /// Renders each supplied annotation. Application state and commands stay with the owner.
    pub fn render_annotation<F, E>(mut self, render: F) -> Self
    where
        F: Fn(&DiffAnnotation, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.annotation_content = Some(Rc::new(move |annotation, window, cx| {
            render(annotation, window, cx).into_any_element()
        }));
        self
    }
    /// Replaces the content of each file header, keeping its shell, separator
    /// and collapse control.
    pub fn render_header<F, E>(mut self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.header.content = Some(file_renderer(render));
        self
    }
    /// Inserts content before the path in the default header.
    pub fn render_header_prefix<F, E>(mut self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.header.prefix = Some(file_renderer(render));
        self
    }
    /// Inserts compact content immediately after the path in the default header.
    pub fn render_header_title_suffix<F, E>(mut self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.header.title_suffix = Some(file_renderer(render));
        self
    }
    /// Inserts trailing content after the change statistics in the default header.
    pub fn render_header_suffix<F, E>(mut self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.header.suffix = Some(file_renderer(render));
        self
    }
    /// Compatibility alias for [`Self::render_annotation`].
    pub fn annotation_content<F, E>(self, render: F) -> Self
    where
        F: Fn(&DiffAnnotation, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.render_annotation(render)
    }

    /// Compatibility alias for [`Self::render_header`].
    pub fn header<F, E>(self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.render_header(render)
    }

    /// Compatibility alias for [`Self::render_header_prefix`].
    pub fn header_prefix<F, E>(self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.render_header_prefix(render)
    }

    /// Compatibility alias for [`Self::render_header_title_suffix`].
    pub fn header_title_suffix<F, E>(self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.render_header_title_suffix(render)
    }

    /// Compatibility alias for [`Self::render_header_suffix`].
    pub fn header_suffix<F, E>(self, render: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> E + 'static,
        E: IntoElement,
    {
        self.render_header_suffix(render)
    }

    /// Shows an add button beside the hovered line and at the end of the
    /// selected lines. The handler receives the selected range when the line
    /// is part of it, or that line alone.
    pub fn on_add_annotation<F>(mut self, handler: F) -> Self
    where
        F: Fn(&DiffLineRange, &mut Window, &mut App) + 'static,
    {
        self.on_add_annotation = Some(Rc::new(handler));
        self
    }
    /// Handles a click on a line's code, outside a text-selection drag.
    pub fn on_line_click<F>(mut self, handler: F) -> Self
    where
        F: Fn(&DiffLinePosition, &ClickEvent, &mut Window, &mut App) + 'static,
    {
        self.on_line_click = Some(Rc::new(handler));
        self
    }
    /// Reports the line under the pointer, or `None` when the pointer leaves it.
    pub fn on_line_hover<F>(mut self, handler: F) -> Self
    where
        F: Fn(Option<&DiffLinePosition>, &mut Window, &mut App) + 'static,
    {
        self.on_line_hover = Some(Rc::new(handler));
        self
    }
    /// Titles the original and modified columns below each file header in Split mode.
    pub fn side_headings<F>(mut self, headings: F) -> Self
    where
        F: Fn(&DiffFile, &mut Window, &mut App) -> (SharedString, SharedString) + 'static,
    {
        self.side_headings = Some(Rc::new(headings));
        self
    }
    /// Replaces the code of a modified-side line, given the file path and the
    /// one-based line number, keeping its gutter, marker and background.
    /// Return `None` to show the line as usual.
    pub fn render_working_cell<F>(mut self, render: F) -> Self
    where
        F: Fn(&SharedString, usize, &mut Window, &mut App) -> Option<AnyElement> + 'static,
    {
        self.working_cell = Some(Rc::new(render));
        self
    }
    /// Marks the modified column of the file at `path` with an accent border, as being
    /// edited (Split mode).
    pub fn accent_working(mut self, path: Option<SharedString>) -> Self {
        self.accent = path;
        self
    }
    /// In Split mode, the text shown in the original column beside an annotation on the
    /// modified side (`on_original`), and in the modified column beside one on the original
    /// side (`on_modified`), centered in the gap the annotation leaves.
    pub fn annotation_hints(
        mut self,
        on_original: impl Into<SharedString>,
        on_modified: impl Into<SharedString>,
    ) -> Self {
        self.annotation_hints = Some([on_original.into(), on_modified.into()]);
        self
    }
}
impl Styled for Diff {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Diff {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.clone();
        state.update(cx, |state, cx| render_diff(self, state, window, cx))
    }
}

fn render_diff(
    props: Diff,
    state: &mut DiffState,
    window: &mut Window,
    cx: &mut Context<DiffState>,
) -> AnyElement {
    state.set_window(window.window_handle().window_id());
    state.ensure_presentation(cx);
    let rem = window.rem_size();
    let font_size = rem * (f32::from(cx.theme().mono_font_size) / 16.);
    let font_family = cx.theme().mono_font_family.clone();
    let row_height = font_size * 1.6;
    let digits = state
        .files()
        .iter()
        .map(|file| file.line_number_digits())
        .max()
        .unwrap_or(1);
    let gutter_width = if props.line_number {
        font_size * 0.7 * digits as f32 + rem
    } else {
        px(0.)
    };
    let mode = state.mode();
    let soft_wrap = props.soft_wrap;
    let layout = LayoutKey::new(
        rem,
        font_size,
        font_family.clone(),
        mode,
        props.line_number,
        soft_wrap,
    );
    if state.update_layout(layout, row_height) || state.measure_width {
        // Wrapped rows take the viewport width instead of their longest line.
        let width = if soft_wrap {
            px(0.)
        } else {
            measure_code_width(
                state.files(),
                font_size,
                &font_family,
                gutter_width,
                mode,
                window,
                cx,
            )
        };
        state.set_content_width(width);
    }
    state.sync_annotations(&props.annotations.items);
    // Cards, headers and annotations take the view's width (the longest line's before the first
    // paint); only code scrolls sideways, as far as the narrower column needs for the widest line.
    let viewport_width = state.viewport().width;
    let content_width = match viewport_width > px(0.) {
        true => viewport_width - CARD_INSET * 2.,
        false => state.content_width(),
    }
    .max(px(0.));
    let ratio = state.split_ratio();
    let left_width = content_width * ratio;
    let (needed, narrow) = match mode {
        DiffMode::Split => (
            state.content_width() * 0.5,
            content_width * ratio.min(1. - ratio),
        ),
        _ => (state.content_width(), content_width),
    };
    let overflow = match soft_wrap {
        true => px(0.),
        false => (needed - narrow).max(px(0.)),
    };
    let code = Rc::new(CodePresentation {
        files: state.files().to_vec(),
        presentations: state.presentations().to_vec(),
        mode,
        content_width,
        left_width,
        scroll_x: (-state.horizontal_scroll().offset().x).clamp(px(0.), overflow),
        font_size,
        row_height,
        gutter_width,
        rem,
        expansion_lines: state.expansion_lines(),
        line_number: props.line_number,
        syntax_highlight: props.syntax_highlight,
        header_visible: props.header_visible,
        hover_highlight: props.hover_highlight,
        hunk_separator: props.hunk_separator,
        change_indicator: props.change_indicator,
        change_background: props.change_background,
        soft_wrap,
        header: props.header,
        annotations: props.annotations.clone(),
        annotation_content: props.annotation_content,
        on_add_annotation: props.on_add_annotation,
        on_line_click: props.on_line_click,
        on_line_hover: props.on_line_hover,
        side_headings: props.side_headings,
        working_cell: props.working_cell,
        accent: props.accent,
        annotation_hints: props.annotation_hints,
        selection: state.selection().clone(),
        selected_lines: state.selected_lines(),
        selected: state.selected_span(),
        hovered: state.hovered.clone(),
        geometry: state.geometry().clone(),
        state: cx.entity(),
    });
    let rows = state.rows().clone();
    let scrollbar = state.list().clone();
    let horizontal = state.horizontal_scroll().clone();
    let has_rows = !rows.is_empty();
    let label = match state.files() {
        [file] => t!("Diff.Viewer", name = file.path().as_str()).to_string(),
        files => t!("Diff.ViewerFiles", count = files.len()).to_string(),
    };
    let body = div()
        .id("diff-body")
        .relative()
        .min_h_0()
        .min_w_0()
        .flex_1()
        // The scrollbar gets its own strip under the rows.
        .when(overflow > px(0.), |this| this.pb(px(16.)))
        // Only tracks the sideways scroll: the code rows shift their text by it. Without
        // GPUI's single-axis wheel remapping; the sibling mask owns X input.
        .child(
            div()
                .id("diff-horizontal")
                .absolute()
                .inset_0()
                .overflow_x_hidden()
                .track_scroll(&horizontal)
                .child(div().w(viewport_width + overflow).h(px(1.))),
        )
        .child(SelectionLayer::new(
            div()
                .size_full()
                .overflow_x_hidden()
                .child(gpui_kit::base::ScrollBounce::new(
                    "diff-bounce",
                    &scrollbar,
                    list(scrollbar.clone(), move |ix, window, cx| {
                        let row = &rows[ix];
                        let first = matches!(row, DisplayRow::File(_));
                        let last = rows
                            .get(ix + 1)
                            .is_none_or(|next| next.file() != row.file());
                        let (radius, border) = (px(8.), cx.theme().border);
                        // The rows of a file form one rounded card, with a gap between cards.
                        div()
                            .w_full()
                            .px(CARD_INSET - px(1.))
                            .when(first, |this| {
                                this.pt(if ix == 0 { LIST_PAD } else { CARD_GAP })
                            })
                            .when(ix + 1 == rows.len(), |this| this.pb(LIST_PAD))
                            .child(
                                div()
                                    .id(("diff-file", row.file()))
                                    .w_full()
                                    .border_x_1()
                                    .border_color(border)
                                    .when(first, |this| this.border_t_1().rounded_t(radius))
                                    .when(last, |this| this.border_b_1().rounded_b(radius))
                                    .child(render_row(row, code.clone(), window, cx)),
                            )
                            .into_any_element()
                    })
                    .size_full(),
                ))
                .into_any_element(),
            scrollbar.clone(),
            horizontal.clone(),
            state.selection().clone(),
            state.geometry().clone(),
        ))
        .when(has_rows && !soft_wrap, |this| {
            this.child(
                ScrollableMask::new(Axis::Horizontal, &horizontal).id("diff-horizontal-wheel"),
            )
        })
        .when(has_rows, |this| {
            this.child(Scrollbar::vertical(&scrollbar))
                .when(!soft_wrap, |this| {
                    this.child(Scrollbar::horizontal(&horizontal))
                })
        })
        // A line-number drag may finish anywhere, including outside the body.
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.end_line_selection(cx);
                this.end_divider_drag(cx);
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.end_line_selection(cx);
                this.end_divider_drag(cx);
            }),
        )
        .on_action(cx.listener(|this, _: &ScrollUp, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.list().scroll_by(-window.rem_size() * 1.5);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &ScrollDown, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.list().scroll_by(window.rem_size() * 1.5);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &ScrollLeft, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            let offset = this.horizontal_scroll().offset();
            this.horizontal_scroll()
                .set_offset(point(offset.x + window.rem_size() * 3., offset.y));
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &ScrollRight, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            let offset = this.horizontal_scroll().offset();
            this.horizontal_scroll()
                .set_offset(point(offset.x - window.rem_size() * 3., offset.y));
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &PageUp, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.list().scroll_by(-this.viewport().height * 0.9);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &PageDown, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.list().scroll_by(this.viewport().height * 0.9);
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &First, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.list().scroll_to(ListOffset::default());
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &Last, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.list().scroll_to_end();
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &ExtendUp, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.keyboard_select(-1, true, window, cx)
        }))
        .on_action(cx.listener(|this, _: &ExtendDown, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            this.keyboard_select(1, true, window, cx)
        }))
        .on_action(cx.listener(|this, _: &SelectAll, window, cx| {
            if body_focused(this, window, cx) {
                this.select_all(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &Copy, window, cx| {
            if !body_focused(this, window, cx) {
                return;
            }
            let text = this.selected_text(cx);
            if text.is_empty() {
                cx.propagate();
            } else {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }))
        .child(
            gpui_kit::canvas(
                {
                    let state = cx.entity();
                    move |bounds, _, cx| {
                        let changed = state.update(cx, |state, _| {
                            // Runs after the list's prepaint, which drops
                            // every row height when its width changes.
                            let rehinted = state.keep_row_height_hint();
                            // The estimate shapes the widest candidates only;
                            // grow to fit any wider line once it is painted.
                            let painted = if soft_wrap {
                                px(0.)
                            } else {
                                let painted = state.geometry().borrow().painted_width();
                                code_width(painted, gutter_width, rem, mode)
                            };
                            state.record_paint(bounds.size, painted) || rehinted
                        });
                        // A notify while painting is dropped; ask for the next frame after it.
                        if changed {
                            let state = state.clone();
                            cx.defer(move |cx| state.update(cx, |_, cx| cx.notify()));
                        }
                    }
                },
                {
                    // A divider drag follows the pointer outside the body too.
                    let state = cx.entity();
                    move |_, _, window, _| {
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase.bubble() && event.pressed_button == Some(MouseButton::Left) {
                                state.update(cx, |state, cx| {
                                    state.drag_divider(event.position.x, content_width, cx)
                                });
                            }
                        });
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .test_support()
        .track_focus(&state.focus_handle(cx))
        .key_context("Diff")
        .role(gpui_kit::accesskit::Role::Document)
        .aria_label(label);
    v_flex()
        .id(("diff", cx.entity_id()))
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .text_sm()
        .child(body)
        .refine_style(&props.style)
        .into_any_element()
}

fn body_focused(state: &DiffState, window: &Window, cx: &mut Context<DiffState>) -> bool {
    if state.focus_handle(cx).is_focused(window) {
        true
    } else {
        cx.propagate();
        false
    }
}

struct CodePresentation {
    files: Vec<DiffFile>,
    presentations: Vec<Option<Arc<FilePresentation>>>,
    mode: DiffMode,
    content_width: Pixels,
    /// The original column's width in Split mode; the modified one takes the rest.
    left_width: Pixels,
    /// How far the code is scrolled right.
    scroll_x: Pixels,
    font_size: Pixels,
    row_height: Pixels,
    gutter_width: Pixels,
    rem: Pixels,
    expansion_lines: usize,
    line_number: bool,
    syntax_highlight: bool,
    header_visible: bool,
    hover_highlight: DiffHoverHighlight,
    hunk_separator: DiffHunkSeparator,
    change_indicator: DiffChangeIndicator,
    change_background: bool,
    soft_wrap: bool,
    header: HeaderSlots,
    annotations: Rc<Annotations>,
    annotation_content: Option<AnnotationRenderer>,
    on_add_annotation: Option<RangeHandler>,
    on_line_click: Option<ClickHandler>,
    on_line_hover: Option<HoverHandler>,
    side_headings: Option<SideHeadings>,
    working_cell: Option<WorkingCell>,
    accent: Option<SharedString>,
    annotation_hints: Option<[SharedString; 2]>,
    selection: gpui_kit::base::TextSelectionHandle,
    selected_lines: Option<DiffLineRange>,
    selected: Option<SelectedLines>,
    hovered: Option<DiffLinePosition>,
    geometry: Rc<std::cell::RefCell<selection::SelectionGeometry>>,
    state: Entity<DiffState>,
}

impl CodePresentation {
    fn position(&self, file: usize, side: DiffSide, ix: usize) -> DiffLinePosition {
        let document = &self.files[file];
        DiffLinePosition::new(
            document.path().clone(),
            side,
            document.line_number(side, ix),
        )
    }

    fn is_selected(&self, file: usize, side: DiffSide, ix: usize) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|selected| selected.contains(&self.files, file, side, ix))
    }
}

/// Each file is a bordered card, this far in from each side of the list (gap and border).
const CARD_INSET: Pixels = px(17.);
const CARD_GAP: Pixels = px(12.);
/// Above the first card and below the last.
const LIST_PAD: Pixels = px(16.);

/// Total row width for code of `width` in the current mode.
fn code_width(width: Pixels, gutter_width: Pixels, rem: Pixels, mode: DiffMode) -> Pixels {
    if mode == DiffMode::Split {
        (width + gutter_width + rem * 2.) * 2.
    } else {
        width + gutter_width * 2. + rem * 2.
    }
}

/// Estimates content width from the lines widest in display cells, then
/// shapes those candidates with the code font for resolved geometry.
fn measure_code_width(
    files: &[DiffFile],
    font_size: Pixels,
    family: &SharedString,
    gutter_width: Pixels,
    mode: DiffMode,
    window: &mut Window,
    cx: &App,
) -> Pixels {
    // Glyph widths vary within a font, so shape several candidates rather
    // than trusting the single widest by cell count.
    const CANDIDATES: usize = 8;
    let mut candidates: Vec<(usize, &SharedString)> = Vec::with_capacity(CANDIDATES + 1);
    for file in files {
        for side in [DiffSide::Original, DiffSide::Modified] {
            for line in file.lines(side) {
                let cells = unicode_width::UnicodeWidthStr::width(line.display().as_str());
                if candidates.len() == CANDIDATES
                    && candidates
                        .last()
                        .is_some_and(|(widest, _)| *widest >= cells)
                {
                    continue;
                }
                let ix = candidates.partition_point(|(widest, _)| *widest >= cells);
                candidates.insert(ix, (cells, line.display()));
                candidates.truncate(CANDIDATES);
            }
        }
    }
    let font = gpui_kit::Font {
        family: family.clone(),
        ..Default::default()
    };
    let width = candidates
        .into_iter()
        .map(|(_, text)| {
            window
                .text_system()
                .shape_line(
                    text.clone(),
                    font_size,
                    &[TextRun {
                        len: text.len(),
                        font: font.clone(),
                        color: cx.theme().foreground,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width
        })
        .fold(px(0.), Pixels::max);
    code_width(width, gutter_width, window.rem_size(), mode)
}

fn render_row(
    row: &DisplayRow,
    code: Rc<CodePresentation>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match row {
        DisplayRow::File(file) => render_file_header(*file, &code, window, cx),
        DisplayRow::Notice(file) => render_notice(&code.files[*file], cx),
        DisplayRow::Hunk { file, hunk } => render_hunk_separator(*file, *hunk, &code, cx),
        DisplayRow::Fold { file, pairs } => render_fold(*file, pairs.clone(), &code, cx),
        DisplayRow::Conflict {
            file,
            conflict,
            part,
        } => render_conflict(*file, *conflict, *part, &code, cx),
        DisplayRow::Code {
            file,
            original,
            modified,
            changed,
        } => {
            if code.mode == DiffMode::Split && !code.files[*file].is_single_column() {
                let divider = render_divider(&code, cx);
                h_flex()
                    .relative()
                    .items_stretch()
                    .w_full()
                    .child(render_cell(
                        *file,
                        DiffSide::Original,
                        *original,
                        *modified,
                        *changed,
                        code.clone(),
                        window,
                        cx,
                    ))
                    .child(render_cell(
                        *file,
                        DiffSide::Modified,
                        *modified,
                        *original,
                        *changed,
                        code,
                        window,
                        cx,
                    ))
                    .child(divider)
                    .into_any_element()
            } else {
                let (side, ix) = match (original, modified) {
                    (_, Some(modified)) => (DiffSide::Modified, *modified),
                    (Some(original), None) => (DiffSide::Original, *original),
                    (None, None) => unreachable!("a code row shows at least one side"),
                };
                render_unified_cell(
                    *file,
                    side,
                    ix,
                    (*original, *modified),
                    *changed,
                    code,
                    window,
                    cx,
                )
            }
        }
    }
}

/// The column boundary of a Split row; dragging it resizes every file's columns.
fn render_divider(code: &CodePresentation, cx: &App) -> AnyElement {
    let state = code.state.clone();
    div()
        .id("split-divider")
        .test_support()
        .absolute()
        .top_0()
        .bottom_0()
        .left(code.left_width - px(3.))
        .w(px(6.))
        .cursor_col_resize()
        .hover(|this| this.bg(cx.theme().border))
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            // A drag on the divider is not a text selection.
            cx.stop_propagation();
            state.update(cx, |state, _| state.begin_divider_drag(event.position.x));
        })
        .into_any_element()
}

fn render_hunk_separator(
    file: usize,
    hunk: usize,
    code: &CodePresentation,
    cx: &App,
) -> AnyElement {
    let hunks = code.files[file].hunks();
    let row = h_flex()
        .w_full()
        .px(px(12.))
        .bg(cx.theme().muted.opacity(0.35))
        .text_xs()
        .text_color(cx.theme().muted_foreground);
    match code.hunk_separator {
        DiffHunkSeparator::Metadata => row
            .h(px(24.))
            .child(hunks[hunk].label().clone())
            .into_any_element(),
        DiffHunkSeparator::LineInfo => {
            let hidden = hunks[hunk].hidden_lines_before(hunk.checked_sub(1).map(|ix| &hunks[ix]));
            if hidden == 0 {
                return div().into_any_element();
            }
            row.h(px(24.))
                .child(t!("Diff.HiddenLines", count = hidden).to_string())
                .into_any_element()
        }
        DiffHunkSeparator::Simple => row.h_1().into_any_element(),
    }
}

fn render_fold(
    file: usize,
    pairs: std::ops::Range<usize>,
    code: &CodePresentation,
    cx: &App,
) -> AnyElement {
    let document = &code.files[file];
    // A fold bordering the hunk start has no source above it, and one
    // bordering the hunk end has none below; offer only the meaningful sides.
    let hunk = document
        .hunks()
        .iter()
        .find(|hunk| hunk.pairs().contains(&pairs.start))
        .map_or(0..document.pairs().len(), |hunk| hunk.pairs());
    let lines = code.expansion_lines;
    let partial = pairs.len() > lines;
    let expand = |id: (&'static str, usize), expansion: FoldExpansion| {
        let state = code.state.clone();
        let pairs = pairs.clone();
        Button::new(id)
            .ghost()
            .xsmall()
            .w_auto()
            .text_color(cx.theme().muted_foreground)
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| {
                    state.expand_fold(file, pairs.clone(), expansion, cx)
                })
            })
    };
    h_flex()
        .w_full()
        .h_6()
        .px_2()
        .gap_1()
        .bg(cx.theme().muted.opacity(0.35))
        .when(partial && pairs.start > hunk.start, |this| {
            this.child(
                expand(("expand-down", pairs.start), FoldExpansion::Down)
                    .icon(IconName::ArrowDown)
                    .accessibility_label(t!("Diff.ExpandBelow", count = lines).to_string()),
            )
        })
        .when(partial && pairs.end < hunk.end, |this| {
            this.child(
                expand(("expand-up", pairs.start), FoldExpansion::Up)
                    .icon(IconName::ArrowUp)
                    .accessibility_label(t!("Diff.ExpandAbove", count = lines).to_string()),
            )
        })
        .child(
            expand(("expand", pairs.start), FoldExpansion::All)
                .icon(if partial {
                    IconName::ChevronsUpDown
                } else {
                    IconName::ChevronDown
                })
                .label(t!("Diff.UnchangedLines", count = pairs.len()).to_string())
                .accessibility_label(t!("Diff.ExpandLines", count = pairs.len()).to_string()),
        )
        .into_any_element()
}

/// The heading of one part of a conflict, with the resolution commands on
/// the current part; a resolved conflict shows its choice and Undo instead.
fn render_conflict(
    file: usize,
    ix: usize,
    part: ConflictPart,
    code: &CodePresentation,
    cx: &App,
) -> AnyElement {
    let document = &code.files[file];
    let conflict = &document.conflicts()[ix];
    let resolution = code.state.read(cx).conflict_resolution(document.path(), ix);
    let choose = |id: &'static str, label: String, resolution: Option<DiffConflictResolution>| {
        let state = code.state.clone();
        Button::new((id, ix))
            .ghost()
            .xsmall()
            .w_auto()
            .label(label)
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| {
                    state.choose_conflict(file, ix, resolution, cx)
                })
            })
    };
    let row = h_flex()
        .w_full()
        .h_6()
        .px_3()
        .gap_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .bg(conflict_tint(Some(part), cx).opacity(0.25));
    if let Some(resolution) = resolution {
        let label = match resolution {
            DiffConflictResolution::Current => t!("Diff.AcceptedCurrent"),
            DiffConflictResolution::Incoming => t!("Diff.AcceptedIncoming"),
            _ => t!("Diff.AcceptedBoth"),
        };
        return row
            .bg(cx.theme().muted.opacity(0.35))
            .child(label.to_string())
            .child(choose(
                "undo-resolution",
                t!("Diff.UndoResolution").to_string(),
                None,
            ))
            .into_any_element();
    }
    let title = match part {
        ConflictPart::Current => t!("Diff.CurrentChange"),
        ConflictPart::Base => t!("Diff.BaseChange"),
        ConflictPart::Incoming => t!("Diff.IncomingChange"),
    };
    let label = conflict.label(part).clone();
    row.child(div().font_medium().child(title.to_string()))
        .when(!label.is_empty(), |this| this.child(label))
        .when(part == ConflictPart::Current, |this| {
            this.child(div().flex_1())
                .child(choose(
                    "accept-current",
                    t!("Diff.AcceptCurrent").to_string(),
                    Some(DiffConflictResolution::Current),
                ))
                .child(choose(
                    "accept-incoming",
                    t!("Diff.AcceptIncoming").to_string(),
                    Some(DiffConflictResolution::Incoming),
                ))
                .child(choose(
                    "accept-both",
                    t!("Diff.AcceptBoth").to_string(),
                    Some(DiffConflictResolution::Both),
                ))
        })
        .into_any_element()
}

/// The color that marks lines of a conflict part.
fn conflict_tint(part: Option<ConflictPart>, cx: &App) -> Hsla {
    match part {
        Some(ConflictPart::Current) => cx.theme().success,
        Some(ConflictPart::Incoming) => cx.theme().info,
        _ => cx.theme().muted_foreground,
    }
}

fn render_file_header(
    file: usize,
    code: &CodePresentation,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if !code.header_visible {
        return div()
            .children(file_annotations(file, code, window, cx))
            .into_any_element();
    }
    let document = &code.files[file];
    let foreground = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let collapsed = code.state.read(cx).is_file_collapsed(document.path());
    let content = if let Some(render) = &code.header.content {
        render(document, window, cx)
    } else {
        let name: SharedString = match (document.original_path(), document.modified_path()) {
            (Some(old), Some(new)) if old != new => format!("{old} → {new}").into(),
            _ => document.path().clone(),
        };
        let slot = |render: &Option<FileRenderer>, window: &mut Window, cx: &mut App| {
            render.as_ref().map(|render| render(document, window, cx))
        };
        let prefix = slot(&code.header.prefix, window, cx);
        let title_suffix = slot(&code.header.title_suffix, window, cx);
        let suffix = slot(&code.header.suffix, window, cx);
        let status = match document.status() {
            DiffFileStatus::Added => Some(t!("Diff.AddedFile")),
            DiffFileStatus::Deleted => Some(t!("Diff.DeletedFile")),
            DiffFileStatus::Renamed => Some(t!("Diff.RenamedFile")),
            DiffFileStatus::Copied => Some(t!("Diff.CopiedFile")),
            DiffFileStatus::Conflicted => Some(t!("Diff.ConflictedFile")),
            _ => None,
        };
        let statistics = !document.is_single_column();
        h_flex()
            .w_full()
            .min_w_0()
            .gap(px(10.))
            .children(prefix)
            .child(
                h_flex()
                    .min_w_0()
                    .flex_1()
                    .gap(px(10.))
                    .child(div().min_w_0().truncate().font_medium().child(name))
                    .when_some(title_suffix, |this, suffix| {
                        this.child(div().flex_shrink_0().child(suffix))
                    }),
            )
            .when(statistics, |this| {
                this.child(div().text_xs().text_color(foreground).child(format!(
                    "+{} −{}",
                    document.additions(),
                    document.deletions()
                )))
            })
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(foreground)
                        .child(status.to_string()),
                )
            })
            .children(suffix)
            .into_any_element()
    };
    let headings = code
        .side_headings
        .as_ref()
        .filter(|_| code.mode == DiffMode::Split && !collapsed && !document.is_single_column())
        .map(|headings| headings(document, window, cx));
    let state = code.state.clone();
    let name = document.path().clone();
    v_flex()
        .id("file-header")
        .w_full()
        .when(!collapsed, |this| this.border_b_1())
        .border_color(border)
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .rounded_t(px(7.))
                .when(collapsed, |this| this.rounded_b(px(7.)))
                .bg(cx.theme().muted.opacity(0.35))
                .h(px(40.))
                .px(px(12.))
                .gap(px(10.))
                .child(
                    Button::new("collapse-file")
                        .ghost()
                        .xsmall()
                        .icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .accessibility_label(
                            if collapsed {
                                t!("Diff.ExpandFile", name = name.as_str())
                            } else {
                                t!("Diff.CollapseFile", name = name.as_str())
                            }
                            .to_string(),
                        )
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| state.toggle_file_collapsed(file, cx))
                        }),
                )
                .child(div().flex_1().min_w_0().child(content)),
        )
        .when_some(headings, |this, (original, modified)| {
            this.child(
                h_flex()
                    .w_full()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(foreground)
                    .child(
                        div()
                            .w(code.left_width)
                            .flex_shrink_0()
                            .h(px(32.))
                            .line_height(px(32.))
                            .px(px(12.))
                            .border_r_1()
                            .border_color(border)
                            .truncate()
                            .child(original),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(32.))
                            .line_height(px(32.))
                            .px(px(12.))
                            .truncate()
                            .child(modified),
                    ),
            )
        })
        .children(file_annotations(file, code, window, cx))
        .into_any_element()
}

fn file_annotations(
    file: usize,
    code: &CodePresentation,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let path = code.files[file].path();
    let ixs = code
        .annotations
        .files
        .get(path)
        .cloned()
        .unwrap_or_default();
    annotation_elements(&ixs, code, window, cx)
}

/// Summarizes a file without source rows: binary, metadata-only or empty.
fn render_notice(file: &DiffFile, cx: &App) -> AnyElement {
    let message = if !file.has_changes() {
        t!("Diff.NoChanges")
    } else if file.is_binary() {
        t!("Diff.BinaryChanges")
    } else if !file.extended_headers().is_empty() {
        t!("Diff.NoTextChanges")
    } else {
        t!("Diff.EmptyFile")
    };
    v_flex()
        .w_full()
        .px_3()
        .py_2()
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .children(
            file.extended_headers()
                .iter()
                .cloned()
                .map(|line| div().child(line)),
        )
        .child(div().text_sm().child(message.to_string()))
        .into_any_element()
}

fn render_gutter(
    file: usize,
    side: DiffSide,
    ix: Option<usize>,
    code: &CodePresentation,
    cx: &App,
) -> AnyElement {
    let hover = matches!(
        code.hover_highlight,
        DiffHoverHighlight::LineNumber | DiffHoverHighlight::Both
    );
    div()
        .id(if side == DiffSide::Original {
            "old-gutter"
        } else {
            "new-gutter"
        })
        .flex_shrink_0()
        .w(code.gutter_width)
        .pr_1()
        .when(hover, |this| {
            this.hover(|this| this.bg(cx.theme().list_hover))
        })
        .when_some(ix, |this, ix| {
            let position = code.position(file, side, ix);
            let down = code.state.clone();
            let moved = code.state.clone();
            let dragged = position.clone();
            // A plain hit target: line numbers need no button chrome or state.
            this.child(
                div()
                    .id(("line", ix))
                    .test_support()
                    .w_full()
                    .h(code.row_height)
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .text_size(code.font_size)
                    .text_color(cx.theme().muted_foreground)
                    .hover(|this| this.text_color(cx.theme().foreground))
                    .role(gpui_kit::accesskit::Role::Button)
                    .aria_label(
                        t!(
                            "Diff.SelectLine",
                            side = side_label(side),
                            line = position.line()
                        )
                        .to_string(),
                    )
                    .child(position.line().to_string())
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                        down.update(cx, |state, cx| {
                            state.begin_line_selection(
                                position.clone(),
                                event.modifiers.shift,
                                window,
                                cx,
                            )
                        })
                    })
                    .on_mouse_move(move |event, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            moved.update(cx, |state, cx| {
                                state.drag_line_selection(dragged.clone(), cx)
                            })
                        }
                    }),
            )
        })
        .into_any_element()
}

fn side_label(side: DiffSide) -> std::borrow::Cow<'static, str> {
    if side == DiffSide::Original {
        t!("Diff.Original")
    } else {
        t!("Diff.Modified")
    }
}

#[allow(clippy::too_many_arguments)]
fn render_cell(
    file: usize,
    side: DiffSide,
    ix: Option<usize>,
    other_ix: Option<usize>,
    changed: bool,
    code: Rc<CodePresentation>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let cell = v_flex()
        .overflow_x_hidden()
        .w(if side == DiffSide::Original {
            code.left_width
        } else {
            code.content_width - code.left_width
        })
        .min_w_0()
        .flex_shrink_0()
        .when(side == DiffSide::Original, |this| {
            this.border_r_1().border_color(cx.theme().border)
        })
        .when(
            side == DiffSide::Modified && code.accent.as_ref() == Some(code.files[file].path()),
            |this| this.border_l_2().border_color(cx.theme().primary),
        )
        .when(ix.is_none(), |this| {
            this.bg(cx.theme().muted.opacity(0.3))
                .child(div().h(code.row_height))
        });
    // Beside an annotation on the other side only: a hint centered in the gap it leaves.
    let annotated = |side, ix: Option<usize>| {
        let lines = &code.annotations.lines;
        ix.is_some_and(|ix| lines.contains_key(&code.position(file, side, ix)))
    };
    let hint = (code.annotation_hints.as_ref())
        .filter(|_| annotated(side.other(), other_ix) && !annotated(side, ix))
        .map(|hints| {
            div()
                .id("annotation-hint")
                .test_support()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(hints[side as usize].clone())
        });
    let Some(ix) = ix else {
        return cell.children(hint).into_any_element();
    };
    cell.id((
        if side == DiffSide::Original {
            "old"
        } else {
            "new"
        },
        ix,
    ))
    .child(code_line(
        file,
        side,
        ix,
        changed,
        code.clone(),
        [Some((side, Some(ix))), None],
        window,
        cx,
    ))
    .children(line_extras(
        file, side, ix, other_ix, changed, &code, window, cx,
    ))
    .children(hint)
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_unified_cell(
    file: usize,
    side: DiffSide,
    ix: usize,
    pair: (Option<usize>, Option<usize>),
    changed: bool,
    code: Rc<CodePresentation>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (original, modified) = pair;
    let single_column = code.files[file].is_single_column();
    let counterpart = code.files[file].lines(side)[ix].counterpart();
    let other_ix = if side == DiffSide::Original {
        modified
    } else {
        original
    };
    let mut extras = line_extras(file, side, ix, counterpart, changed, &code, window, cx);
    if !changed && let Some(other_ix) = other_ix {
        extras.extend(line_annotations(
            file,
            side.other(),
            other_ix,
            &code,
            window,
            cx,
        ));
    }
    let gutters = if single_column {
        [Some((DiffSide::Modified, modified)), None]
    } else {
        [
            Some((DiffSide::Original, original)),
            Some((DiffSide::Modified, modified)),
        ]
    };
    v_flex()
        .w_full()
        .id((
            if side == DiffSide::Original {
                "old"
            } else {
                "new"
            },
            ix,
        ))
        .child(code_line(
            file,
            side,
            ix,
            changed,
            code.clone(),
            gutters,
            window,
            cx,
        ))
        .children(extras)
        .into_any_element()
}

/// Syntax colors with inline-change and selection emphasis layered over them.
#[allow(clippy::too_many_arguments)]
fn line_highlights(
    file: usize,
    side: DiffSide,
    ix: usize,
    changed: bool,
    selected: bool,
    status: Hsla,
    code: &CodePresentation,
    cx: &App,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let line = &code.files[file].lines(side)[ix];
    let presentation = code.presentations.get(file).and_then(Option::as_ref);
    let mut highlights = match presentation {
        Some(presentation) if code.syntax_highlight => {
            presentation.syntax_highlights(line, side, ix, cx.theme().highlight_theme.as_ref())
        }
        _ => Vec::new(),
    };
    if changed && let Some(presentation) = presentation {
        highlights.extend(presentation.inline_changes(line, side, ix).map(|range| {
            (
                range,
                HighlightStyle {
                    background_color: Some(status.opacity(0.3)),
                    ..Default::default()
                },
            )
        }));
    }
    // A replaced file keeps its old presentation until the new one is ready; drop ranges
    // that split a character of the new text.
    let display = line.display();
    highlights.retain(|(range, _)| {
        display.is_char_boundary(range.start) && display.is_char_boundary(range.end)
    });
    if selected {
        highlights.push((
            0..line.display().len(),
            HighlightStyle {
                background_color: Some(cx.theme().selection),
                ..Default::default()
            },
        ));
    }
    highlights
}

/// The add-annotation button, shown on hover and at the end of the selection.
fn add_annotation_button(
    file: usize,
    side: DiffSide,
    ix: usize,
    left: Pixels,
    code: &CodePresentation,
    cx: &App,
) -> Option<AnyElement> {
    let handler = code.on_add_annotation.clone()?;
    let position = code.position(file, side, ix);
    let range = code
        .selected_lines
        .clone()
        .filter(|_| code.is_selected(file, side, ix))
        .unwrap_or_else(|| {
            DiffLineRange::new(
                position.path().clone(),
                side,
                position.line(),
                position.line(),
            )
        });
    let at_selection_end = code
        .selected
        .as_ref()
        .is_some_and(|selected| selected.is_last(&code.files, file, side, ix));
    // Shown on the hovered line only: a hover style can outlive the pointer on rows that
    // aren't drawn again.
    if !at_selection_end && code.hovered.as_ref() != Some(&position) {
        return None;
    }
    // Over the change sign, clear of the code.
    Some(
        div()
            .id(("add-annotation", ix))
            .test_support()
            .absolute()
            .top((code.row_height - code.rem) / 2.)
            .left(left)
            .size(code.rem)
            .flex()
            .items_center()
            .justify_center()
            .rounded(cx.theme().radius)
            .bg(cx.theme().primary)
            .text_color(cx.theme().primary_foreground)
            .role(gpui_kit::accesskit::Role::Button)
            .aria_label(t!("Diff.AddAnnotation", line = position.line()).to_string())
            .child(Icon::new(IconName::Plus).xsmall())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| handler(&range, window, cx))
            .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn code_line(
    file: usize,
    side: DiffSide,
    ix: usize,
    changed: bool,
    code: Rc<CodePresentation>,
    gutters: [Option<(DiffSide, Option<usize>)>; 2],
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let document = &code.files[file];
    let line = &document.lines(side)[ix];
    let conflict_part = (document.status() == DiffFileStatus::Conflicted && changed)
        .then(|| {
            document
                .conflicts()
                .iter()
                .find(|conflict| conflict.source_lines().contains(&ix))
                .and_then(|conflict| conflict.part_of(ix))
        })
        .flatten();
    let status = if conflict_part.is_some() {
        conflict_tint(conflict_part, cx)
    } else if side == DiffSide::Original {
        cx.theme().danger
    } else {
        cx.theme().success
    };
    let selected = code.is_selected(file, side, ix)
        || (code.mode == DiffMode::Unified
            && !changed
            && gutters
                .iter()
                .flatten()
                .any(|(side, ix)| ix.is_some_and(|ix| code.is_selected(file, *side, ix))));
    let highlights = line_highlights(file, side, ix, changed, selected, status, &code, cx);
    // An unchanged Unified row shows one source line for both sides; follow
    // the side a text selection started on so its offsets stay consistent.
    let (text_side, text_ix) = if code.mode == DiffMode::Unified
        && !changed
        && let Some((selected_file, selected_side, _)) =
            selection::selected_source_range(&code.selection, cx)
        && selected_file == file
        && selected_side != side
        && let Some(other_ix) = line.counterpart()
    {
        (selected_side, other_ix)
    } else {
        (side, ix)
    };
    let position = code.position(file, text_side, text_ix);
    let source_line = &document.lines(text_side)[text_ix];
    // Wrapped text must be one run to break across the column; unwrapped
    // long lines stay chunked to bound shaping and selection projection.
    let ranges: Vec<_> = if code.soft_wrap {
        vec![0..source_line.display().len()]
    } else {
        source_line.chunk_ranges().collect()
    };
    // A block container gives wrapped text a definite width to break at.
    let text = div()
        .relative()
        .left(-code.scroll_x)
        .map(|this| {
            if code.soft_wrap {
                this.w_full()
            } else {
                this.flex().flex_row().flex_shrink_0()
            }
        })
        .children(ranges.into_iter().enumerate().map(|(chunk_ix, range)| {
            CodeText::new(
                ("code", chunk_ix).into(),
                document.clone(),
                file,
                text_side,
                text_ix,
                range,
                highlights.clone(),
                code.selection.clone(),
                code.geometry.clone(),
            )
        }));
    let working = (text_side == DiffSide::Modified)
        .then(|| {
            let render = code.working_cell.as_ref()?;
            render(document.path(), position.line(), window, cx)
        })
        .flatten();
    let gutters_width = if code.line_number {
        code.gutter_width * gutters.iter().flatten().count() as f32
    } else {
        px(0.)
    };
    let indicator = match code.change_indicator {
        DiffChangeIndicator::Signs => div()
            .w_4()
            .flex_shrink_0()
            .text_color(if changed {
                status
            } else {
                cx.theme().muted_foreground
            })
            .child(match (changed && conflict_part.is_none(), side) {
                (false, _) => " ",
                (true, DiffSide::Original) => "−",
                (true, DiffSide::Modified) => "+",
            }),
        DiffChangeIndicator::Bars => div().w_4().h_full().flex_shrink_0().when(changed, |this| {
            this.child(div().w(px(3.)).h_full().bg(status))
        }),
        DiffChangeIndicator::None => div().w_2().flex_shrink_0(),
    };
    let hover_line = matches!(
        code.hover_highlight,
        DiffHoverHighlight::Line | DiffHoverHighlight::Both
    );
    let add_button =
        add_annotation_button(file, side, ix, gutters_width + code.rem / 2., &code, cx);
    h_flex()
        .id("line-row")
        .relative()
        .w_full()
        .px_2()
        .map(|this| {
            if code.soft_wrap {
                this.min_h(code.row_height)
                    .items_start()
                    .whitespace_normal()
            } else {
                this.h(code.row_height).whitespace_nowrap()
            }
        })
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(code.font_size)
        .line_height(code.row_height)
        .when(changed && code.change_background, |this| {
            this.bg(status.opacity(0.12))
        })
        .when(selected, |this| this.bg(cx.theme().selection))
        .when(hover_line && !selected, |this| {
            this.hover(|this| this.bg(cx.theme().list_hover))
        })
        .on_hover({
            let (handler, state) = (code.on_line_hover.clone(), code.state.clone());
            let (position, at) = (position.clone(), code.position(file, side, ix));
            // Only the add-annotation button reads the hovered line; without it, don't redraw.
            let adds = code.on_add_annotation.is_some();
            move |hovered, window, cx| {
                state.update(cx, |state, cx| {
                    if adds && (*hovered || state.hovered.as_ref() == Some(&at)) {
                        state.hovered = hovered.then(|| at.clone());
                        cx.notify();
                    }
                });
                if let Some(handler) = &handler {
                    handler(hovered.then_some(&position), window, cx)
                }
            }
        })
        .when(code.line_number, |this| {
            this.children(
                gutters
                    .into_iter()
                    .flatten()
                    .map(|(side, ix)| render_gutter(file, side, ix, &code, cx)),
            )
        })
        .child(indicator)
        .child(
            div()
                .id("source")
                .test_support()
                // The code scrolls sideways within its column; the gutter stays.
                .flex_1()
                .min_w_0()
                .overflow_x_hidden()
                .role(gpui_kit::accesskit::Role::Label)
                .aria_label(source_line.text().clone())
                .aria_description(
                    t!(
                        if changed {
                            if side == DiffSide::Original {
                                "Diff.RemovedLine"
                            } else {
                                "Diff.AddedLine"
                            }
                        } else {
                            "Diff.SourceLine"
                        },
                        side = side_label(text_side),
                        line = position.line()
                    )
                    .to_string(),
                )
                .when_some(code.on_line_click.clone(), |this, handler| {
                    let position = position.clone();
                    this.on_click(move |event, window, cx| {
                        // A drag selects text; only a stationary press is a click.
                        if let ClickEvent::Mouse(mouse) = event {
                            let moved = mouse.up.position - mouse.down.position;
                            if moved.x.abs() > px(3.) || moved.y.abs() > px(3.) {
                                return;
                            }
                        }
                        handler(&position, event, window, cx)
                    })
                })
                .map(|this| match working {
                    Some(working) => this.child(working),
                    None => this.child(text),
                }),
        )
        .children(add_button)
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn line_extras(
    file: usize,
    side: DiffSide,
    ix: usize,
    other_ix: Option<usize>,
    changed: bool,
    code: &CodePresentation,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let mut extras = Vec::new();
    let document = &code.files[file];
    let line = &document.lines(side)[ix];
    let ending = &document.source(side)[line.content_end()..line.source().end];
    let ending_only = changed && has_line_ending_change(document, side, ix, other_ix);
    if ending.is_empty() || ending_only {
        let label = if ending.is_empty() {
            t!("Diff.NoNewline").to_string()
        } else {
            format!("\\ {}", if ending == "\r\n" { "CRLF" } else { "LF" })
        };
        extras.push(
            div()
                .h_6()
                .px_3()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label)
                .into_any_element(),
        );
    }
    extras.extend(line_annotations(file, side, ix, code, window, cx));
    extras
}

fn line_annotations(
    file: usize,
    side: DiffSide,
    ix: usize,
    code: &CodePresentation,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let position = code.position(file, side, ix);
    let ixs = code
        .annotations
        .lines
        .get(&position)
        .cloned()
        .unwrap_or_default();
    annotation_elements(&ixs, code, window, cx)
}

/// Annotations under a line or header.
fn annotation_elements(
    ixs: &[usize],
    code: &CodePresentation,
    window: &mut Window,
    cx: &mut App,
) -> Vec<AnyElement> {
    let Some(render) = &code.annotation_content else {
        return Vec::new();
    };
    ixs.iter()
        .map(|ix| {
            let annotation = &code.annotations.items[*ix];
            div()
                .id(annotation.id.clone())
                .px_3()
                .py_3()
                .border_t_1()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .child(render(annotation, window, cx))
                .into_any_element()
        })
        .collect()
}

fn has_line_ending_change(
    file: &DiffFile,
    side: DiffSide,
    ix: usize,
    counterpart: Option<usize>,
) -> bool {
    counterpart.is_some_and(|counterpart| {
        let other_side = side.other();
        let line = &file.lines(side)[ix];
        let other = &file.lines(other_side)[counterpart];
        line.text() == other.text()
            && file.source(side)[line.content_end()..line.source().end]
                != file.source(other_side)[other.content_end()..other.source().end]
    })
}

#[cfg(test)]
mod copy_tests;
#[cfg(all(test, feature = "test-support"))]
mod header_tests;
#[cfg(test)]
mod newline_tests;
#[cfg(all(test, feature = "test-support"))]
mod review_tests;
#[cfg(all(test, feature = "test-support"))]
mod scroll_tests;
#[cfg(test)]
mod selection_tests;
#[cfg(all(test, feature = "test-support"))]
mod split_tests;
#[cfg(all(test, feature = "test-support"))]
mod tests;
