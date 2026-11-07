//! Diff review: a keyboard line cursor that survives refreshes, and the review queue: line
//! comments that stay pending until sent as one prompt (⇧C, review_send.rs), then show what
//! the agent made of them (Addressed / Outdated), and can go to GitHub (review_share.rs).
//!
//! The keyboard view: `j/k` move a cursor over a model of the diff's rows and scroll by
//! putting a row at the top with `scroll_to_line`; a refresh puts the same row back. Mouse
//! scrolling loses the model; `j/k` then scroll natively until the next jump. The comment
//! model is review_comment.rs, its persistence review_store.rs.
use super::review_comment::{Comment, Status, judge, lines_now, posted_as, side, slice};
use super::review_store::CommentStore;
use super::*;
use crate::diff_view::{
    CONTEXT_LINES, DiffAnnotation, DiffLinePosition, DiffLineRange, DiffSide, DisplayRow,
};
use crate::review_protocol::parse_reports;
use gpui_kit::component::input::{Enter, Escape, Textarea, TextareaState};
use std::cell::Cell;

/// One hunk's patch lines: kind (`b' '`, `b'-'`, `b'+'`), old and new one-based line numbers.
pub(crate) type Hunk = Vec<(u8, usize, usize)>;

/// Geometry from the same validated patch model used for display.
pub(crate) fn hunks(file: &DiffFile) -> Vec<Hunk> {
    file.patch_hunks()
        .map(|hunk| {
            hunk.iter()
                .map(|line| {
                    let old = line
                        .line(DiffSide::Original)
                        .map(|ix| file.line_number(DiffSide::Original, ix));
                    let new = line
                        .line(DiffSide::Modified)
                        .map(|ix| file.line_number(DiffSide::Modified, ix));
                    let kind = match (old, new) {
                        (Some(_), Some(_)) => b' ',
                        (Some(_), None) => b'-',
                        _ => b'+',
                    };
                    (kind, old.unwrap_or(0), new.unwrap_or(0))
                })
                .collect()
        })
        .collect()
}

/// The hunks git prints at its default context, for GitHub inline comments.
/// Local review uses the displayed rows, including expanded context.
pub(crate) fn narrow_hunks(hunks: Vec<Hunk>) -> Vec<Hunk> {
    let mut out = vec![];
    for hunk in hunks {
        let mut keep = vec![false; hunk.len()];
        for (i, _) in hunk.iter().enumerate().filter(|(_, l)| l.0 != b' ') {
            let end = (i + CONTEXT_LINES + 1).min(hunk.len());
            keep[i.saturating_sub(CONTEXT_LINES)..end].fill(true);
        }
        let mut run = vec![];
        for (line, keep) in hunk.into_iter().zip(keep) {
            if keep {
                run.push(line);
            } else if !run.is_empty() {
                out.push(std::mem::take(&mut run));
            }
        }
        if !run.is_empty() {
            out.push(run);
        }
    }
    out
}

/// A row the cursor can sit on: a file header or a source line.
#[derive(Clone, Debug, PartialEq)]
enum Spot {
    File(SharedString),
    Line(DiffLinePosition),
}

impl Spot {
    fn path(&self) -> &SharedString {
        match self {
            Spot::File(p) => p,
            Spot::Line(l) => l.path(),
        }
    }
}

#[derive(Debug)]
struct Row {
    spot: Spot,
    /// The original line also shown on this row: a context line, or a split pair's removal.
    old: Option<usize>,
    changed: bool,
    /// Height in code rows; headers and hunk separators are taller than a line.
    h: f32,
}

/// The displayed code rows, including opened context but skipping folds themselves.
fn rows(diff: &DiffState) -> Vec<Row> {
    let mut out = vec![];
    let mut gap = 0.;
    for row in diff.rows().iter() {
        let file = &diff.files()[row.file()];
        let (spot, old, changed, h) = match row {
            DisplayRow::File(_) => (Spot::File(file.path().clone()), None, false, 2.),
            DisplayRow::Code {
                original,
                modified,
                changed,
                ..
            } => {
                let position = modified
                    .map(|i| file.position(DiffSide::Modified, i))
                    .or_else(|| original.map(|i| file.position(DiffSide::Original, i)))
                    .unwrap();
                let old = original.map(|i| file.line_number(DiffSide::Original, i));
                (Spot::Line(position), old, *changed, 1.)
            }
            DisplayRow::Hunk { .. } | DisplayRow::Fold { .. } => {
                gap += 1.;
                continue;
            }
            DisplayRow::Notice(_) => continue,
        };
        out.push(Row {
            spot,
            old,
            changed,
            h: h + gap,
        });
        gap = 0.;
    }
    if let Some(last) = out.last_mut() {
        last.h += gap;
    }
    out
}

/// The row showing `spot`, or the closest one before it in the same file after a refresh.
fn find(rows: &[Row], spot: &Spot) -> usize {
    // An original line is on its own row, or beside the working line it pairs with.
    let before = |r: &Row, want: &DiffLinePosition| match &r.spot {
        Spot::Line(l) if l.side() == want.side() => l.line() <= want.line(),
        Spot::Line(_) if want.side() == DiffSide::Original => {
            r.old.is_some_and(|o| o <= want.line())
        }
        _ => false,
    };
    if let Some(i) = rows.iter().position(|r| {
        r.spot == *spot
            || matches!(spot, Spot::Line(w) if w.side() == DiffSide::Original
                && r.spot.path() == w.path() && r.old == Some(w.line()))
    }) {
        return i;
    }
    let Some(head) = rows
        .iter()
        .position(|r| r.spot == Spot::File(spot.path().clone()))
    else {
        return 0;
    };
    let Spot::Line(want) = spot else {
        return head;
    };
    let same_file = rows[head + 1..]
        .iter()
        .take_while(|r| matches!(&r.spot, Spot::Line(_)));
    same_file
        .enumerate()
        .filter(|(_, r)| before(r, want))
        .last()
        .map_or(head, |(i, _)| head + 1 + i)
}

const SCROLLOFF: f32 = 3.;

/// The top row that keeps `cur` on screen, `cap` code rows high, with a margin around it.
fn fit(rows: &[Row], top: usize, cur: usize, cap: f32) -> usize {
    let mut top = top.min(cur.saturating_sub(SCROLLOFF as usize));
    while top < cur && rows[top..=cur].iter().map(|r| r.h).sum::<f32>() + SCROLLOFF > cap {
        top += 1;
    }
    // Past the end the list stops scrolling; stay where it really is so `k` moves at once.
    let mut last = rows.len();
    let mut tail = 0.;
    while last > 0 && tail + rows[last - 1].h <= cap {
        last -= 1;
        tail += rows[last].h;
    }
    top.min(last)
}

/// Index of the next (or previous) start of a run of changed rows, wrapping around.
fn next_change(rows: &[Row], cur: usize, forward: bool) -> Option<usize> {
    let starts: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].changed && (i == 0 || !rows[i - 1].changed))
        .collect();
    match forward {
        true => starts.iter().find(|&&i| i > cur).or(starts.first()),
        false => starts.iter().rev().find(|&&i| i < cur).or(starts.last()),
    }
    .copied()
}

/// The inline composer (A·01): under the commented lines, on their side.
pub(crate) struct Composer {
    /// The comment being edited; id 0 for a new one on its lines.
    pub(crate) draft: Comment,
    pub(crate) input: Entity<TextareaState>,
    /// Suggest change (cmd-shift-s): the lines, to edit into the proposed code.
    pub(crate) suggest: Option<Entity<TextareaState>>,
    /// Its height as last laid out; when it grows, the diff scrolls it into view.
    height: Rc<Cell<Pixels>>,
}

#[derive(Default)]
pub(crate) struct Review {
    /// Top row and cursor of the keyboard view; `None` once the mouse scrolled.
    view: Option<(Spot, Spot)>,
    /// Default-context hunks for GitHub inline eligibility.
    pub(crate) hunks: HashMap<SharedString, Vec<Hunk>>,
    /// The diff pane as last painted.
    viewport: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) store: CommentStore,
    pub(crate) composer: Option<Composer>,
    pub(crate) sheet: Option<review_send::Sheet>,
    pub(crate) share: Option<review_share::Share>,
    /// The list below the diff, so `]c` brings the orphan it lands on into view.
    orphan_scroll: ScrollHandle,
    /// The thread last jumped to, and the lines selected for it: while they stay selected it
    /// is the thread there, though another one starts on the same line. `None` lines for a
    /// thread without a row (under its file header, or its file left the diff).
    jumped: Option<(u64, Option<DiffLineRange>)>,
}

