//! Diff review: a keyboard line cursor that survives refreshes, and line comments that go to
//! the agent as one prompt.
//!
//! gpui-kit's `DiffState` keeps its scroll offset private and `set_files` resets it, so the
//! keyboard view is tracked here instead: `j/k` move a cursor over a model of the diff's rows
//! and scroll by putting a row at the top with `scroll_to_line`. A refresh puts the same row
//! back. Mouse scrolling loses the model; `j/k` then scroll natively until the next jump.
use super::*;
use crate::state::support_dir;
use gpui_kit::component::diff::{DiffAnnotation, DiffLinePosition, DiffLineRange, DiffSide};
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// One hunk's patch lines: kind (`b' '`, `b'-'`, `b'+'`), old and new one-based line numbers.
pub(super) type Hunk = Vec<(u8, usize, usize)>;

/// Hunks of one file's patch chunk, so the cursor knows the rows the diff draws.
pub(super) fn parse_hunks(chunk: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = vec![];
    let (mut old, mut new, mut inside) = (0, 0, false);
    for line in chunk.lines() {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let start = |s: &str| s.split(',').next()?.parse::<usize>().ok();
            let mut parts = rest.split(' ');
            let o = parts.next().and_then(start);
            let n = parts.next().and_then(|s| start(s.strip_prefix('+')?));
            inside = o.is_some() && n.is_some();
            (old, new) = (o.unwrap_or(0), n.unwrap_or(0));
            if inside {
                hunks.push(vec![]);
            }
            continue;
        }
        let Some(h) = hunks.last_mut().filter(|_| inside) else {
            continue;
        };
        match line.as_bytes().first() {
            Some(b' ') | None => {
                h.push((b' ', old, new));
                (old, new) = (old + 1, new + 1);
            }
            Some(b'-') => {
                h.push((b'-', old, 0));
                old += 1;
            }
            Some(b'+') => {
                h.push((b'+', 0, new));
                new += 1;
            }
            Some(b'\\') => {}
            _ => inside = false,
        }
    }
    hunks
}

/// A row the cursor can sit on: a file header or a source line.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Spot {
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
pub(super) struct Row {
    spot: Spot,
    changed: bool,
    /// Height in code rows; headers and hunk separators are taller than a line.
    h: f32,
}

/// The diff's rows in display order, like `DiffState` projects them: unified mode lists a
/// change's removed lines before its added ones, split mode pairs them. Collapsed (viewed)
/// files are a header only.
// ponytail: rebuilt per key, ~1ms per 50k patch lines; cache per refresh if it shows up.
pub(super) fn rows(files: &[(SharedString, &[Hunk], bool)], split: bool) -> Vec<Row> {
    let mut out = vec![];
    for (path, hunks, collapsed) in files {
        out.push(Row {
            spot: Spot::File(path.clone()),
            changed: false,
            h: 2.,
        });
        if *collapsed {
            continue;
        }
        let line =
            |old: bool, n: usize| Spot::Line(DiffLinePosition::new(path.clone(), side(old), n));
        for hunk in hunks.iter() {
            let first = out.len();
            let mut i = 0;
            while i < hunk.len() {
                if hunk[i].0 == b' ' {
                    out.push(Row {
                        spot: line(false, hunk[i].2),
                        changed: false,
                        h: 1.,
                    });
                    i += 1;
                    continue;
                }
                let end = (i..hunk.len())
                    .find(|&j| hunk[j].0 == b' ')
                    .unwrap_or(hunk.len());
                let (dels, adds): (Vec<usize>, Vec<usize>) =
                    (i..end).partition(|&j| hunk[j].0 == b'-');
                let spots: Vec<Spot> = match split {
                    false => (dels.iter().map(|&j| line(true, hunk[j].1)))
                        .chain(adds.iter().map(|&j| line(false, hunk[j].2)))
                        .collect(),
                    true => (0..dels.len().max(adds.len()))
                        .map(|k| match adds.get(k) {
                            Some(&j) => line(false, hunk[j].2),
                            None => line(true, hunk[dels[k]].1),
                        })
                        .collect(),
                };
                out.extend(spots.into_iter().map(|spot| Row {
                    spot,
                    changed: true,
                    h: 1.,
                }));
                i = end;
            }
            if let Some(r) = out.get_mut(first) {
                r.h += 1.; // the hunk separator above it
            }
        }
    }
    out
}

