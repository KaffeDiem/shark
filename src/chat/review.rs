//! Diff reviews as chat messages: the card, the reports claude replies with.
use super::*;

/// Ends the context notes before a message. They may hold blank lines, so a blank line can't.
pub(super) const NOTES_END: &str = "\n[/shark]\n\n";

/// A message as it went on the wire, split into the `[shark]` context notes put before it
/// (`pending_context`) and the text itself. Older messages end their notes with a blank line.
pub(super) fn split_notes(wire: &str) -> (&str, &str) {
    let split = wire.split_once(NOTES_END).or(wire.split_once("\n\n"));
    match split {
        Some((notes, text)) if notes.starts_with("[shark] ") => (notes, text),
        _ => ("", wire),
    }
}

/// The rows context notes show as: their `[shark]` lines, without what a note goes on to quote.
pub(super) fn info_notes(notes: &str) -> impl Iterator<Item = Block> {
    (notes.lines())
        .filter(|l| l.starts_with("[shark] "))
        .map(|l| Block::Info(l.into()))
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
        if self.busy || self.held().is_some() {
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
