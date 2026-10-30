//! Tabstops use the editor's selections, including multiple selections for mirrored fields.
use super::*;
use crate::lsp::document::{CompletionMeta, Stop, tab_order as order};
use serde::Deserialize as _;
use std::ops::Range;

#[derive(Default)]
pub(super) struct Snippets {
    pub(super) active: Option<Session>,
    accepted: Option<(i32, u64)>,
    /// Resolves the last accepted item's imports; a newer acceptance, undo, reload or closing
    /// cancels it.
    pub(super) resolve: Option<Task<()>>,
}

pub(super) struct Session {
    pub(super) text: String,
    pub(super) stops: Vec<Stop>,
    pub(super) index: u32,
}

impl FileEditor {
    pub(super) fn snippet_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A snippet completion accepted inside an active field replaces the outer session.
        if self.snippet_accepted(window, cx) {
            return;
        }
        let Some(session) = self.snippets.active.as_mut() else {
            return;
        };
        if !session.changed(&self.input.read(cx).value()) {
            self.snippets.active = None;
        }
    }

    /// Starts the tabstops of the completion this change accepted, if any. GPUI keeps the last
    /// menu's items after it closes, so this runs on every change: check metadata before text.
    fn snippet_accepted(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(language) = &self.intelligence else {
            return false;
        };
        let snapshot = language.snapshot.lock().unwrap().clone();
        let input = self.input.read(cx);
        let len = input.text().len();
        let metadata: Vec<(CompletionMeta, Option<serde_json::Value>)> = input
            .completion_menu_state()
            .items
            .iter()
            .filter_map(|item| {
                let data = item.data.as_ref()?;
                let m = data.get("sharkCompletion")?;
                (m["version"] == snapshot.version && m["len"] == len).then(|| {
                    Some((
                        CompletionMeta::deserialize(m).ok()?,
                        data.get("sharkResolve").cloned(),
                    ))
                })?
            })
            .filter(|(m, _)| self.snippets.accepted != Some((m.version, m.fingerprint)))
            .collect();
        if metadata.is_empty() {
            return false;
        }
        let text = input.value();
        let tail = |m: &CompletionMeta| {
            len.checked_sub(m.end)
                .and_then(|n| snapshot.text.len().checked_sub(n))
        };
        let Some((completion, resolve)) = metadata.into_iter().find(|(m, _)| {
            text.get(m.start..m.end)
                .is_some_and(|span| fingerprint(span.as_bytes()) == m.fingerprint)
                && snapshot.text.get(..m.start) == text.get(..m.start)
                && tail(m).and_then(|t| snapshot.text.get(t..)) == text.get(m.end..)
        }) else {
            return false;
        };
        self.snippets.accepted = Some((completion.version, completion.fingerprint));
        if let Some(item) = resolve {
            self.resolve_accepted(item, snapshot.text.clone(), completion.start, window, cx);
        }
        // A plain completion typed into a field is an edit of that field; a snippet replaces it.
        if !(completion.tabstops.is_empty()
            && self
                .snippets
                .active
                .as_mut()
                .is_some_and(|s| s.changed(&text)))
        {
            self.snippets.active = None;
        }
        if completion.tabstops.iter().any(|s| {
            s.start > s.end
                || s.end > text.len()
                || !text.is_char_boundary(s.start)
                || !text.is_char_boundary(s.end)
        }) {
            return true;
        }
        let index = completion
            .tabstops
            .iter()
            .map(|s| s.index)
            .min_by_key(|i| order(*i));
        match index {
            Some(index) => {
                self.snippets.active = Some(Session {
                    text: text.to_string(),
                    stops: completion.tabstops,
                    index,
                });
                self.select_snippet(cx);
                if index == 0 {
                    self.snippets.active = None;
                }
            }
            None => {
                let cursor = completion.cursor.min(text.len());
                if text.is_char_boundary(cursor) {
                    self.input
                        .update(cx, |i, cx| i.set_selected_range(cursor..cursor, cx));
                }
            }
        }
        true
    }

    /// GPUI has no accepted-item hook, so an item shown unresolved gets its auto-imports after
    /// acceptance, as a second undo step.
    fn resolve_accepted(
        &mut self,
        item: serde_json::Value,
        before: std::sync::Arc<str>,
        start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.intelligence.as_ref().and_then(|l| l.client.clone()) else {
            return;
        };
        let encoding = client.encoding();
        let alive = self.intelligence.as_ref().map(|l| l.alive.clone());
        self.snippets.resolve = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(response) = client.request("completionItem/resolve", item).await else {
                return;
            };
            let Some(edits) = serde_json::from_value::<lsp_types::CompletionItem>(response)
                .ok()
                .and_then(|item| item.additional_text_edits)
            else {
                return;
            };
            // A detached or restarted session dropped the document this acceptance belonged to.
            if !alive.is_some_and(|a| a.load(std::sync::atomic::Ordering::Acquire)) {
                return;
            }
            let _ = this.update_in(cx, |e, window, cx| {
                e.apply_completion_imports(&before, start, &edits, encoding, window, cx)
            });
        }));
    }

    pub(super) fn apply_completion_imports(
        &mut self,
        before: &str,
        start: usize,
        edits: &[lsp_types::TextEdit],
        encoding: crate::lsp::document::Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = self.input.read(cx).value().to_string();
        let Some((text, end)) = completion_imports(before, &now, start, edits, encoding) else {
            return;
        };
        // Imports land before the accepted span, so the active snippet moves by their length.
        let delta = text.len() as isize - now.len() as isize;
        let selected = self.input.read(cx).selected_range();
        let fields = (self.snippets.active.as_mut()).map(|s| s.shift(end, delta, &text, selected));
        if matches!(fields, Some(None)) {
            self.snippets.active = None;
        }
        self.replace_language_text(text, window, cx);
        // The rewrite keeps only the main caret; put it back in every mirrored field.
        if let Some(Some(fields)) = fields {
            self.select_ranges(fields, cx);
        }
    }

    /// The first range becomes the main selection; the rest are added as mirrors.
    fn select_ranges(&self, ranges: Vec<Range<usize>>, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| {
            for (n, range) in ranges.into_iter().enumerate() {
                if n == 0 {
                    i.set_selected_range(range, cx);
                } else {
                    i.add_selection(range, cx);
                }
            }
        });
    }

    fn select_snippet(&self, cx: &mut Context<Self>) {
        let Some(session) = &self.snippets.active else {
            return;
        };
        let ranges = session.field().map(|s| s.start..s.end).collect();
        self.select_ranges(ranges, cx);
    }

    pub(super) fn snippet_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.snippets.active.as_mut() else {
            return false;
        };
        if *self.input.read(cx).text() != *session.text {
            self.snippets.active = None;
            return false;
        }
        if self.vim_on && !self.vim.typing() {
            self.snippets.active = None;
            return false;
        }
        if ks.key == "escape" {
            self.snippets.active = None;
            return false;
        }
        if ks.key != "tab" || ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
            return false;
        }
        let selected = self.input.read(cx).selected_range();
        if !(session.field()).any(|s| selected.start >= s.start && selected.end <= s.end) {
            self.snippets.active = None;
            return false;
        }
        let mut indexes: Vec<u32> = session.stops.iter().map(|s| s.index).collect();
        indexes.sort_by_key(|i| order(*i));
        indexes.dedup();
        let current = indexes
            .iter()
            .position(|i| *i == session.index)
            .unwrap_or(0);
        let next = if ks.modifiers.shift {
            current.checked_sub(1)
        } else {
            (current + 1 < indexes.len()).then_some(current + 1)
        };
        let Some(next) = next else {
            if !ks.modifiers.shift {
                self.snippets.active = None;
            }
            return true;
        };
        session.index = indexes[next];
        let final_stop = session.index == 0;
        self.select_snippet(cx);
        if final_stop {
            self.snippets.active = None;
        }
        true
    }
}