/// The row showing `spot`, or the closest one before it in the same file after a refresh.
fn find(rows: &[Row], spot: &Spot) -> usize {
    if let Some(i) = rows.iter().position(|r| r.spot == *spot) {
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
        .filter(|(_, r)| {
            matches!(&r.spot, Spot::Line(l) if l.side() == want.side() && l.line() <= want.line())
        })
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

/// A comment on lines of the diff, waiting to be sent to the agent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(super) struct Comment {
    path: String,
    /// `(on the old side, one-based line)` of the first and last commented line.
    start: (bool, usize),
    end: (bool, usize),
    text: String,
}

impl Comment {
    fn from_range(r: &DiffLineRange) -> Self {
        let old = |s: DiffSide| s == DiffSide::Original;
        Comment {
            path: r.path().to_string(),
            start: (old(r.side()), r.start()),
            end: (old(r.end_side()), r.end()),
            text: String::new(),
        }
    }

    /// `api/a.rs:12`, `api/a.rs:12-14`, `api/a.rs:7 (old)`.
    fn place(&self) -> String {
        let n = |(old, line): (bool, usize)| match old {
            true => format!("{line} (old)"),
            false => line.to_string(),
        };
        match self.start == self.end {
            true => format!("{}:{}", self.path, n(self.start)),
            false if self.start.0 == self.end.0 => {
                format!("{}:{}-{}", self.path, self.start.1, n(self.end))
            }
            false => format!("{}:{}-{}", self.path, n(self.start), n(self.end)),
        }
    }

    fn anchor(&self) -> DiffLinePosition {
        DiffLinePosition::new(self.path.clone(), side(self.end.0), self.end.1)
    }
}

fn side(old: bool) -> DiffSide {
    if old {
        DiffSide::Original
    } else {
        DiffSide::Modified
    }
}

/// All comments as one prompt, in file order.
pub(super) fn prompt(comments: &[Comment]) -> String {
    let mut out = String::from(
        "Review comments on the diff. Paths are workspace-relative; \"(old)\" is a line number \
         in the base version (a removed line). Address each comment:\n",
    );
    for c in comments {
        out.push_str(&format!("\n{}\n", c.place()));
        for l in c.text.lines() {
            out.push_str(&format!("> {l}\n"));
        }
    }
    out
}

fn comments_path(ws: &str) -> PathBuf {
    support_dir().join("comments").join(format!("{ws}.json"))
}

#[derive(Default)]
pub(super) struct Review {
    /// Top row and cursor of the keyboard view; `None` once the mouse scrolled.
    view: Option<(Spot, Spot)>,
    pub(super) hunks: HashMap<SharedString, Vec<Hunk>>,
    /// Height of the diff pane as last painted.
    viewport: Rc<Cell<Pixels>>,
    /// Pending comments per workspace, loaded from disk on first use.
    comments: HashMap<String, Vec<Comment>>,
}

impl Review {
    /// `}`/`{` and the tree moved the diff to a file header.
    pub(super) fn at_file(&mut self, path: &str) {
        let s = Spot::File(path.to_string().into());
        self.view = Some((s.clone(), s));
    }

    fn save_comments(&self, ws: &str) {
        let path = comments_path(ws);
        match self.comments.get(ws).filter(|c| !c.is_empty()) {
            None => {
                let _ = std::fs::remove_file(path);
            }
            Some(c) => {
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let _ = std::fs::write(path, serde_json::to_vec(c).unwrap_or_default());
            }
        }
    }

    pub(super) fn load_comments(&mut self, ws: &str) {
        if !self.comments.contains_key(ws) {
            let c = std::fs::read(comments_path(ws))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            self.comments.insert(ws.to_string(), c);
        }
    }
}

impl Shark {
    fn diff_rows(&self, cx: &App) -> Vec<Row> {
        let d = self.diff.read(cx);
        let files: Vec<(SharedString, &[Hunk], bool)> = (self.diff_files.iter())
            .map(|f| {
                let hunks = self.review.hunks.get(f.path()).map_or(&[][..], |h| &h[..]);
                (f.path().clone(), hunks, d.is_file_collapsed(f.path()))
            })
            .collect();
        rows(&files, d.mode() == DiffMode::Split)
    }

    /// Scrolls `top` to the top of the diff and shows the cursor as the selected line.
    fn apply_view(&mut self, rows: &[Row], top: usize, cur: usize, cx: &mut Context<Self>) {
        let (Some(t), Some(c)) = (rows.get(top), rows.get(cur)) else {
            return;
        };
        let (t, c) = (t.spot.clone(), c.spot.clone());
        if let Some(ix) = self.diff_files.iter().position(|f| f.path() == c.path()) {
            self.diff_file = ix; // `v`/`o` act on the cursor's file
            if let Some(row) = self.tree_rows.iter().position(|r| r.file == Some(ix)) {
                self.tree_sel = row;
            }
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
        self.review.view = Some((t, c));
        cx.notify();
    }

    fn view_cap(&self, window: &Window, cx: &App) -> f32 {
        let row = window.rem_size() * (f32::from(cx.theme().mono_font_size) / 16.) * 1.6;
        (self.review.viewport.get() / row).max(SCROLLOFF * 2. + 1.)
    }

    /// `j/k` (`n` = ±1), `ctrl-d/u` (± half a screen), `gg`/`G` (`to` = first/last row).
    pub(super) fn diff_move(
        &mut self,
        n: isize,
        to: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cap = self.view_cap(window, cx);
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
                (true, true) => "diff::ScrollDown",
                (false, true) => "diff::ScrollUp",
                (true, false) => "diff::PageDown",
                (false, false) => "diff::PageUp",
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
        self.apply_view(&rows, t, c, cx);
    }

    /// `]c` / `[c`: the cursor goes to the next change; natively without a keyboard view.
    pub(super) fn diff_change(
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
        let t = fit(&rows, find(&rows, &top), c, self.view_cap(window, cx));
        self.apply_view(&rows, t, c, cx);
    }

    /// After `set_files` reset the diff, put the keyboard view back; false when there is none.
    pub(super) fn restore_view(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((top, cur)) = self.review.view.clone() else {
            return false;
        };
        let rows = self.diff_rows(cx);
        let (t, c) = (find(&rows, &top), find(&rows, &cur));
        self.apply_view(&rows, t, c, cx);
        true
    }

    /// The mouse scrolled: the view model no longer matches the screen.
    pub(super) fn lose_view(&mut self) {
        self.review.view = None;
    }

    /// Tracks the pane's height for paging; goes in the diff pane.
    pub(super) fn viewport_probe(&self) -> impl IntoElement {
        let vp = self.review.viewport.clone();
        canvas(move |b, _, _| vp.set(b.size.height), |_, _, _, _| {})
            .absolute()
            .size_full()
    }

    pub(super) fn ws_comments(&self) -> &[Comment] {
        self.active
            .as_ref()
            .and_then(|ws| self.review.comments.get(ws))
            .map_or(&[], |c| &c[..])
    }

    /// The pending comments as diff annotations, under their last line.
    pub(super) fn with_comments(&self, diff: Diff, cx: &App) -> Diff {
        let comments = self.ws_comments();
        let texts: Rc<Vec<String>> = Rc::new(comments.iter().map(|c| c.text.clone()).collect());
        let (muted, fg) = (cx.theme().muted_foreground, cx.theme().foreground);
        diff.annotations(
            comments
                .iter()
                .enumerate()
                .map(|(i, c)| DiffAnnotation::line(("comment", i), c.anchor())),
        )
        .render_annotation(move |a, _, _| {
            let text = match a.id() {
                ElementId::NamedInteger(_, i) => texts.get(*i as usize).cloned(),
                _ => None,
            };
            h_flex()
                .gap_2()
                .items_start()
                .text_sm()
                .child(
                    Icon::new(IconName::MessageSquare)
                        .with_size(px(14.))
                        .text_color(muted)
                        .mt_0p5(),
                )
                .child(
                    div()
                        .flex_1()
                        .text_color(fg)
                        .whitespace_normal()
                        .child(text.unwrap_or_default()),
                )
        })
    }

    /// `c`: comment on the selected lines (the cursor line); an existing one is edited, and
    /// saving it empty removes it.
    pub(super) fn comment_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.active.clone() else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let Some(range) = self.diff.read(cx).selected_lines() else {
            return Self::notify_err(window, cx, "Move to a line first (j/k), or select lines");
        };
        let draft = Comment::from_range(&range);
        let text = (self.ws_comments().iter())
            .find(|c| c.place() == draft.place())
            .map(|c| c.text.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("What should the agent change here?")
                .default_value(text)
        });
        let (shark, title) = (
            cx.entity().downgrade(),
            format!("Comment on {}", draft.place()),
        );
        let field = input.clone();
        window.open_dialog(cx, move |d, _, _| {
            let (shark, input, ws, draft) =
                (shark.clone(), input.clone(), ws.clone(), draft.clone());
            d.font_features(inter_features())
                .title(title.clone())
                .w(px(560.))
                .child(Input::new(&input))
                .on_ok(move |_, _, cx| {
                    let text = input.read(cx).value().trim().to_string();
                    let comment = Comment {
                        text,
                        ..draft.clone()
                    };
                    shark
                        .update(cx, |s, cx| s.put_comment(&ws, comment, cx))
                        .ok();
                    true
                })
        });
        field.update(cx, |i, cx| i.focus(window, cx));
    }

    fn put_comment(&mut self, ws: &str, c: Comment, cx: &mut Context<Self>) {
        self.review.load_comments(ws);
        let list = self.review.comments.entry(ws.to_string()).or_default();
        list.retain(|o| o.place() != c.place());
        if !c.text.is_empty() {
            list.push(c);
            // File order, then line, so the prompt reads top to bottom.
            let order: HashMap<&str, usize> = (self.diff_files.iter().enumerate())
                .map(|(i, f)| (f.path().as_str(), i))
                .collect();
            list.sort_by_key(|c| (order.get(c.path.as_str()).copied(), c.start.1));
        }
        self.review.save_comments(ws);
        cx.notify();
    }

    /// `shift-c`: sends every pending comment as one prompt, then clears them.
    pub(super) fn send_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(ws), Some(chat)) = (self.active.clone(), self.active_chat().cloned()) else {
            return Self::notify_err(window, cx, "Open a workspace first");
        };
        let comments = self.ws_comments().to_vec();
        if comments.is_empty() {
            return Self::notify_err(window, cx, "No comments yet (c on a diff line)");
        }
        // Refused (on hold, archiving, spawn failed): the chat says why; the comments stay.
        if !chat.update(cx, |c, cx| c.ask(&prompt(&comments), window, cx)) {
            return Self::notify_err(window, cx, "Comments not sent; the chat says why");
        }
        self.review.comments.insert(ws.clone(), vec![]);
        self.review.save_comments(&ws);
        let n = comments.len();
        window.push_notification(
            format!(
                "Sent {n} comment{} to the agent",
                if n == 1 { "" } else { "s" }
            ),
            cx,
        );
        self.show(Tab::Chat, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    const PATCH: &str = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@ fn x\n a\n-b\n-c\n+B\n d\n\\ No newline at end of file\n@@ -10 +10,2 @@\n x\n+y\n";

    fn spots(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| match &r.spot {
                Spot::File(p) => format!("F {p}"),
                Spot::Line(l) => format!("{:?} {}", l.side(), l.line()),
            })
            .collect()
    }

    #[test]
    fn rows_follow_the_diff_layout() {
        let hunks = parse_hunks(PATCH);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0][3], (b'+', 0, 2));
        assert_eq!(hunks[1], [(b' ', 10, 10), (b'+', 0, 11)]);
        let files = [
            ("r/f".into(), &hunks[..], false),
            ("r/g".into(), &[][..], false),
        ];
        let unified = rows(&files, false);
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
        let split = rows(&files, true);
        assert_eq!(spots(&split)[2..4], ["Modified 2", "Original 3"]);
        assert_eq!(rows(&[("r/f".into(), &hunks[..], true)], false).len(), 1);

        // A refresh that dropped the line lands on the closest one above it.
        let gone = Spot::Line(DiffLinePosition::new("r/f", DiffSide::Modified, 7));
        assert_eq!(find(&unified, &gone), 5);
        assert_eq!(find(&unified, &Spot::File("r/x".into())), 0);
        assert_eq!(next_change(&unified, 0, true), Some(2));
        assert_eq!(next_change(&unified, 2, true), Some(7));
        assert_eq!(next_change(&unified, 7, true), Some(2), "wraps");
        // Cursor at the end of a 9-row (11 high) diff on a 10-row screen: the top can't pass
        // the point where the end shows.
        assert_eq!(fit(&unified, 0, 8, 10.), 2);
        assert_eq!(fit(&unified, 5, 1, 10.), 0, "scrolloff above");
    }

    #[test]
    fn prompt_lists_places() {
        let c = |start, end, text: &str| Comment {
            path: "api/a.rs".into(),
            start,
            end,
            text: text.into(),
        };
        let p = prompt(&[
            c((false, 12), (false, 12), "rename"),
            c((true, 3), (true, 5), "keep\nthis"),
            c((true, 7), (false, 8), "x"),
        ]);
        assert!(p.contains("\napi/a.rs:12\n> rename\n"));
        assert!(p.contains("\napi/a.rs:3-5 (old)\n> keep\n> this\n"));
        assert!(p.contains("\napi/a.rs:7 (old)-8\n"));
    }
}
