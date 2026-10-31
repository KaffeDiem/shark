//! Messages typed while the agent runs.
use super::*;

/// A message typed while the agent runs. The top one goes out when a turn ends well; any can
/// go out now to steer the running turn.
pub(super) struct Queued {
    /// With its chips, so editing puts them back.
    pub(super) content: InputContent,
    /// First line, capped, so a long paste isn't shaped on every frame.
    pub(super) preview: SharedString,
    /// A review waiting for the turn to end; it goes out as its card.
    pub(super) review: Option<Rc<ReviewCard>>,
    /// GitHub reviews are acknowledged only once this prompt is sent.
    pub(super) git_reviews: Vec<String>,
}

impl Queued {
    pub(super) fn new(content: InputContent) -> Self {
        let first = content.text().trim().lines().next().unwrap_or_default();
        let mut preview: String = first.chars().take(200).collect();
        if preview.len() < content.text().trim().len() {
            preview.push('…');
        }
        Queued {
            content,
            preview: preview.into(),
            review: None,
            git_reviews: vec![],
        }
    }

    pub(super) fn text(&self) -> String {
        self.content.text().trim().to_string()
    }
}

impl Chat {
    /// Sends the top queued message if the agent is free.
    pub fn drain(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.hold || self.editing.is_some() || self.queue.is_empty() {
            return;
        }
        let q = self.queue.remove(0); // editing is None here
        match self.send_block(q.text(), false, q.review.clone(), cx) {
            true => {
                self.qsel = self.qsel.saturating_sub(1);
                cx.emit(ChatEvent::GitReviewsSent(q.git_reviews));
            }
            false => self.queue.insert(0, q),
        }
        self.after_queue_change(cx);
    }

    /// Where a message goes back into the queue: the slot of the one being edited, else last.
    pub(super) fn queue_slot(&self) -> usize {
        self.editing
            .unwrap_or(self.queue.len())
            .min(self.queue.len())
    }

    /// Queues the composer's content in `queue_slot`, taking the git reviews of the edit; the slot.
    pub(super) fn queue_draft(&mut self, content: InputContent) -> usize {
        let at = self.queue_slot();
        let git_reviews = std::mem::take(&mut self.editing_git_reviews);
        self.queue.insert(
            at,
            Queued {
                git_reviews,
                ..Queued::new(content)
            },
        );
        at
    }

