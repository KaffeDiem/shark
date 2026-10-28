//! Tabstops use the editor's selections, including multiple selections for mirrored fields.
use super::*;
use serde::Deserialize;
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

#[derive(Clone, Deserialize, Debug, PartialEq, Eq)]
struct Stop {
    index: u32,
    start: usize,
    end: usize,
}

#[derive(Deserialize)]
struct Completion {
    version: i32,
    fingerprint: u64,
    cursor: usize,
    tabstops: Vec<Stop>,
}

impl FileEditor {
    pub(super) fn snippet_changed(&mut self, cx: &mut Context<Self>) {
        if self.snippets.active.is_none()
            && !self
                .input
                .read(cx)
                .completion_menu_state()
                .items
                .iter()
                .any(|item| {
                    item.data
                        .as_ref()
                        .is_some_and(|data| data.get("sharkCompletion").is_some())
                })
        {
            return;
        }
        let text = self.input.read(cx).value().to_string();
        if let Some(session) = self.snippets.active.as_mut() {
            if !session.changed(&text) {
                self.snippets.active = None;
            }
            return;
        }
        let Some(language) = &self.intelligence else {
            return;
        };
        let version = language.snapshot.lock().unwrap().version;
        let metadata: Vec<Completion> = self
            .input
            .read(cx)
            .completion_menu_state()
            .items
            .iter()
            .filter_map(|item| {
                serde_json::from_value(item.data.as_ref()?.get("sharkCompletion")?.clone()).ok()
            })
            .filter(|m: &Completion| {
                m.version == version && self.snippets.accepted != Some((m.version, m.fingerprint))
            })
            .collect();
        if metadata.is_empty() {
            return;
        }
        let fp = fingerprint(text.as_bytes());
        let Some(completion) = metadata.into_iter().find(|m| m.fingerprint == fp) else {
            return;
        };
        self.snippets.accepted = Some((version, fp));
        if completion.tabstops.iter().any(|s| {
            s.start > s.end
                || s.end > text.len()
                || !text.is_char_boundary(s.start)
                || !text.is_char_boundary(s.end)
        }) {
            return;
        }
        let index = completion
            .tabstops
            .iter()
            .map(|s| s.index)
            .min_by_key(|i| order(*i));
        match index {
            Some(index) => {
                self.snippets.active = Some(Session {
                    text,
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
        if self.input.read(cx).value().as_ref() != session.text {
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

fn order(index: u32) -> u32 {
    if index == 0 { u32::MAX } else { index }
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
                && active.iter().any(|a| {
                    old_start >= a.start
                        && old_end <= a.end
                        && !(stop.index == 0 && old_start == a.end)
                })
            {
                return false;
            }
            let before_start = active
                .iter()
                .filter(|a| {
                    a.end <= old_start && !(stop.index == self.index && a.start == old_start)
                })
                .count();
            let before_end = active.iter().filter(|a| a.end <= old_end).count();
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
}