impl Session {
    /// The stops (mirrors) of the current tabstop.
    fn field(&self) -> impl Iterator<Item = &Stop> {
        self.stops.iter().filter(|s| s.index == self.index)
    }

    /// Moves every stop by `delta` for an edit that ends at or before `at`, returning the
    /// selections to restore: the caret's offsets within its field, in each mirror. `None` when
    /// a stop starts before `at`, so its position is unknown.
    fn shift(
        &mut self,
        at: usize,
        delta: isize,
        new: &str,
        selected: Range<usize>,
    ) -> Option<Vec<Range<usize>>> {
        if self.stops.iter().any(|s| s.start < at) {
            return None;
        }
        let field = (self.field())
            .find(|s| s.start <= selected.start && selected.end <= s.end)
            .map(|s| (selected.start - s.start, selected.end - s.start, s.start));
        for stop in &mut self.stops {
            stop.start = stop.start.checked_add_signed(delta)?;
            stop.end = stop.end.checked_add_signed(delta)?;
        }
        self.text = new.into();
        let Some((from, to, primary)) = field else {
            return Some(vec![]);
        };
        let primary = primary.checked_add_signed(delta)?;
        let mut active: Vec<_> = self.field().map(|s| s.start).collect();
        // The caret's own field first, so it stays the main selection.
        active.sort_by_key(|&start| start != primary);
        Some(active.into_iter().map(|s| s + from..s + to).collect())
    }