impl Review {
    /// `}`/`{` and the tree moved the diff to a file header.
    pub(crate) fn at_file(&mut self, path: &str) {
        let s = Spot::File(path.to_string().into());
        self.view = Some((s.clone(), s));
        self.jumped = None; // the cursor left the jumped thread
    }
}

/// How many of `cs` have a status `f` accepts.
fn count(cs: &[&Comment], f: fn(&Status) -> bool) -> usize {
    cs.iter().filter(|c| f(&c.status)).count()
}

/// A small status chip: a colored dot and a label.
fn chip(label: String, color: Hsla, theme: &Theme) -> Div {
    h_flex()
        .h(px(22.))
        .px_2()
        .gap_1p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(label)
}

/// A composer box holding `text`, focused with the cursor at its end.
fn textarea(
    window: &mut Window,
    text: String,
    placeholder: &'static str,
    cx: &mut Context<Differ>,
) -> Entity<TextareaState> {
    let t = cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(1, 12)
            .placeholder(placeholder)
            .default_value(text)
    });
    t.update(cx, |i, cx| {
        let end = i.value().len();
        i.set_selected_range(end..end, cx);
        i.focus(window, cx)
    });
    t
}

impl Differ {
    fn diff_rows(&self, cx: &App) -> Vec<Row> {
        rows(self.diff.read(cx))
    }

    /// Scrolls `top` to the top of the diff and shows the cursor as the selected line.
    fn apply_view(&mut self, rows: &[Row], top: usize, cur: usize, cx: &mut Context<Self>) {
        let (Some(t), Some(c)) = (rows.get(top), rows.get(cur)) else {
            return;
        };
        let (t, c) = (t.spot.clone(), c.spot.clone());
        if let Some(ix) = self.diff_files.iter().position(|f| f.path() == c.path()) {
            self.diff_file = ix; // `v`/`o` act on the cursor's file
            self.follow_tree(ix);
        }
        self.diff.update(cx, |d, cx| {
            match &t {
                Spot::File(p) => d.scroll_to_file(p, cx),
                Spot::Line(l) => d.scroll_to_line(l.clone(), cx),
            }
            let sel = match &c {
                Spot::Line(l) => Some(DiffLineRange::new(
                    l.path().clone(),
                    l.side(),
                    l.line(),
                    l.line(),
                )),
                Spot::File(_) => None,
            };
            d.set_selected_lines(sel, cx);
        });
        // A refresh restoring the same spot keeps the jumped thread; a move drops it.
        if self.review.view.as_ref().is_none_or(|v| v.1 != c) {
            self.review.jumped = None;
        }
        self.review.view = Some((t, c));
        cx.notify();
    }

    /// A code row's height.
    fn row_height(&self, cx: &App) -> Pixels {
        crate::editor::diff_font(cx).1
    }

    fn view_cap(&self, cx: &App) -> f32 {
        let h = self.review.viewport.get().size.height;
        (h / self.row_height(cx)).max(SCROLLOFF * 2. + 1.)
    }

    /// `j/k` (`n` = ±1), `ctrl-d/u` (± half a screen), `gg`/`G` (`to` = first/last row).
    pub(crate) fn diff_move(
        &mut self,
        n: isize,
        to: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cap = self.view_cap(cx);
        let n = match n.abs() {
            1 => n,
            _ => n.signum() * (cap / 2.) as isize,
        };
        let Some((top, cur)) = self.review.view.clone().or_else(|| {
            let first = Spot::File(self.diff_files.first()?.path().clone());
            to.map(|_| (first.clone(), first))
        }) else {
            // The mouse moved the diff: we don't know where it is, so scroll natively.
            let name = match (n > 0, n.abs() == 1) {
                (true, true) => "shark_diff::ScrollDown",
                (false, true) => "shark_diff::ScrollUp",
                (true, false) => "shark_diff::PageDown",
                (false, false) => "shark_diff::PageUp",
            };
            if let Ok(a) = cx.build_action(name, None) {
                window.dispatch_action(a, cx);
            }
            return;
        };
        let rows = self.diff_rows(cx);
        if rows.is_empty() {
            return;
        }
        let (t, c) = (find(&rows, &top), find(&rows, &cur));
        let c = match to {
            Some(true) => 0,
            Some(false) => rows.len() - 1,
            None => c.saturating_add_signed(n).min(rows.len() - 1),
        };
        let t = fit(&rows, t, c, cap);
        self.drop_mouse_selection(window, cx);
        self.apply_view(&rows, t, c, cx);
    }

    /// `]c` / `[c`: the cursor goes to the next change; natively without a keyboard view.
    pub(crate) fn diff_change(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((top, cur)) = self.review.view.clone() else {
            return self.diff.update(cx, |d, cx| match forward {
                true => d.next_change(cx),
                false => d.previous_change(cx),
            });
        };
        let rows = self.diff_rows(cx);
        let Some(c) = next_change(&rows, find(&rows, &cur), forward) else {
            return;
        };
        let t = fit(&rows, find(&rows, &top), c, self.view_cap(cx));
        self.drop_mouse_selection(window, cx);
        self.apply_view(&rows, t, c, cx);
    }

    /// After `set_files` reset the diff, put the keyboard view back; false when there is none.
    pub(crate) fn restore_view(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((top, cur)) = self.review.view.clone() else {
            return false;
        };
        self.diff.update(cx, |d, cx| {
            for spot in [&top, &cur] {
                if let Spot::Line(line) = spot {
                    d.scroll_to_line(line.clone(), cx);
                }
            }
        });
        let rows = self.diff_rows(cx);
        let (t, c) = (find(&rows, &top), find(&rows, &cur));
        self.apply_view(&rows, t, c, cx);
        true
    }

    /// The mouse scrolled: the view model no longer matches the screen.
    pub(crate) fn lose_view(&mut self) {
        self.review.jumped = None;
        self.review.view = None;
    }

