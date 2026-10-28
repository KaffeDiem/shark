//! Tabstops use the editor's selections, including multiple selections for mirrored fields.
use super::*;
use crate::lsp::document::{CompletionMeta, Stop, tab_order as order};
use serde::Deserialize as _;
use std::ops::Range;

#[derive(Default)]
pub(super) struct Snippets {
    active: Option<Session>,
    accepted: Option<(i32, u64)>,
}

struct Session {
    text: String,
    stops: Vec<Stop>,
    index: u32,
}

impl FileEditor {
    pub(super) fn snippet_changed(&mut self, cx: &mut Context<Self>) {
        // A snippet completion accepted inside an active field replaces the outer session.
        if self.snippet_accepted(cx) {
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
    fn snippet_accepted(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(language) = &self.intelligence else {
            return false;
        };
        let snapshot = language.snapshot.lock().unwrap().clone();
        let input = self.input.read(cx);
        let len = input.text().len();
        let metadata: Vec<CompletionMeta> = input
            .completion_menu_state()
            .items
            .iter()
            .filter_map(|item| {
                let m = item.data.as_ref()?.get("sharkCompletion")?;
                (m["version"] == snapshot.version && m["len"] == len)
                    .then(|| CompletionMeta::deserialize(m).ok())?
            })
            .filter(|m| self.snippets.accepted != Some((m.version, m.fingerprint)))
            .collect();
        if metadata.is_empty() {
            return false;
        }
        let text = input.value();
        let tail = |m: &CompletionMeta| {
            len.checked_sub(m.end)
                .and_then(|n| snapshot.text.len().checked_sub(n))
        };
        let Some(completion) = metadata.into_iter().find(|m| {
            text.get(m.start..m.end)
                .is_some_and(|span| fingerprint(span.as_bytes()) == m.fingerprint)
                && snapshot.text.get(..m.start) == text.get(..m.start)
                && tail(m).and_then(|t| snapshot.text.get(t..)) == text.get(m.end..)
        }) else {
            return false;
        };
        self.snippets.accepted = Some((completion.version, completion.fingerprint));
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

    fn select_snippet(&self, cx: &mut Context<Self>) {
        let Some(session) = &self.snippets.active else {
            return;
        };
        let ranges: Vec<Range<usize>> = session
            .stops
            .iter()
            .filter(|s| s.index == session.index)
            .map(|s| s.start..s.end)
            .collect();
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

    pub(super) fn snippet_key(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.snippets.active.as_mut() else {
            return false;
        };
        if *self.input.read(cx).text() != *session.text {
            self.snippets.active = None;
            return false;
        }
        if self.vim_on && self.vim.mode != vim::Mode::Insert {
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
        if !session
            .stops
            .iter()
            .filter(|s| s.index == session.index)
            .any(|s| selected.start >= s.start && selected.end <= s.end)
        {
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
    /// GPUI edits all mirrored selections together; unchanged gaps prove this is still our snippet.
    fn changed(&mut self, new: &str) -> bool {
        let mut active: Vec<_> = self
            .stops
            .iter()
            .filter(|s| s.index == self.index)
            .cloned()
            .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn tabstops_follow_unicode_and_mirrored_edits() {
        let mut session = Session {
            text: "f(arg, arg, next)".into(),
            index: 1,
            stops: vec![
                Stop {
                    index: 1,
                    start: 2,
                    end: 5,
                },
                Stop {
                    index: 1,
                    start: 7,
                    end: 10,
                },
                Stop {
                    index: 2,
                    start: 12,
                    end: 16,
                },
                Stop {
                    index: 0,
                    start: 17,
                    end: 17,
                },
            ],
        };
        assert!(session.changed("f(😀, 😀, next)"));
        assert_eq!(
            session.stops[0],
            Stop {
                index: 1,
                start: 2,
                end: 6
            }
        );
        assert_eq!(
            session.stops[1],
            Stop {
                index: 1,
                start: 8,
                end: 12
            }
        );
        assert_eq!(
            session.stops[2],
            Stop {
                index: 2,
                start: 14,
                end: 18
            }
        );
        assert!(!session.changed("other text"));
    }
    #[::core::prelude::v1::test]
    fn replacing_outer_placeholder_removes_nested_stops() {
        let mut session = Session {
            text: "fn name(x)".into(),
            index: 1,
            stops: vec![
                Stop {
                    index: 1,
                    start: 3,
                    end: 10,
                },
                Stop {
                    index: 2,
                    start: 8,
                    end: 9,
                },
                Stop {
                    index: 0,
                    start: 10,
                    end: 10,
                },
            ],
        };
        assert!(session.changed("fn done"));
        assert!(session.stops.iter().all(|s| s.index != 2));
    }

    #[::core::prelude::v1::test]
    fn adjacent_empty_tabstops_survive_typing_in_the_first() {
        let mut session = Session {
            text: "f()".into(),
            index: 1,
            stops: vec![
                Stop {
                    index: 1,
                    start: 2,
                    end: 2,
                },
                Stop {
                    index: 2,
                    start: 2,
                    end: 2,
                },
                Stop {
                    index: 0,
                    start: 3,
                    end: 3,
                },
            ],
        };
        assert!(session.changed("f(x)"));
        assert_eq!(
            session.stops,
            vec![
                Stop {
                    index: 1,
                    start: 2,
                    end: 3
                },
                Stop {
                    index: 2,
                    start: 3,
                    end: 3
                },
                Stop {
                    index: 0,
                    start: 4,
                    end: 4
                },
            ]
        );
        assert!(session.changed("f(xy)"));
        assert_eq!(
            session.stops[1],
            Stop {
                index: 2,
                start: 4,
                end: 4
            }
        );
        session.index = 2;
        assert!(session.changed("f(xy😀)"));
        assert_eq!(
            session.stops[0],
            Stop {
                index: 1,
                start: 2,
                end: 4
            }
        );
        assert_eq!(
            session.stops[1],
            Stop {
                index: 2,
                start: 4,
                end: 8
            }
        );
        assert_eq!(
            session.stops[2],
            Stop {
                index: 0,
                start: 9,
                end: 9
            }
        );
        // Shift-Tab returns to only the first field, leaving the second field intact.
        session.index = 1;
        assert!(session.changed("f(z😀)"));
        assert_eq!(
            session.stops,
            vec![
                Stop {
                    index: 1,
                    start: 2,
                    end: 3
                },
                Stop {
                    index: 2,
                    start: 3,
                    end: 7
                },
                Stop {
                    index: 0,
                    start: 8,
                    end: 8
                },
            ]
        );
    }
}