    /// GPUI edits all mirrored selections together; unchanged gaps prove this is still our snippet.
    fn changed(&mut self, new: &str) -> bool {
        let mut active: Vec<_> = self.field().cloned().collect();
        active.sort_by_key(|s| s.start);
        if active.is_empty() {
            return false;
        }
        let total_delta = new.len() as isize - self.text.len() as isize;
        if total_delta % active.len() as isize != 0 {
            return false;
        }
        let delta = total_delta / active.len() as isize;
        let mut old_at = 0;
        let mut new_at = 0;
        let mut replacement: Option<&str> = None;
        for stop in &active {
            if stop.start < old_at {
                return false;
            }
            let gap = stop.start - old_at;
            if new.get(new_at..new_at + gap) != self.text.get(old_at..stop.start) {
                return false;
            }
            new_at += gap;
            let Some(length) = (stop.end - stop.start).checked_add_signed(delta) else {
                return false;
            };
            let Some(value) = new.get(new_at..new_at + length) else {
                return false;
            };
            if replacement.is_some_and(|previous| previous != value) {
                return false;
            }
            replacement = Some(value);
            new_at += length;
            old_at = stop.end;
        }
        if new.get(new_at..) != self.text.get(old_at..) {
            return false;
        }
        self.stops.retain_mut(|stop| {
            let old_start = stop.start;
            let old_end = stop.end;
            if stop.index != self.index
                && active
                    .iter()
                    .any(|a| old_start >= a.start && old_start < a.end && old_end <= a.end)
            {
                return false;
            }
            let before_start = active
                .iter()
                .filter(|a| {
                    a.end <= old_start && !(stop.index == self.index && a.start == old_start)
                })
                .count();
            let before_end = active
                .iter()
                .filter(|a| {
                    a.end < old_end
                        || (a.end == old_end && (a.start < a.end || old_start == old_end))
                })
                .count();
            let Some(start) = old_start.checked_add_signed(delta * before_start as isize) else {
                return false;
            };
            let Some(end) = old_end.checked_add_signed(delta * before_end as isize) else {
                return false;
            };
            stop.start = start;
            stop.end = end;
            start <= end && end <= new.len()
        });
        self.text = new.into();
        true
    }
}