    /// The draft is out (sent or queued): leave edit mode and clear the composer.
    pub(super) fn finish_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
    }

    /// Removes a queued message, keeping the slot of the one being edited in place.
    fn take_queued(&mut self, ix: usize) -> Queued {
        if let Some(e) = &mut self.editing
            && *e > ix
        {
            *e -= 1;
        }
        self.queue.remove(ix)
    }

    pub(super) fn after_queue_change(&mut self, cx: &mut Context<Self>) {
        self.qsel = self.qsel.min(self.queue.len().saturating_sub(1));
        self.qscroll.scroll_to_item(self.qsel);
        cx.notify();
    }

    pub fn focus_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.queue.is_empty() {
            return self.focus_composer(window, cx);
        }
        self.qfocus.focus(window, cx);
        self.after_queue_change(cx);
    }

    fn queue_select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // The composer sits below the last row, so moving past it lands there.
        if ix >= self.queue.len() {
            return self.focus_composer(window, cx);
        }
        self.qsel = ix;
        self.after_queue_change(cx);
    }

    /// Sends the selected message into the running turn.
    pub(super) fn steer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.qsel >= self.queue.len() {
            return;
        }
        if self.hold {
            self.push(
                Block::Info("a git step is running; it stays queued".into()),
                cx,
            );
            return;
        }
        let editing = self.editing;
        let q = self.take_queued(self.qsel);
        let busy = self.busy;
        if self.send_block(q.text(), true, q.review.clone(), cx) {
            // Claude may hold it unread until this turn ends; don't restart the process under it.
            self.steered |= busy;
            cx.emit(ChatEvent::GitReviewsSent(q.git_reviews));
        } else {
            self.editing = editing;
            self.queue.insert(self.qsel, q);
        }
        self.after_queue_change(cx);
        if self.queue.is_empty() {
            self.focus_composer(window, cx);
        }
    }

    /// Remove on a queued review card.
    pub(crate) fn remove_queued(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.qsel = ix;
        self.delete_queued(window, cx);
    }

    pub(super) fn delete_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.qsel >= self.queue.len() {
            return;
        }
        if let Some(r) = self.take_queued(self.qsel).review {
            cx.emit(ChatEvent::ReviewRemoved(r.ids()));
        }
        self.after_queue_change(cx);
        if self.queue.is_empty() {
            self.focus_composer(window, cx);
        }
    }

    fn move_queued(&mut self, d: isize, cx: &mut Context<Self>) {
        let to = self.qsel as isize + d;
        if to < 0 || to as usize >= self.queue.len() {
            return;
        }
        self.queue.swap(self.qsel, to as usize);
        self.qsel = to as usize;
        self.after_queue_change(cx);
    }

    /// Loads the selected message into the composer; Enter puts it back in the same slot.
    /// A draft already there is queued first, so nothing typed is lost.
    pub(super) fn edit_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.queue.get(self.qsel).is_none_or(|q| q.review.is_some()) {
            return;
        }
        let mut ix = self.qsel;
        let item = self.take_queued(ix);
        let draft = self.composer.read(cx).content();
        if !draft.text().trim().is_empty() && self.queue_draft(draft) <= ix {
            ix += 1;
        }
        self.editing = Some(ix);
        self.editing_git_reviews = item.git_reviews;
        let end = item.content.text().len();
        self.composer.update(cx, |c, cx| {
            c.set_value(item.content, window, cx);
            c.set_selected_range(end..end, cx);
            c.focus(window, cx);
        });
        self.after_queue_change(cx);
    }

    /// Appends a git prompt without changing the draft or an edit already in progress.
    pub fn enqueue(&mut self, prompt: &str, reviews: Vec<String>, cx: &mut Context<Self>) -> bool {
        if self.archiving {
            return false;
        }
        self.queue.push(Queued {
            git_reviews: reviews,
            ..Queued::new(prompt.to_string().into())
        });
        self.qsel = self.queue.len() - 1;
        self.drain(cx);
        self.after_queue_change(cx);
        true
    }

    /// Queued messages above the input, in send order: the top row goes next.
    pub(super) fn render_queue(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.queue.is_empty() {
            return None;
        }
        let focused = self.qfocus.is_focused(window);
        let theme = cx.theme();
        let faint = crate::ui::faint(theme);
        let action = |id: (&'static str, usize),
                      icon: IconName,
                      tip: &'static str,
                      f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let ix = id.1;
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(tip)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.qsel = ix;
                    f(this, window, cx);
                    cx.stop_propagation(); // the row's own click would refocus the queue
                }))
        };
        let rows: Vec<AnyElement> = (self.queue.iter().enumerate())
            .map(|(ix, q)| {
                let sel = focused && ix == self.qsel;
                if let Some(r) = &q.review {
                    let footer = h_flex()
                        .gap_1()
                        .child("Included with your next message ·")
                        .child(
                            div()
                                .id(("q-remove", ix))
                                .cursor_pointer()
                                .text_color(theme.foreground)
                                .hover(|d| d.underline())
                                .child("Remove")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_queued(ix, window, cx);
                                    cx.stop_propagation();
                                })),
                        );
                    return div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .when(sel, |d| d.bg(theme.list_active))
                        .child(self.review_card(r, ix, Some(footer.into_any_element()), cx))
                        .into_any_element();
                }
                h_flex()
                    .id(("queued", ix))
                    .h(px(28.))
                    .pl_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .border_l_2()
                    .border_color(if sel {
                        theme.primary
                    } else {
                        theme.transparent
                    })
                    .when(sel, |d| d.bg(theme.list_active))
                    .when(!sel, |d| d.hover(|d| d.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.qsel = ix;
                        this.focus_queue(window, cx);
                    }))
                    .child(
                        Icon::new(IconName::CornerDownRight)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(q.preview.clone()),
                    )
                    .child(action(
                        ("q-send", ix),
                        IconName::ArrowUp,
                        "Send now (enter)",
                        Self::steer,
                    ))
                    .child(action(
                        ("q-edit", ix),
                        IconName::Pencil,
                        "Edit (e)",
                        Self::edit_queued,
                    ))
                    .child(action(
                        ("q-del", ix),
                        IconName::Trash,
                        "Delete (d)",
                        Self::delete_queued,
                    ))
                    .into_any_element()
            })
            .collect();
        let n = self.queue.len();
        Some(
            v_flex()
                .id("queue")
                .key_context("Queue")
                .track_focus(&self.qfocus)
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                .text_sm()
                .on_action(cx.listener(|this, _: &SelectNext, w, cx| {
                    this.queue_select(this.qsel + 1, w, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectPrev, w, cx| {
                    this.queue_select(this.qsel.saturating_sub(1), w, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectFirst, w, cx| this.queue_select(0, w, cx)))
                .on_action(cx.listener(|this, _: &SelectLast, w, cx| {
                    this.queue_select(this.queue.len().saturating_sub(1), w, cx)
                }))
                .on_action(cx.listener(|this, _: &Confirm, w, cx| this.steer(w, cx)))
                .on_action(cx.listener(|this, _: &EditQueued, w, cx| this.edit_queued(w, cx)))
                .on_action(cx.listener(|this, _: &DeleteQueued, w, cx| this.delete_queued(w, cx)))
                .on_action(cx.listener(|this, _: &MoveQueuedUp, _, cx| this.move_queued(-1, cx)))
                .on_action(cx.listener(|this, _: &MoveQueuedDown, _, cx| this.move_queued(1, cx)))
                .on_action(cx.listener(|this, _: &FocusComposer, w, cx| this.focus_composer(w, cx)))
                .child(
                    h_flex()
                        .h(px(24.))
                        .px_2()
                        .gap_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(match self.editing.is_some() {
                            true => format!("{n} queued · paused while you edit"),
                            false => format!("{n} queued · sends when the turn ends"),
                        })
                        .child(div().flex_1())
                        .when(focused, |d| {
                            d.text_color(faint)
                                .child(hint("enter", "send now"))
                                .child(hint("e", "edit"))
                                .child(hint("d", "delete"))
                                .child(hint("shift-k shift-j", "reorder"))
                        })
                        .when(!focused, |d| {
                            d.text_color(faint).child(hint("up", "manage"))
                        }),
                )
                .child(
                    div()
                        .id("queue-rows")
                        .max_h(px(5. * 28.))
                        .overflow_y_scroll()
                        .track_scroll(&self.qscroll)
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}
