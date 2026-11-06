//! Diff reviews as chat messages: the card, the reports claude replies with.
use super::*;

/// Diff comments sent as one message (src/features/differ/review.rs), shown as a card instead of the
/// prompt text.
#[derive(Clone)]
pub struct ReviewCard {
    pub prompt: String,
    pub items: Vec<ReviewItem>,
    /// The diff when it was sent, e.g. "3 files · +5 −3".
    pub changes: String,
}

impl ReviewCard {
    pub fn ids(&self) -> Vec<u64> {
        self.items.iter().map(|i| i.id).collect()
    }
}

/// A `[c<id>] addressed: <note>` line, as `(id, note)`.
pub(super) fn report(l: &str) -> Option<(u64, String)> {
    let l = l.trim().trim_start_matches(['-', '*', '`', ' ']);
    let (id, rest) = l.strip_prefix("[c")?.split_once(']')?;
    let rest = rest.trim_start();
    let note = rest
        .get(..10)
        .filter(|p| p.eq_ignore_ascii_case("addressed:"))
        .map(|_| &rest[10..])?;
    Some((
        id.parse().ok()?,
        note.trim().trim_end_matches('`').trim().to_string(),
    ))
}

/// The reply's lines (with their ends), each with its report when it is one; lines in a code
/// block are none.
fn report_lines(reply: &str) -> impl Iterator<Item = (&str, Option<(u64, String)>)> {
    let mut fence: Option<&str> = None;
    reply.split_inclusive('\n').map(move |l| {
        let ticks = l.trim_start();
        let ticks = &ticks[..ticks.len() - ticks.trim_start_matches('`').len()];
        let inside = fence.is_some();
        match fence {
            None if ticks.len() >= 3 => fence = Some(ticks),
            Some(f) if ticks.starts_with(f) && l.trim().len() == ticks.len() => fence = None,
            _ => {}
        }
        (l, (!inside && fence.is_none()).then(|| report(l)).flatten())
    })
}

/// The reply's `[c<id>] addressed: <note>` lines, as `(id, note)`.
pub fn parse_reports(reply: &str) -> Vec<(u64, String)> {
    report_lines(reply).filter_map(|(_, r)| r).collect()
}

/// The reply as shown: without its `[c<id>] addressed:` lines, which only feed the diff's
/// threads (src/features/differ/review.rs).
pub(super) fn hide_reports(reply: &str) -> String {
    (report_lines(reply).filter(|(_, r)| r.is_none()))
        .map(|(l, _)| l)
        .collect()
}

/// A reply that is only report lines: nothing to show. Stops at the first other line, so
/// it is cheap per frame.
pub(super) fn hidden_reply(src: &str) -> bool {
    let mut any = false;
    for (l, r) in report_lines(src) {
        match r {
            Some(_) => any = true,
            None if l.trim().is_empty() => {}
            None => return false,
        }
    }
    any
}