/// Applies import edits computed against `before` to `now` when they all precede the accepted
/// span at `start` and that prefix is unchanged; otherwise their positions are unknown. Returns
/// the text and the end of the edited prefix, after which offsets only shift.
pub(super) fn completion_imports(
    before: &str,
    now: &str,
    start: usize,
    edits: &[lsp_types::TextEdit],
    encoding: crate::lsp::document::Encoding,
) -> Option<(String, usize)> {
    let lines = crate::lsp::document::Lines::new(before);
    let mut ranges = (edits.iter())
        .map(|e| {
            let from = lines.offset(e.range.start, encoding).ok()?;
            Some((from, lines.offset(e.range.end, encoding).ok()?, &e.new_text))
        })
        .collect::<Option<Vec<_>>>()?;
    ranges.sort_by_key(|&(from, ..)| from);
    let end = ranges.iter().map(|&(_, to, _)| to).max()?;
    if end > start
        || ranges.windows(2).any(|w| w[0].1 > w[1].0)
        || before.get(..end)? != now.get(..end)?
    {
        return None;
    }
    let mut text = now.to_string();
    for (from, to, new) in ranges.into_iter().rev() {
        text.replace_range(from..to, new);
    }
    Some((text, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    fn stop(index: u32, start: usize, end: usize) -> Stop {
        Stop { index, start, end }
    }

    #[test]
    fn resolved_imports_apply_only_before_the_accepted_span_with_an_unchanged_prefix() {
        use lsp_types::{Position, Range, TextEdit};
        let utf16 = crate::lsp::document::Encoding::Utf16;
        let at = |line, character, text: &str| {
            TextEdit::new(
                Range::new(
                    Position::new(line, character),
                    Position::new(line, character),
                ),
                text.into(),
            )
        };
        let before = "# 😀\nsq";
        let import = [at(0, 0, "import m\n")];
        assert_eq!(
            completion_imports(before, "# 😀\nsqrt(x)", 7, &import, utf16),
            Some(("import m\n# 😀\nsqrt(x)".into(), 0))
        );
        // An edit after the accepted span, or a changed prefix, has no known position.
        assert_eq!(
            completion_imports(before, "# 😀\nsqrt", 7, &[at(1, 2, "!")], utf16),
            None
        );
        assert_eq!(
            completion_imports(before, "x# 😀\nsqrt", 7, &[at(0, 4, "!")], utf16),
            None
        );
    }

    #[test]
    fn imports_before_the_snippet_shift_its_stops_and_mirrored_carets() {
        let mut session = Session {
            text: "f(arg, arg)".into(),
            index: 1,
            stops: vec![stop(1, 2, 5), stop(1, 7, 10), stop(0, 11, 11)],
        };
        let new = "import m\nf(arg, arg)";
        assert_eq!(session.shift(0, 9, new, 8..8), Some(vec![17..17, 12..12]));
        assert_eq!(
            session.stops,
            vec![stop(1, 11, 14), stop(1, 16, 19), stop(0, 20, 20)]
        );
        assert!(session.changed("import m\nf(ab, ab)"));
        // An edit past a stop's start leaves its position unknown.
        assert_eq!(session.shift(12, 1, "x", 0..0), None);
    }

    #[test]
    fn tabstops_follow_unicode_and_mirrored_edits() {
        let mut session = Session {
            text: "f(arg, arg, next)".into(),
            index: 1,
            stops: vec![
                stop(1, 2, 5),
                stop(1, 7, 10),
                stop(2, 12, 16),
                stop(0, 17, 17),
            ],
        };
        assert!(session.changed("f(😀, 😀, next)"));
        assert_eq!(session.stops[0], stop(1, 2, 6));
        assert_eq!(session.stops[1], stop(1, 8, 12));
        assert_eq!(session.stops[2], stop(2, 14, 18));
        assert!(!session.changed("other text"));
    }

    #[test]
    fn replacing_outer_placeholder_removes_nested_stops() {
        let mut session = Session {
            text: "fn name(x)".into(),
            index: 1,
            stops: vec![stop(1, 3, 10), stop(2, 8, 9), stop(0, 10, 10)],
        };
        assert!(session.changed("fn done"));
        assert!(session.stops.iter().all(|s| s.index != 2));
    }

    #[test]
    fn adjacent_empty_tabstops_survive_typing_in_the_first() {
        let mut session = Session {
            text: "f()".into(),
            index: 1,
            stops: vec![stop(1, 2, 2), stop(2, 2, 2), stop(0, 3, 3)],
        };
        assert!(session.changed("f(x)"));
        assert_eq!(
            session.stops,
            vec![stop(1, 2, 3), stop(2, 3, 3), stop(0, 4, 4),]
        );
        assert!(session.changed("f(xy)"));
        assert_eq!(session.stops[1], stop(2, 4, 4));
        session.index = 2;
        assert!(session.changed("f(xy😀)"));
        assert_eq!(session.stops[0], stop(1, 2, 4));
        assert_eq!(session.stops[1], stop(2, 4, 8));
        assert_eq!(session.stops[2], stop(0, 9, 9));
        // Shift-Tab returns to only the first field, leaving the second field intact.
        session.index = 1;
        assert!(session.changed("f(z😀)"));
        assert_eq!(
            session.stops,
            vec![stop(1, 2, 3), stop(2, 3, 7), stop(0, 8, 8),]
        );
    }
}