    /// Tracks the pane's height for paging; goes in the diff pane.
    pub(crate) fn viewport_probe(&self) -> impl IntoElement {
        let (vp, width) = (self.review.viewport.clone(), self.media_width.clone());
        canvas(
            move |b, _, _| {
                vp.set(b);
                width.set(b.size.width);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    pub(crate) fn ws_comments(&self) -> &[Comment] {
        self.task_name()
            .and_then(|ws| self.review.store.comments.get(ws))
            .map_or(&[], |c| &c[..])
    }

    /// The open task's comment `id`.
    pub(crate) fn comment(&self, id: u64) -> Option<&Comment> {
        self.ws_comments().iter().find(|c| c.id == id)
    }

    /// Edits the open task's comments and saves them.
    pub(crate) fn edit_comments(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Vec<Comment>),
    ) {
        if let Some(ws) = self.task_name().map(str::to_string) {
            self.review.store.edit(&ws, f);
            cx.notify();
        }
    }

    /// Threads (not resolved) and the composer as diff annotations under their last line, and
    /// the before/after row of each image or video under its header (not while folded). File
    /// headers get `review_chips` through `card_header_suffix`.
    pub(crate) fn with_comments(&self, diff: Diff, cx: &mut Context<Self>) -> Diff {
        let editing = self.review.composer.as_ref().map(|c| c.draft.id);
        let state = self.diff.read(cx);
        // Indexed as in `self.media`, which `render_annotation` reads.
        let media: Vec<_> = (self.media.iter().enumerate())
            .filter(|(_, (path, _))| !state.is_file_collapsed(path))
            .map(|(i, (path, _))| (i, path.clone()))
            .collect();
        // On their line while the diff has it; else under their file's header (the rest show
        // below the diff, `render_orphans`).
        let threads = (self.ws_comments().iter())
            .filter(|c| c.shown() && Some(c.id) != editing)
            .filter_map(|c| {
                let id = ("thread", c.id as usize);
                match self.on_its_line(c) {
                    true => Some(DiffAnnotation::line(id, c.anchor())),
                    false => (self.diff_files.iter().any(|f| *f.path() == c.path))
                        .then(|| DiffAnnotation::file(id, c.path.clone())),
                }
            });
        let composer = (self.review.composer.as_ref())
            .map(|c| DiffAnnotation::line(("composer", 0usize), c.draft.anchor()));
        let shark = cx.entity().downgrade();
        let add = shark.clone();
        // The diff's + beside a line or a selection: the composer there.
        diff.on_add_annotation(move |range, window, cx| {
            add.update(cx, |s, cx| {
                s.diff
                    .update(cx, |d, cx| d.set_selected_lines(Some(range.clone()), cx));
                s.comment_at(range.clone(), window, cx)
            })
            .ok();
        })
        .annotation_hints("Comment on working copy →", "← Comment on original")
        .annotations(threads.chain(composer).chain(
            (media.iter()).map(|(i, path)| DiffAnnotation::file(("media", *i), path.clone())),
        ))
        .render_annotation(move |a, _, cx| {
            let (name, i) = match a.id() {
                ElementId::NamedInteger(name, i) => (name.to_string(), *i as usize),
                _ => (String::new(), 0),
            };
            shark
                .update(cx, |s, cx| match name.as_str() {
                    "media" => (s.media.get(i).map(|(_, v)| v.clone().into_any_element()))
                        .unwrap_or_else(|| div().into_any_element()),
                    "composer" => s.render_composer(cx),
                    _ => s.render_thread(i as u64, cx),
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
    }

    /// Whether the current diff contains the thread's line, including folded context.
    pub(crate) fn on_its_line(&self, c: &Comment) -> bool {
        let now = (c.path.split_once('/')).and_then(|(repo, _)| self.diff_revs.get(repo));
        if c.end.0 && now.is_some_and(|r| !r.is_empty() && !c.rev.is_empty() && *r != c.rev) {
            return false;
        }
        self.diff_files
            .iter()
            .any(|file| *file.path() == c.path && file.line_index(side(c.end.0), c.end.1).is_some())
    }

    /// Whether one hunk of the diff shows all the thread's lines, as GitHub wants an inline
    /// comment's.
    pub(crate) fn in_one_hunk(&self, c: &Comment) -> bool {
        let h = self.hunk_of(c, c.end.1);
        self.on_its_line(c) && h.is_some() && self.hunk_of(c, c.start.1) == h
    }

    /// The default-context hunk containing the line, for GitHub inline eligibility.
    fn hunk_of(&self, c: &Comment, line: usize) -> Option<usize> {
        (self.review.hunks.get(c.path.as_str()))?
            .iter()
            .position(|h| {
                h.iter().any(|&(k, old, new)| match c.end.0 {
                    true => k != b'+' && old == line,
                    false => k != b'-' && new == line,
                })
            })
    }

    /// Moves the open task's working-copy threads to where their lines are on disk now, so
    /// lines added above one don't leave it beside other code.
    pub(crate) fn reanchor_comments(&mut self) {
        let Some((ws, dir)) = self.active_task().map(|w| (w.name.clone(), w.dir())) else {
            return;
        };
        // ponytail: reads each thread's file on every diff refresh; group by path if many.
        self.review.store.edit(&ws, |l| {
            for c in l.iter_mut().filter(|c| c.shown() && !c.end.0) {
                lines_now(&dir, c);
            }
        });
    }

    /// Threads whose file left the diff, below it, so none goes missing. Pending ones get
    /// Edit and Delete there, as no line cursor reaches them; edited, the composer shows in
    /// the card's place.
    pub(crate) fn render_orphans(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // No diff (yet, or git failed): every thread would look orphaned.
        if !self.diff_loaded || self.diff_error.is_some() {
            return None;
        }
        let ids: Vec<u64> = (self.ws_comments().iter())
            .filter(|c| c.shown() && !self.diff_files.iter().any(|f| *f.path() == c.path))
            .map(|c| c.id)
            .collect();
        if ids.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let removable: Vec<u64> = (ids.iter().copied())
            .filter(|&id| self.comment(id).is_some_and(|c| c.status != Status::Queued))
            .collect();
        let title = h_flex()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child("Threads on files no longer in the diff · ]c reaches them")
            .child(div().flex_1())
            .when(!removable.is_empty(), |d| {
                d.child(
                    Button::new("orphans-remove")
                        .ghost()
                        .xsmall()
                        .label(format!("Remove all {}", removable.len()))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            // A composer on one of them would add it back (`keep_draft`).
                            if (this.review.composer.as_ref())
                                .is_some_and(|c| removable.contains(&c.draft.id))
                            {
                                this.cancel_composer(w, cx);
                            }
                            this.edit_comments(cx, |l| l.retain(|c| !removable.contains(&c.id)))
                        })),
                )
            });
        let cards: Vec<AnyElement> = (ids.iter())
            .map(|&id| {
                let c = self.comment(id).map(|c| c.path.clone()).unwrap_or_default();
                let editing = (self.review.composer.as_ref()).is_some_and(|c| c.draft.id == id);
                let button = |name: &'static str, label: &'static str| {
                    Button::new((name, id as usize))
                        .ghost()
                        .xsmall()
                        .label(label)
                };
                v_flex()
                    .p_3()
                    .gap_1()
                    .child(div().text_xs().child(c))
                    .child(match editing {
                        true => self.render_composer(cx),
                        false => self.render_thread(id, cx),
                    })
                    // Queued ones say why they stay (`unqueued`).
                    .when(!editing, |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(button("orphan-edit", "Edit").on_click(
                                    cx.listener(move |this, _, w, cx| this.edit_thread(id, w, cx)),
                                ))
                                .child(button("orphan-delete", "Delete").on_click(cx.listener(
                                    move |this, _, w, cx| this.delete_thread(id, w, cx),
                                ))),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        Some(
            v_flex()
                .id("orphan-threads")
                .track_scroll(&self.review.orphan_scroll)
                .flex_none()
                .max_h(px(240.))
                .overflow_y_scroll()
                .px_4()
                .pt_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(title)
                .children(cards)
                .into_any_element(),
        )
    }

    /// The comment quoting its lines as they are now (`snapshot`), at the revision they are
    /// read at (`rev`).
    fn anchor_text(&self, c: &Comment) -> Comment {
        let Some(ws) = self.active_task() else {
            return c.clone();
        };
        let rev = match c.end.0 {
            false => "working tree".to_string(),
            // The base the diff compares against, as `DiffData::load` found it.
            true => (c.path.split_once('/'))
                .and_then(|(repo, _)| self.diff_revs.get(repo).cloned())
                .unwrap_or_default(),
        };
        // A file being edited in place shows its buffer, not what is on disk.
        let edited = (self.editing.as_ref())
            .filter(|e| !c.end.0 && c.start.0 == c.end.0 && *e.path == c.path)
            .map(|e| slice(&e.buf.text(), c.start.1, c.end.1));
        let mut c = Comment { rev, ..c.clone() };
        // ponytail: lines_now's git show blocks the UI thread as the composer opens; move it
        // to the background executor if big repos lag.
        let now = edited.or_else(|| lines_now(&ws.dir(), &mut c));
        Comment {
            snapshot: now.unwrap_or_default(),
            ..c
        }
    }

    /// `c`: the composer on the selected lines (the cursor line). A pending thread there is
    /// edited instead.
    pub(crate) fn comment_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.task.is_none() {
            return notify_err(window, cx, "Open a task first");
        }
        let Some(range) = self.picked_lines(cx) else {
            let keys = if self.preferences.vim {
                "j/k"
            } else {
                "↑/↓"
            };
            return notify_err(
                window,
                cx,
                format!("Select code or lines first, or move to a line ({keys})"),
            );
        };
        self.comment_at(range, window, cx)
    }

    /// `c` is on working-copy lines with unsaved edits in place: not what the agent reads.
    pub(crate) fn unsaved_edit(&self, c: &Comment) -> bool {
        (!c.start.0 || !c.end.0)
            && (self.editing.as_ref()).is_some_and(|e| e.dirty && *e.path == c.path)
    }

    /// Code selected with the mouse, else the line cursor or selected lines.
    fn picked_lines(&self, cx: &App) -> Option<DiffLineRange> {
        let d = self.diff.read(cx);
        d.text_selection_lines(cx).or_else(|| d.selected_lines())
    }

    /// A key moved the line cursor: a mouse selection left behind is no longer what `c` means.
    fn drop_mouse_selection(&self, window: &mut Window, cx: &mut App) {
        if self.diff.read(cx).text_selection_lines(cx).is_some() {
            gpui_kit::base::TextSelection::clear(window, cx);
        }
    }

    /// The composer on `range`, whatever is selected (the header's Comment, the diff's +).
    fn comment_at(&mut self, range: DiffLineRange, window: &mut Window, cx: &mut Context<Self>) {
        let draft = Comment::from_range(&range);
        if self.unsaved_edit(&draft) {
            return notify_err(
                window,
                cx,
                "Save first (⌘S): the agent reads the file on disk",
            );
        }
        self.keep_draft(window, cx);
        let same = (self.ws_comments().iter())
            .find(|c| {
                c.status == Status::Pending
                    && (&c.path, c.start, c.end) == (&draft.path, draft.start, draft.end)
            })
            .cloned();
        let c = same.unwrap_or_else(|| self.anchor_text(&draft));
        self.open_composer(c, window, cx)
    }

    /// A composer open elsewhere with text in it is added (or saved), not lost, before
    /// another opens.
    pub(crate) fn keep_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = |c: &Composer| !c.input.read(cx).value().trim().is_empty();
        if self.review.composer.as_ref().is_some_and(typed) {
            self.submit_composer(window, cx);
        }
    }

    /// `e`, an orphan card's Edit: the composer on thread `id`, keeping an open draft.
    fn edit_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_draft(window, cx);
        if let Some(c) = self.comment(id).cloned() {
            self.open_composer(c, window, cx);
        }
    }

    /// Opens the composer on `c`: an existing comment (id set) or a new draft, scrolled into
    /// view above the review bar.
    fn open_composer(&mut self, c: Comment, window: &mut Window, cx: &mut Context<Self>) {
        // The text box last: it takes the focus.
        let suggest =
            (c.suggestion.clone()).map(|s| textarea(window, s, "The code you propose", cx));
        let input = textarea(
            window,
            c.text.clone(),
            "What should the agent change here?",
            cx,
        );
        let anchor = Spot::Line(c.anchor());
        self.review.composer = Some(Composer {
            draft: c,
            input,
            suggest,
            height: Rc::default(),
        });
        // Room for most of it, so it is laid out; then `render_composer` measures it. An
        // orphan's shows below the diff.
        if self.diff_files.iter().any(|f| f.path() == anchor.path()) {
            self.reveal(&anchor, 8., cx);
        }
        cx.notify();
    }

    /// The diff scrolls `by` down (in the diff's own steps, so a little more), the composer
    /// with it. Threads and headers aren't the row model's height, so this goes by pixels.
    /// The diff scrolls only while it has the focus: it borrows it from the composer.
    fn scroll_diff(&mut self, by: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(back), Ok(down)) = (
            window.focused(cx),
            cx.build_action("shark_diff::ScrollDown", None),
        ) else {
            return;
        };
        let diff = self.diff.read(cx).focus_handle(cx);
        diff.focus(window, cx);
        for _ in 0..(by / (window.rem_size() * 1.5)).ceil() as usize {
            diff.dispatch_action(&*down, window, cx);
        }
        back.focus(window, cx);
    }

    /// cmd-shift-s: the Suggest change box, prefilled with the lines; again closes it.
    pub(crate) fn toggle_suggest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comp) = &mut self.review.composer else {
            return;
        };
        match comp.suggest.take() {
            Some(_) => comp.input.update(cx, |i, cx| i.focus(window, cx)),
            None => {
                let text = comp.draft.snapshot.clone();
                comp.suggest = Some(textarea(window, text, "The code you propose", cx));
            }
        }
        cx.notify();
    }