/// Some line of `text` starts like a report (`[c<digit>`), unlike `#[cfg(...)]`.
pub(super) fn may_report(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start().trim_start_matches(['-', '*', '`', ' ']);
        l.strip_prefix("[c")
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// A message as it went on the wire, split into the `[shark]` workspace notes put before it
/// (`repos_changed`) and the text itself.
pub(super) fn split_notes(wire: &str) -> (&str, &str) {
    match wire.split_once("\n\n") {
        Some((notes, text)) if notes.starts_with("[shark] ") => (notes, text),
        _ => ("", wire),
    }
}

/// A review's prompt (review.rs `prompt`) back as its card, for a replayed transcript: each
/// `[c<id>] <place>` and its quoted text. The diff it was sent with is unknown by then.
pub(super) fn card_of(prompt: &str) -> Option<ReviewCard> {
    // A workspace note may come before it on the wire.
    let prompt = &prompt[prompt.find("Review comments on the diff.")?..];
    let (mut items, mut fence) = (Vec::<ReviewItem>::new(), None);
    for l in prompt.lines() {
        if let Some(f) = fence {
            fence = (l != f).then_some(f);
            continue;
        }
        if l.starts_with("```") {
            fence = Some(l);
        } else if let Some((id, rest)) = l.strip_prefix("[c").and_then(|l| l.split_once("] "))
            && let Ok(id) = id.parse()
        {
            let place = rest.split(" · ").next().unwrap_or(rest).to_string();
            items.push(ReviewItem {
                id,
                place,
                text: String::new(),
            });
        } else if let (Some(t), Some(it)) = (l.strip_prefix("> "), items.last_mut()) {
            if !it.text.is_empty() {
                it.text.push('\n');
            }
            it.text.push_str(t);
        }
    }
    (!items.is_empty()).then(|| ReviewCard {
        prompt: prompt.to_string(),
        items,
        changes: String::new(),
    })
}

/// One comment of a review card: its id, "app.rs · Working copy · lines 631–632", its text.
#[derive(Clone)]
pub struct ReviewItem {
    pub id: u64,
    pub place: String,
    pub text: String,
}

/// A chat started by a review: "Review · 3 comments".
pub(super) fn review_title(r: &ReviewCard) -> String {
    let n = r.items.len();
    format!("Review · {n} comment{}", plural(n))
}

impl Chat {
    /// Sends exactly a review's prompt, leaving the composer's draft alone. While a turn or
    /// git step runs it waits at the top of the queue. `ReviewSent` says when it goes out;
    /// false when it was refused (the transcript says why).
    pub fn send_review(&mut self, mut card: ReviewCard, cx: &mut Context<Self>) -> bool {
        // As the queue sends it, so claude's echo matches the card.
        card.prompt = card.prompt.trim().to_string();
        let card = Rc::new(card);
        if self.busy || self.hold {
            let q = Queued {
                review: Some(card.clone()),
                ..Queued::new(card.prompt.clone().into())
            };
            self.queue.insert(0, q);
            // The rows below it moved down one: the cursor and the edited slot follow them.
            self.qsel += 1;
            if let Some(e) = &mut self.editing {
                *e += 1;
            }
            self.after_queue_change(cx);
            return true;
        }
        self.send_block(card.prompt.clone(), false, Some(card), cx)
    }

    /// Comments of sent reviews claude hasn't echoed yet; with `held`, only ones sent mid-turn,
    /// which the turn that is ending never read.
    pub fn unread_review_ids(&self, held: bool) -> Vec<u64> {
        (self.unread.iter())
            .filter_map(|&bi| match self.blocks.get(bi) {
                Some(Block::Review(r, queued)) if *queued || !held => Some(r.ids()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Comments of reviews still waiting to be read: the tab shows "Chat · N".
    pub fn review_count(&self) -> usize {
        let queued = self.queue.iter().filter_map(|q| q.review.as_ref());
        let unread = (self.unread.iter()).filter_map(|&bi| match self.blocks.get(bi) {
            Some(Block::Review(r, _)) => Some(r),
            _ => None,
        });
        queued.chain(unread).map(|r| r.items.len()).sum()
    }

    /// A review as sent (screen 06): its comments, each linking to the diff, then a row that
    /// opens the diff. `footer` says where it stands.
    pub(super) fn review_card(
        &self,
        r: &ReviewCard,
        key: usize,
        footer: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let n = r.items.len();
        v_flex()
            .gap_2()
            .text_sm()
            .child(
                v_flex()
                    .gap_2()
                    .pl_3()
                    .py_2()
                    .border_l_2()
                    .border_color(theme.primary)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.success_foreground)
                            .child(match n {
                                1 => "Diff comment added to context".to_string(),
                                n => format!("{n} diff comments added to context"),
                            }),
                    )
                    .children(r.items.iter().map(|it| {
                        let id = it.id;
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .id(("review-link", (key << 16) + id as usize))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .cursor_pointer()
                                    .hover(|d| d.text_color(theme.foreground))
                                    .child(format!("{} ↗", it.place))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(ChatEvent::ShowComment(id))
                                    })),
                            )
                            .child(div().text_color(theme.foreground).child(it.text.clone()))
                    }))
                    .when_some(footer, |d, f| {
                        d.child(div().text_xs().text_color(theme.muted_foreground).child(f))
                    }),
            )
            .child(
                h_flex()
                    .id(("review-changes", key))
                    .h(px(32.))
                    .px_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.list_hover))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(ChatEvent::ShowChanges)
                    }))
                    .child(div().flex_1().child("Review changes"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(r.changes.clone()),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    /// A review prompt in a replayed transcript comes back as its card; report lines stay
    /// out of the shown reply.
    #[test]
    fn review_cards_and_reports() {
        let p = "note\n\nReview comments on the diff. Paths …:\n\n[c1] api/a.rs:12 · working tree\n````\n[c9] x\n> y\n````\n> rename\n> it\nSuggested change:\n```\nlet y;\n```\n\n[c4] api/b.rs:3-5 (old) · base 0123456789\n> keep\n\nEnd your reply with one line per comment you addressed, exactly like:\n[c<id>] addressed: <what you changed, in one line>\n";
        let card = card_of(p).unwrap();
        let items: Vec<_> = (card.items.iter())
            .map(|i| (i.id, i.place.as_str(), i.text.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                (1, "api/a.rs:12", "rename\nit"),
                (4, "api/b.rs:3-5 (old)", "keep")
            ]
        );
        assert!(card.prompt.starts_with("Review comments"));
        assert!(card_of("plain [c1] x\n> y").is_none());
        assert_eq!(
            hide_reports("Done.\n[c1] addressed: x\n- `[c2] addressed: y`\nBye"),
            "Done.\nBye"
        );
        // One in a code block is an example, not a report.
        let fenced = "See:\n```\n[c1] addressed: example\n```\n[c2] addressed: real";
        assert_eq!(parse_reports(fenced), [(2, "real".to_string())]);
        assert_eq!(
            hide_reports(fenced),
            "See:\n```\n[c1] addressed: example\n```\n"
        );
        assert!(hidden_reply("[c1] addressed: x\n"));
        assert!(hidden_reply("\n- [c1] addressed: x\n[c2] addressed: y"));
        assert!(!hidden_reply("Done.\n[c1] addressed: x"));
        assert!(!hidden_reply("") && !hidden_reply("#[cfg(test)]"));
        assert!(may_report("x\n - `[c12] addr") && may_report("[c3"));
        assert!(!may_report("#[cfg(test)]\n[cx] y\n[c"));
    }
}