    /// cmd-shift-enter: the draft straight to the shown chat, skipping the sheet (the other
    /// pending threads stay for a later send).
    pub(crate) fn send_composer_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.submit_composer(window, cx) else {
            return;
        };
        // An edited thread with the same words is still where it was.
        match self.comment(id).map(|c| c.status == Status::Pending) {
            Some(true) => self.send_review(&[id], "", false, window, cx),
            _ => notify_err(window, cx, "Nothing new to send: the agent has this thread"),
        }
    }

    /// cmd-enter: adds the draft to the review (Pending), or saves the edited thread. Empty
    /// text removes it. The thread's id, when one is left.
    pub(crate) fn submit_composer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let comp = self.review.composer.take()?;
        let text = comp.input.read(cx).value().trim().to_string();
        let suggestion = (comp.suggest.as_ref())
            .map(|s| s.read(cx).value().trim_end().to_string())
            .filter(|s| !s.trim().is_empty());
        let order: HashMap<String, usize> = (self.diff_files.iter().enumerate())
            .map(|(i, f)| (f.path().to_string(), i))
            .collect();
        let ws = self.task_name()?.to_string();
        let id = match comp.draft.id {
            0 => self.review.store.new_id(&ws),
            id => id,
        };
        let kept = !text.is_empty();
        self.edit_comments(cx, |list| {
            let old = list.iter().position(|c| c.id == id).map(|i| list.remove(i));
            if !text.is_empty() {
                let mut old = old.unwrap_or(comp.draft);
                // New words the agent hasn't seen: pending again, to send.
                if old.text != text || old.suggestion != suggestion {
                    (old.status, old.chat) = (Status::Pending, None);
                }
                list.push(Comment {
                    id,
                    text,
                    suggestion,
                    ..old
                });
                // File order, then line, so the prompt reads top to bottom.
                list.sort_by_key(|c| (order.get(&c.path).copied(), c.start.1));
            }
        });
        self.focus_diff(window, cx);
        kept.then_some(id)
    }

    /// Esc: drops the draft (an edited thread stays as it was).
    pub(crate) fn cancel_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.review.composer = None;
        self.focus_diff(window, cx);
        cx.notify();
    }

    pub(crate) fn focus_diff(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// The shown thread under the line cursor (or the selection's last line).
    pub(crate) fn thread_at(&self, cx: &App) -> Option<u64> {
        let r = self.diff.read(cx).selected_lines();
        let jumped = (self.review.jumped.as_ref()).filter(|(_, sel)| *sel == r);
        let Some(r) = r else {
            // `]c` landed on a thread without a row: it is the one, until the cursor moves.
            return jumped
                .map(|(id, _)| *id)
                .filter(|&id| self.comment(id).is_some_and(Comment::shown));
        };
        let at = DiffLinePosition::new(r.path().clone(), r.end_side(), r.end());
        let here: Vec<u64> = (self.ws_comments().iter())
            .filter(|c| c.shown() && c.covers(&at))
            .map(|c| c.id)
            .collect();
        (jumped.map(|(id, _)| *id).filter(|id| here.contains(id))).or(here.first().copied())
    }

    /// Thread `id`, unless it waits in a chat's queued review card (that card would send it
    /// as it was, or not at all).
    fn unqueued(&self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> Option<Comment> {
        let c = self.comment(id)?;
        if c.status == Status::Queued {
            notify_err(
                window,
                cx,
                "Queued in a chat · remove the review from its queue first",
            );
            return None;
        }
        Some(c.clone())
    }

    /// Runs a thread action (`e`, `x`, `d`) on the thread at the cursor.
    pub(crate) fn at_cursor(
        &mut self,
        f: fn(&mut Self, u64, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.thread_at(cx) {
            f(self, id, window, cx);
        }
    }

    pub(crate) fn edit_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.unqueued(id, window, cx).is_some() {
            self.edit_comment(id, window, cx);
        }
    }

    pub(crate) fn delete_thread(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.unqueued(id, window, cx).is_some() {
            self.edit_comments(cx, |l| l.retain(|c| c.id != id));
        }
    }

    pub(crate) fn set_status(&mut self, ids: &[u64], status: Status, cx: &mut Context<Self>) {
        self.edit_comments(cx, |l| {
            for c in l.iter_mut().filter(|c| ids.contains(&c.id)) {
                c.status = status.clone();
            }
        });
    }

    /// `r`: resolve the sent thread at the cursor (a pending one has no way back from hidden);
    /// with none, `r` refreshes the diff as before.
    pub(crate) fn resolve_thread(&mut self, cx: &mut Context<Self>) {
        match (self.thread_at(cx).and_then(|id| self.comment(id))).filter(|c| c.status.resolvable())
        {
            Some(c) => self.set_status(&[c.id], Status::Resolved, cx),
            None => cx.propagate(),
        }
    }

    /// `R`: back to pending, to send again.
    pub(crate) fn reopen_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = (self.thread_at(cx)).filter(|&id| self.unqueued(id, window, cx).is_some())
        {
            self.set_status(&[id], Status::Pending, cx);
        }
    }

    fn with_status(&self, f: impl Fn(&Status) -> bool) -> Vec<u64> {
        (self.ws_comments().iter())
            .filter(|c| f(&c.status))
            .map(|c| c.id)
            .collect()
    }

    /// Outdated threads pending again, quoting their lines as they are now.
    pub(crate) fn requeue_outdated(&mut self, cx: &mut Context<Self>) {
        let mut now: HashMap<u64, Comment> = (self.ws_comments().iter())
            .filter(|c| c.status == Status::Outdated)
            .map(|c| (c.id, self.anchor_text(c)))
            .collect();
        self.edit_comments(cx, |l| {
            for c in l.iter_mut() {
                if let Some(n) = now.remove(&c.id) {
                    *c = Comment {
                        status: Status::Pending,
                        ..n
                    };
                }
            }
        });
    }

    pub(crate) fn resolve_addressed(&mut self, cx: &mut Context<Self>) {
        let ids = self.with_status(|s| matches!(s, Status::Addressed { .. }));
        self.set_status(&ids, Status::Resolved, cx);
    }

    /// The review bar's Discard: every pending comment, once confirmed.
    pub(crate) fn discard_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.with_status(|s| *s == Status::Pending).len();
        confirm(
            format!("Discard {n} pending comment{}?", crate::ui::plural(n)),
            "They have not been sent to a chat or GitHub.",
            "Discard",
            |s, _, cx| s.edit_comments(cx, |l| l.retain(|c| c.status != Status::Pending)),
            window,
            cx,
        );
    }

    /// The diff file thread `id` is on; none once its file left the diff.
    fn change_ix(&self, id: u64) -> Option<usize> {
        let path = &self.comment(id)?.path;
        self.diff_files.iter().position(|f| f.path() == path)
    }

    /// The thread's file in the editor, to see what the agent made of it.
    pub(crate) fn view_change(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.change_ix(id) {
            self.open_file(ix, window, cx);
        }
    }

    /// A thread card's right-click menu. Shortcuts stay in the card's key hints.
    pub(crate) fn thread_menu(&self, id: u64, cx: &Context<Self>) -> Vec<Entry> {
        let Some(c) = self.comment(id).cloned() else {
            return vec![];
        };
        let act = |f: fn(&mut Self, u64, &mut Window, &mut Context<Self>)| {
            on_differ(self.task_name(), cx, move |s, w, cx| f(s, id, w, cx))
        };
        let addressed = matches!(c.status, Status::Addressed { .. } | Status::Outdated);
        let mut entries = vec![menu::item(IconName::Pencil, "Edit", act(Self::edit_thread))];
        if c.status.resolvable() {
            entries.push(menu::item(
                IconName::CircleCheck,
                "Resolve",
                act(|s, id, _, cx| s.set_status(&[id], Status::Resolved, cx)),
            ));
        }
        if addressed || c.status == Status::Resolved {
            entries.push(menu::item(
                IconName::CircleDashed,
                "Reopen",
                act(|s, id, _, cx| s.set_status(&[id], Status::Pending, cx)),
            ));
        }
        if addressed && self.change_ix(id).is_some() {
            let view = act(Self::view_change);
            entries.push(menu::item(IconName::FileDiff, "View change", view));
        }
        if c.status.in_context() {
            let show = act(Self::show_thread_chat);
            entries.push(menu::item(IconName::MessageSquare, "Show chat", show));
        }
        if c.status != Status::Pending {
            entries.push(match c.github {
                Some(url) => menu::item(IconName::Github, "View on GitHub", move |_, cx| {
                    cx.open_url(&url)
                }),
                None => menu::item(IconName::Github, "Share to GitHub…", act(Self::open_share)),
            });
        }
        let text = c.text;
        entries.extend([
            menu::item(IconName::Copy, "Copy comment", move |w, cx| {
                copy(text.clone(), w, cx)
            }),
            Entry::Sep,
            menu::item(IconName::Trash, "Delete", act(Self::delete_thread)),
        ]);
        entries
    }

    /// Scrolls row `spot` into view with `below` code rows of room under it (for what is
    /// annotated there), keeping the selected lines.
    fn reveal(&mut self, spot: &Spot, below: f32, cx: &mut Context<Self>) {
        let sel = self.diff.read(cx).selected_lines();
        if let Spot::Line(line) = spot {
            self.diff
                .update(cx, |d, cx| d.scroll_to_line(line.clone(), cx));
        }
        let rows = self.diff_rows(cx);
        let cur = find(&rows, spot);
        let top = (self.review.view.as_ref()).map_or(cur, |(t, _)| find(&rows, t));
        let cap = (self.view_cap(cx) - below).max(SCROLLOFF * 2. + 1.);
        self.apply_view(&rows, fit(&rows, top, cur, cap), cur, cx);
        self.diff.update(cx, |d, cx| d.set_selected_lines(sel, cx));
    }

    /// Puts the line cursor on comment `id`. A thread without a row (under its file header,
    /// or its file left the diff and it shows below) is still the one `e`/`x`/`r` act on,
    /// with no line selected, until the cursor moves.
    pub(crate) fn jump_to_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.comment(id).cloned() else {
            return;
        };
        self.drop_mouse_selection(window, cx);
        let in_diff = self.change_ix(id).is_some();
        let on_line = in_diff && self.on_its_line(&c);
        if in_diff {
            // A viewed file is folded: its line has no row.
            self.diff.update(cx, |d, cx| {
                if d.is_file_collapsed(&c.path) {
                    d.set_file_collapsed(&c.path, false, cx)
                }
            });
            let spot = match on_line {
                true => Spot::Line(c.anchor()),
                false => Spot::File(c.path.clone().into()),
            };
            self.reveal(&spot, 0., cx);
        } else if let Some(ix) = (self.ws_comments().iter())
            .filter(|c| c.shown() && self.change_ix(c.id).is_none())
            .position(|c| c.id == id)
        {
            self.review.orphan_scroll.scroll_to_item(ix + 1); // after the title
        }
        // `find` lands on the closest row; the thread's own line is what `e`/`x` read.
        let sel =
            on_line.then(|| DiffLineRange::new(c.path.clone(), side(c.end.0), c.end.1, c.end.1));
        self.diff.update(cx, |d, cx| d.set_selected_lines(sel, cx));
        self.review.jumped = Some((id, self.diff.read(cx).selected_lines()));
        cx.notify();
    }

    /// `]c` / `[c`: the next (previous) thread, wrapping; with none, the next change.
    pub(crate) fn thread_jump(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Top to bottom as the diff shows them; one row's by id. A thread under its file
        // header sits at the header; orphans (their file left the diff) come last.
        let rows = self.diff_rows(cx);
        let row = |l: DiffLinePosition| find(&rows, &Spot::Line(l));
        let mut threads: Vec<(usize, u64)> = (self.ws_comments().iter())
            .filter(|c| c.shown())
            .map(|c| {
                let r = match (self.change_ix(c.id).is_some(), self.on_its_line(c)) {
                    (true, true) => row(c.anchor()),
                    (true, false) => find(&rows, &Spot::File(c.path.clone().into())),
                    (false, _) => usize::MAX,
                };
                (r, c.id)
            })
            .collect();
        if threads.is_empty() {
            return self.diff_change(forward, window, cx);
        }
        threads.sort();
        let here = self.thread_at(cx);
        let at = match self.diff.read(cx).selected_lines() {
            Some(r) => {
                let l = DiffLinePosition::new(r.path().clone(), r.end_side(), r.end());
                Some((row(l), here.unwrap_or(0)))
            }
            // On a jumped thread without a row: the next one after it.
            None => here.and_then(|id| threads.iter().find(|t| t.1 == id).copied()),
        };
        let next = match (forward, at) {
            (_, None) => threads.first(),
            (true, Some(at)) => threads.iter().find(|&&t| t > at).or(threads.first()),
            (false, Some(at)) => threads.iter().rev().find(|&&t| t < at).or(threads.last()),
        };
        if let Some(&(_, id)) = next {
            self.jump_to_comment(id, window, cx);
        }
    }

    /// "Show chat ↗": the chat the thread went to (else the open one).
    pub(crate) fn show_thread_chat(
        &mut self,
        id: u64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.comment(id).and_then(|c| c.chat.clone());
        if let Some(task) = self.task_name() {
            cx.emit(DifferEvent::OpenChat {
                task: task.into(),
                chat,
            });
        }
    }

    /// A chat turn ended: its reply's reports and the code decide what became of the comments
    /// it was sent. Read from disk, so it doesn't wait for the diff refresh.
    pub(crate) fn review_turn_done(
        &mut self,
        ws: &str,
        dir: &Path,
        id: &str,
        turn: usize,
        reply: &str,
        held: &[u64],
        cx: &mut Context<Self>,
    ) {
        let reports: HashMap<u64, String> = parse_reports(reply).into_iter().collect();
        self.review.store.edit(ws, |list| {
            for c in list.iter_mut() {
                let ours = c.chat.as_deref() == Some(id) && !held.contains(&c.id);
                if !ours || !matches!(c.status, Status::Sent { .. }) {
                    continue;
                }
                let now = (!c.end.0).then(|| lines_now(dir, c)).flatten();
                let report = reports.get(&c.id).map(|s| s.as_str());
                c.status = judge(&c.status, turn, report, now.as_deref(), &c.snapshot);
            }
        });
        cx.notify();
    }

    /// Chat `chat` sent comments `ids` (`Some(turn)`: read in that turn), or they never
    /// went out (`None`: removed from its queue, or never read before its process ended) and
    /// are pending again. Only comments still waiting in that chat change: an id may be gone.
    pub(crate) fn review_delivered(
        &mut self,
        ws: &str,
        chat: &str,
        ids: &[u64],
        turn: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.review.store.edit(ws, |l| {
            let ours = |c: &Comment| ids.contains(&c.id) && c.chat.as_deref() == Some(chat);
            for c in l.iter_mut().filter(|c| ours(c)) {
                match (turn, &c.status) {
                    (Some(turn), Status::Queued) => c.status = Status::Sent { turn },
                    (None, Status::Queued | Status::Sent { .. }) => {
                        (c.status, c.chat) = (Status::Pending, None)
                    }
                    _ => {}
                }
            }
        });
        cx.notify();
    }

    /// The composer (A·01): lines and side, Suggest change, the text, Esc / Add to review.
    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(comp) = &self.review.composer else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let editing = comp.draft.id != 0;
        let (seen, vp) = (comp.height.clone(), self.review.viewport.clone());
        let shark = cx.entity().downgrade();
        // Measured after layout: taller than last time (opened, grew) and past the pane's
        // bottom, the diff scrolls up to show it, but never its top out of view.
        let probe = canvas(
            move |b, window, cx| {
                if b.size.height <= seen.get() {
                    return;
                }
                seen.set(b.size.height);
                let pane = vp.get();
                let by = (b.bottom() - pane.bottom()).min(b.top() - pane.top());
                if by > px(0.) && b.top() < pane.bottom() {
                    window.defer(cx, move |w, cx| {
                        (shark.update(cx, |s, cx| s.scroll_diff(by, w, cx))).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        v_flex()
            .id("composer-card")
            .key_context("ReviewComposer")
            .min_w_0()
            .overflow_hidden()
            .mx(px(-12.))
            .my(px(-12.))
            .px_4()
            .py_3()
            .gap_2()
            .border_l_2()
            .border_color(theme.primary)
            .text_sm()
            // cmd-enter would add a line and Esc stops in the textarea; both are ours here.
            .capture_action(cx.listener(|this, a: &Enter, w, cx| {
                if a.secondary {
                    cx.stop_propagation();
                    this.submit_composer(w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SendNow, w, cx| this.send_composer_now(w, cx)))
            .capture_action(cx.listener(|this, _: &Escape, w, cx| {
                cx.stop_propagation();
                this.cancel_composer(w, cx);
            }))
            .on_action(cx.listener(|this, _: &SuggestChange, w, cx| this.toggle_suggest(w, cx)))
            .child(probe)
            // Narrow, the buttons wrap below rather than squeeze.
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(comp.draft.lines()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(comp.draft.side()),
                    )
                    .child(
                        Button::new("suggest-change")
                            .outline()
                            .xsmall()
                            .ml_auto()
                            .flex_shrink_1()
                            .min_w_0()
                            .label("Suggest change ⌘⇧S")
                            .on_click(cx.listener(|this, _, w, cx| this.toggle_suggest(w, cx))),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.primary)
                    .child(Textarea::new(&comp.input).appearance(false)),
            )
            .when_some(comp.suggest.as_ref(), |d, s| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .child(Textarea::new(s).appearance(false)),
                )
            })
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_grow(1.)
                            .min_w_0()
                            .whitespace_normal()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match editing {
                                true => "Empty text deletes the thread",
                                false => "⌘↵ keeps it for ⇧C's review · ⌘⇧↵ sends it now",
                            }),
                    )
                    .child(
                        Button::new("composer-esc")
                            .ghost()
                            .xsmall()
                            .flex_shrink_1()
                            .min_w_0()
                            .label("Cancel esc")
                            .on_click(cx.listener(|this, _, w, cx| this.cancel_composer(w, cx))),
                    )
                    .when(!editing, |d| {
                        d.child(
                            Button::new("composer-send")
                                .outline()
                                .xsmall()
                                .flex_shrink_1()
                                .min_w_0()
                                .label("Send now ⌘⇧↵")
                                .on_click(
                                    cx.listener(|this, _, w, cx| this.send_composer_now(w, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("composer-add")
                            .primary()
                            .xsmall()
                            .flex_shrink_1()
                            .min_w_0()
                            .label(if editing {
                                "Save ⌘↵"
                            } else {
                                "Add to review ⌘↵"
                            })
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.submit_composer(w, cx);
                            })),
                    ),
            )
            .test_support()
            .into_any_element()
    }

    /// A thread card (A·02, A·04, 05, 08).
    fn render_thread(&self, id: u64, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self.comment(id).cloned() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let (pill, color, hints) = match &c.status {
            Status::Pending => (
                "Pending".to_string(),
                theme.warning_foreground,
                "e edit · x delete",
            ),
            Status::Queued => ("Queued".into(), theme.primary, ""),
            Status::Sent { .. } => ("Sent".into(), theme.primary, "r resolve · e edit"),
            Status::Addressed { turn, .. } => (
                format!("Addressed · turn {turn}"),
                theme.success_foreground,
                "d view change · r resolve · R reopen",
            ),
            Status::Outdated => (
                "Outdated".into(),
                theme.muted_foreground,
                "d view change · r resolve · R reopen",
            ),
            Status::Resolved => ("Resolved".into(), theme.muted_foreground, ""),
        };
        let in_context = c.status.in_context();
        // Off its line (gone from the diff): the lines as they were, so it still reads.
        let snapshot =
            (!self.on_its_line(&c) && !c.snapshot.is_empty()).then(|| c.snapshot.clone());
        let posted = c.github.as_deref().map(posted_as);
        let where_ = match (in_context, posted.is_some()) {
            (true, true) => "In AI context · On GitHub",
            (true, false) => "In AI context",
            (false, true) => "On GitHub",
            (false, false) => "",
        };
        let mono = theme.mono_font_family.clone();
        // No line to select for it: the card itself shows `]c` landed here.
        let picked = !self.on_its_line(&c) && self.thread_at(cx) == Some(id);
        v_flex()
            .id(("thread-card", id as usize))
            .min_w_0()
            .overflow_hidden()
            .mx(px(-12.))
            .my(px(-12.))
            .px_4()
            .py_3()
            .gap_1p5()
            .border_l_2()
            .border_color(color)
            .when(picked, |d| d.bg(theme.list_active))
            .text_sm()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    // Stopped here, the diff never took the focus: the keys after the menu
                    // closes are its, not the last text box's.
                    cx.stop_propagation();
                    this.focus_diff(w, cx);
                    this.jump_to_comment(id, w, cx); // selects it like `]c`
                    let entries = this.thread_menu(id, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            // Narrow, the pill and where it went wrap to the next line rather than clip; the
            // keys take what room is left on the last line, the lines truncate last.
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_2()
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("You"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", c.lines(), c.side())),
                    )
                    .child(
                        div()
                            .id(("thread-pill", id as usize))
                            .min_w_0()
                            .truncate()
                            .px_1p5()
                            .rounded(theme.radius)
                            .text_xs()
                            .text_color(color)
                            .bg(color.opacity(0.12))
                            .child(pill)
                            .test_support(),
                    )
                    .child(
                        div()
                            .id(("thread-where", id as usize))
                            .min_w_0()
                            .whitespace_normal()
                            .text_xs()
                            .text_color(theme.success_foreground)
                            .child(where_)
                            .test_support(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(faint(theme))
                            .child(hints),
                    ),
            )
            .when_some(snapshot, |d, s| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .font_family(mono.clone())
                        .text_size(theme.mono_font_size)
                        .text_color(theme.muted_foreground)
                        .child(s),
                )
            })
            .child(
                div()
                    .text_color(theme.foreground)
                    .whitespace_normal()
                    .child(c.text.clone()),
            )
            .when_some(c.suggestion.clone(), |d, s| {
                d.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Suggested change"),
                        )
                        .child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.muted)
                                .font_family(mono.clone())
                                .text_size(theme.mono_font_size)
                                .child(s),
                        ),
                )
            })
            .when_some(
                match &c.status {
                    Status::Addressed { note, .. } => Some(note.clone()),
                    _ => None,
                },
                |d, note| {
                    d.child(
                        // One paragraph, so a narrow column wraps it like the body.
                        div()
                            .pt_1p5()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_xs()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(StyledText::new(format!("Agent  {note}")).with_highlights([(
                                0..5,
                                HighlightStyle {
                                    font_weight: Some(FontWeight::SEMIBOLD),
                                    color: Some(theme.foreground),
                                    ..Default::default()
                                },
                            )])),
                    )
                },
            )
            .when_some(posted, |d, p| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("Posted as {p}. AI context is unchanged.")),
                )
            })
            .when(c.status != Status::Pending, |d| {
                // A narrow column wraps the buttons rather than clipping them.
                d.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .justify_end()
                        .when(in_context, |d| {
                            d.child(
                                Button::new(("show-chat", id as usize))
                                    .ghost()
                                    .xsmall()
                                    .flex_shrink_1()
                                    .min_w_0()
                                    .label("Show chat ↗")
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.show_thread_chat(id, w, cx)
                                    })),
                            )
                        })
                        .child(match c.github.clone() {
                            Some(url) => Button::new(("view-gh", id as usize))
                                .outline()
                                .xsmall()
                                .flex_shrink_1()
                                .min_w_0()
                                .label("View on GitHub ↗")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                            None => Button::new(("share-gh", id as usize))
                                .outline()
                                .xsmall()
                                .flex_shrink_1()
                                .min_w_0()
                                .label("Share to GitHub…")
                                .on_click(
                                    cx.listener(move |this, _, w, cx| this.open_share(id, w, cx)),
                                ),
                        }),
                )
            })
            .test_support()
            .into_any_element()
    }

    /// A file header's review chip and Comment button: "● N pending", "Addressed", "Outdated";
    /// "Commenting" while the composer is in the file, else "Comment" / "N comments".
    pub(crate) fn review_chips(&self, path: &SharedString, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let threads: Vec<&Comment> = (self.ws_comments().iter())
            .filter(|c| c.shown() && c.path == path.as_str())
            .collect();
        let pending = count(&threads, |s| *s == Status::Pending);
        let chip = match () {
            _ if pending > 0 => Some(chip(
                format!("{pending} pending"),
                theme.warning_foreground,
                theme,
            )),
            _ if count(&threads, |s| *s == Status::Outdated) > 0 => {
                Some(chip("Outdated".into(), theme.warning_foreground, theme))
            }
            _ if count(&threads, |s| matches!(s, Status::Addressed { .. })) > 0 => {
                Some(chip("Addressed".into(), theme.success_foreground, theme))
            }
            _ if count(&threads, |s| {
                matches!(s, Status::Queued | Status::Sent { .. })
            }) > 0 =>
            {
                Some(chip("Sent".into(), theme.primary, theme))
            }
            _ => None,
        };
        let commenting =
            (self.review.composer.as_ref()).is_some_and(|c| c.draft.path == path.as_str());
        let label = match (commenting, threads.len()) {
            (true, _) => "Commenting".to_string(),
            (_, 0) => "Comment".into(),
            (_, 1) => "1 comment".into(),
            (_, n) => format!("{n} comments"),
        };
        let (path, button) = (path.clone(), commenting || chip.is_none());
        h_flex()
            .gap(px(10.))
            .children(chip)
            .when(button, |d| {
                d.child(
                    Button::new(SharedString::from(format!("comment:{path}")))
                        .outline()
                        .xsmall()
                        .h(px(28.))
                        .px(px(9.))
                        .rounded(px(6.))
                        .label(label)
                        .on_click(cx.listener(move |this, _, w, cx| this.comment_in(&path, w, cx))),
                )
            })
            .into_any_element()
    }

    /// The header's Comment: the selection when it is in `path`, else its first changed line.
    pub(crate) fn comment_in(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.picked_lines(cx).filter(|r| r.path() == path);
        let range = here.or_else(|| {
            let first = (self.review.hunks.get(path))
                .and_then(|h| h.iter().flatten().find(|l| l.0 != b' '))
                .map(|&(k, old, new)| match k {
                    b'-' => (DiffSide::Original, old),
                    _ => (DiffSide::Modified, new),
                })?;
            Some(DiffLineRange::new(
                path.to_string(),
                first.0,
                first.1,
                first.1,
            ))
        });
        let Some(range) = range else { return };
        self.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(range.clone()), cx));
        self.comment_at(range, window, cx);
    }

    /// The sticky review bar at the bottom of the diff (A·01/02/04). `status`, when set,
    /// replaces its left text (in-place editing says what it is doing there). Without
    /// threads it is the key hint line.
    pub(crate) fn render_review_bar(
        &self,
        status: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let shown: Vec<&Comment> = self.ws_comments().iter().filter(|c| c.shown()).collect();
        let pending = count(&shown, |s| *s == Status::Pending);
        let addressed = count(&shown, |s| matches!(s, Status::Addressed { .. }));
        let outdated = count(&shown, |s| *s == Status::Outdated);
        // 28 tall with just a hint, 40 with the review's buttons.
        let bar = h_flex()
            .flex_none()
            .px(px(16.))
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground);
        if let Some(s) = status {
            return bar.h(px(28.)).child(s).into_any_element();
        }
        // Without Vim mode the arrows move the cursor, not j / k.
        let jk = if self.preferences.vim {
            "j / k"
        } else {
            "↑ / ↓"
        };
        if shown.is_empty() {
            return bar
                .h(px(28.))
                .child(format!("s split / unified · c comment · Drag divider to resize · {jk} move through changes"))
                .into_any_element();
        }
        let (lead, color, summary) = match pending {
            0 => (
                "Review · sent".to_string(),
                theme.success_foreground,
                format!("{addressed} addressed · {outdated} outdated · 0 pending"),
            ),
            n => {
                // In file order already, so a file's threads sit together.
                let mut files: Vec<&str> = (shown.iter())
                    .filter(|c| c.status == Status::Pending)
                    .map(|c| c.path.rsplit('/').next().unwrap_or(&c.path))
                    .collect();
                files.dedup();
                let files = match files.len() {
                    1 | 2 => files.join(" · "),
                    n => format!("{n} files"),
                };
                (
                    format!("Review · {n} pending"),
                    theme.warning_foreground,
                    files,
                )
            }
        };
        // Narrow, the lead, the summary and the keys truncate; the actions stay whole.
        let text = |t: String| div().min_w_0().truncate().child(t);
        let button = |id: &'static str, label: String| Button::new(id).small().label(label);
        let bar = bar.h(px(40.));
        bar.child(
            h_flex()
                .min_w_0()
                .gap_1p5()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(div().flex_none().size(px(6.)).rounded_full().bg(color))
                .child(div().min_w_0().truncate().child(lead)),
        )
        .child(text(summary))
        .child(text(format!("[c / ]c jump · {jk} change · v viewed")).text_color(faint(theme)))
        .child(div().flex_1())
        .when(outdated > 0, |d| {
            d.child(
                button("review-requeue", "Re-queue outdated".into())
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.requeue_outdated(cx))),
            )
        })
        .when(addressed > 0, |d| {
            d.child(
                button("review-resolve", format!("Resolve {addressed} addressed"))
                    .on_click(cx.listener(|this, _, _, cx| this.resolve_addressed(cx))),
            )
        })
        .when(pending > 0, |d| {
            d.child(
                button("review-discard", "Discard…".into())
                    .ghost()
                    .on_click(cx.listener(|this, _, w, cx| this.discard_pending(w, cx))),
            )
            .child(
                button("review-send", "Send review… ⇧C".into())
                    .when(self.review.sheet.is_none(), |b| b.primary())
                    .on_click(cx.listener(|this, _, w, cx| this.open_sheet(w, cx))),
            )
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    const PATCH: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,3 @@ fn x\n a\n-b\n-c\n+B\n d\n@@ -10 +10,2 @@\n x\n+y\n\\ No newline at end of file\n";

    #[test]
    fn geometry_preserves_empty_sides_and_unterminated_lines() {
        let file = |body: &str| {
            DiffFile::parse(&format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{body}"))
                .unwrap()
                .remove(0)
        };
        assert_eq!(
            hunks(&file("@@ -0,0 +1,2 @@\n+a\n+b\n")),
            [vec![(b'+', 0, 1), (b'+', 0, 2)]]
        );
        assert_eq!(
            hunks(&file("@@ -1,2 +0,0 @@\n-a\n-b\n")),
            [vec![(b'-', 1, 0), (b'-', 2, 0)]]
        );
        assert_eq!(
            hunks(&file(
                "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n"
            )),
            [vec![(b'-', 1, 0), (b'+', 0, 1)]]
        );
        assert!(hunks(&file("@@ -0,0 +0,0 @@\n")).iter().all(Vec::is_empty));
        assert!(hunks(&DiffFile::unchanged("f", "parse failure\n")).is_empty());
    }

    #[test]
    fn expanded_geometry_is_narrowed_only_for_remote_inline_policy() {
        let patch = |expanded: bool| {
            let (first, last) = if expanded { (1, 20) } else { (7, 13) };
            let body: String = (first..=last)
                .map(|line| {
                    if line == 10 {
                        "-old\n+new\n".into()
                    } else {
                        format!(" line {line}\n")
                    }
                })
                .collect();
            let count = last - first + 1;
            format!(
                "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -{first},{count} +{first},{count} @@\n{body}"
            )
        };
        let expanded = hunks(&DiffFile::parse(&patch(true)).unwrap()[0]);
        let remote = hunks(&DiffFile::parse(&patch(false)).unwrap()[0]);
        assert_eq!(expanded[0].len(), 21);
        assert_eq!(narrow_hunks(expanded), remote);
        for bad in ["@@ -1,2 +1,2 @@\n-a\n+b\n", "@@ -1 +1 @@\n-a\n+b\n+c\n"] {
            let source = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{bad}");
            assert!(DiffFile::parse(&source).is_err());
        }
    }

    fn spots(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.spot {
                Spot::File(p) => format!("F {p}"),
                Spot::Line(l) => format!("{:?} {}", l.side(), l.line()),
            })
            .collect()
    }

    #[gpui_kit::test]
    fn expanded_context_keeps_local_comments_and_navigation(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            crate::diff_view::init(cx);
        });
        let (differ, cx) = cx.add_window_view(|window, cx| {
            Differ::new(
                Preferences::from(&crate::state::State::default()),
                window,
                cx,
            )
        });
        let body = (1..=60)
            .map(|n| {
                if n == 30 {
                    "-line 30\n+changed 30\n".to_string()
                } else {
                    format!(" line {n}\n")
                }
            })
            .collect::<String>();
        let patch = format!(
            "diff --git a/api/f b/api/f\n--- a/api/f\n+++ b/api/f\n@@ -1,60 +1,60 @@\n{body}"
        );
        differ.update_in(cx, |d, window, cx| {
            d.diff_files = DiffFile::parse(&patch).unwrap();
            d.review
                .hunks
                .insert("api/f".into(), narrow_hunks(hunks(&d.diff_files[0])));
            d.diff
                .update(cx, |state, cx| state.set_files(d.diff_files.clone(), cx));
            let line = |n| Spot::Line(DiffLinePosition::new("api/f", DiffSide::Modified, n));
            assert!(!d.diff_rows(cx).iter().any(|r| r.spot == line(10)));
            d.review.at_file("api/f");
            d.diff_move(1, None, window, cx);
            assert_eq!(d.diff.read(cx).selected_lines().unwrap().start(), 27);

            // Opening a context comment reveals its line before finding the keyboard row.
            d.reveal(&line(10), 0., cx);
            let visible = d.diff_rows(cx);
            assert!(visible.iter().any(|r| r.spot == line(10)));
            assert!(!visible.iter().any(|r| r.spot == line(50)));
            d.diff_move(1, None, window, cx);
            assert_eq!(d.diff.read(cx).selected_lines().unwrap().start(), 11);

            // Adding or removing another file resets folds but must preserve both saved lines.
            for add in [true, false] {
                if add {
                    d.diff_files.push(DiffFile::unchanged("api/other", "other"));
                } else {
                    d.diff_files.pop();
                }
                d.review.view = Some((line(10), line(50)));
                d.diff
                    .update(cx, |state, cx| state.set_files(d.diff_files.clone(), cx));
                assert!(!d.diff_rows(cx).iter().any(|r| r.spot == line(10)));
                assert!(d.restore_view(cx));
                assert_eq!(d.review.view, Some((line(10), line(50))));
                assert_eq!(d.diff.read(cx).selected_lines().unwrap().start(), 50);
            }

            let mut comment =
                Comment::from_range(&DiffLineRange::new("api/f", DiffSide::Modified, 10, 10));
            assert!(
                d.on_its_line(&comment),
                "context comments stay attached to their source line"
            );
            assert!(
                !d.in_one_hunk(&comment),
                "expanded context is not a GitHub anchor"
            );
            comment.start.1 = 30;
            comment.end.1 = 30;
            assert!(d.in_one_hunk(&comment));
            comment.start.0 = true;
            comment.end.0 = true;
            comment.rev = "old-base".into();
            d.diff_revs.insert("api".into(), "new-base".into());
            assert!(!d.on_its_line(&comment));
            assert!(!d.in_one_hunk(&comment));
        });
    }

    #[gpui_kit::test]
    fn rows_follow_the_diff_layout(cx: &mut TestAppContext) {
        // Whole-file context narrows to git's default hunks; 7 lines apart stay separate.
        let line = |k: u8, n: usize| (k, n, n);
        let hunk = |changes: &[usize]| -> Hunk {
            (1..=30)
                .map(|n| line(if changes.contains(&n) { b'+' } else { b' ' }, n))
                .collect()
        };
        let spans = |hunks: Vec<Hunk>| -> Vec<(usize, usize)> {
            let span = |h: &Hunk| (h[0].2, h[h.len() - 1].2);
            hunks.iter().map(span).collect()
        };
        assert_eq!(spans(narrow_hunks(vec![hunk(&[10])])), [(7, 13)]);
        assert_eq!(spans(narrow_hunks(vec![hunk(&[10, 17])])), [(7, 20)]);
        assert_eq!(
            spans(narrow_hunks(vec![hunk(&[10, 18])])),
            [(7, 13), (15, 21)]
        );
        assert_eq!(
            spans(narrow_hunks(vec![hunk(&[1, 30])])),
            [(1, 4), (27, 30)]
        );

        let hunks = hunks(&DiffFile::parse(PATCH).unwrap()[0]);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0][3], (b'+', 0, 2));
        assert_eq!(hunks[1], [(b' ', 10, 10), (b'+', 0, 11)]);
        let patch = PATCH.replace("a/f", "a/r/f").replace("b/f", "b/r/f");
        let mut files = DiffFile::parse(&patch).unwrap();
        files.push(DiffFile::unchanged("r/g", ""));
        let state = cx.new(|cx| DiffState::new(files, cx));
        let unified = state.read_with(cx, |d, _| rows(d));
        assert_eq!(
            spots(&unified),
            [
                "F r/f",
                "Modified 1",
                "Original 2",
                "Original 3",
                "Modified 2",
                "Modified 3",
                "Modified 10",
                "Modified 11",
                "F r/g"
            ]
        );
        state.update(cx, |d, cx| d.set_mode(DiffMode::Split, cx));
        let split = state.read_with(cx, |d, _| rows(d));
        assert_eq!(spots(&split)[2..4], ["Modified 2", "Original 3"]);
        state.update(cx, |d, cx| d.set_file_collapsed("r/f", true, cx));
        assert_eq!(state.read_with(cx, |d, _| rows(d).len()), 2);

        // A refresh that dropped the line lands on the closest one above it.
        let gone = Spot::Line(DiffLinePosition::new("r/f", DiffSide::Modified, 7));
        assert_eq!(find(&unified, &gone), 5);
        assert_eq!(find(&unified, &Spot::File("r/x".into())), 0);
        // An original line on a context row, or paired beside a working line in split.
        let orig = |n| Spot::Line(DiffLinePosition::new("r/f", DiffSide::Original, n));
        assert_eq!(find(&unified, &orig(10)), 6);
        assert_eq!(find(&split, &orig(2)), 2);
        assert_eq!(next_change(&unified, 0, true), Some(2));
        assert_eq!(next_change(&unified, 2, true), Some(7));
        assert_eq!(next_change(&unified, 7, true), Some(2), "wraps");
        // Cursor at the end of a 9-row (11 high) diff on a 10-row screen: the top can't pass
        // the point where the end shows.
        assert_eq!(fit(&unified, 0, 8, 10.), 2);
        assert_eq!(fit(&unified, 5, 1, 10.), 0, "scrolloff above");
    }
}
