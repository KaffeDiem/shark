//! ⇧C: the send sheet (A·03). Pending threads as a checklist, an overall note, an optional
//! PR review; what is ticked goes to the chat as one message, the rest stays pending.
use super::review::{Comment, Status, prompt};
use super::*;
use crate::chat::{ReviewCard, ReviewItem};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::{Enter, Escape};

pub(super) struct Sheet {
    /// The pending threads when it opened, ticked or not.
    pub(super) rows: Vec<(u64, bool)>,
    sel: usize,
    pub(super) note: Entity<InputState>,
    /// Also post the ticked comments to the PR as a Comment review.
    pub(super) publish: bool,
    preview: bool,
    pub(super) focus: FocusHandle,
}

impl Shark {
    /// `shift-c`, the bar's Send review…: the sheet over the pending threads.
    pub(super) fn open_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows: Vec<(u64, bool)> = (self.ws_comments().iter())
            .filter(|c| c.status == Status::Pending)
            .map(|c| (c.id, true))
            .collect();
        if rows.is_empty() {
            return notify_err(window, cx, "No pending comments (c on a diff line)");
        }
        let note = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Overall note (optional) — e.g. “Run cargo test after fixing.”")
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.review.sheet = Some(Sheet {
            rows,
            sel: 0,
            note,
            publish: false,
            preview: false,
            focus,
        });
        cx.notify();
    }

    pub(super) fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.review.sheet = None;
        self.diff.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// `space`: tick or untick the row under the cursor.
    pub(super) fn sheet_toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.review.sheet
            && let Some(r) = s.rows.get_mut(s.sel)
        {
            r.1 = !r.1;
            cx.notify();
        }
    }

    pub(super) fn sheet_move(&mut self, d: isize, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.review.sheet {
            s.sel = s.sel.saturating_add_signed(d).min(s.rows.len() - 1);
            cx.notify();
        }
    }

    /// `enter`: the row's thread in the diff.
    pub(super) fn sheet_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = (self.review.sheet.as_ref())
            .and_then(|s| s.rows.get(s.sel))
            .map(|r| r.0);
        self.close_sheet(window, cx);
        if let Some(id) = id {
            self.jump_to_comment(id, window, cx);
        }
    }

    /// `cmd-enter`: sends the ticked rows.
    pub(super) fn sheet_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.review.sheet else { return };
        let ids: Vec<u64> = s.rows.iter().filter(|r| r.1).map(|r| r.0).collect();
        let (note, publish) = (s.note.read(cx).value().to_string(), s.publish);
        self.send_review(&ids, &note, publish, window, cx);
    }

    /// Pending comments `ids` as one message in the open chat (queued while it works); they
    /// become Sent there. With `publish`, also a Comment review on the PR.
    pub(super) fn send_review(
        &mut self,
        ids: &[u64],
        note: &str,
        publish: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = self.active_chat().cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let comments: Vec<Comment> = (self.ws_comments().iter())
            .filter(|c| c.status == Status::Pending && ids.contains(&c.id))
            .cloned()
            .collect();
        if comments.is_empty() {
            return notify_err(window, cx, "Tick a comment to send (space)");
        }
        let card = ReviewCard {
            prompt: prompt(&comments, note),
            items: (comments.iter())
                .map(|c| ReviewItem {
                    id: c.id,
                    place: c.card_place(),
                    text: c.text.clone(),
                })
                .collect(),
            changes: self.changes_label(),
        };
        let chat_id = chat.read(cx).meta.id.clone();
        // Refused (archiving, spawn failed): the chat says why; the comments stay pending.
        if !chat.update(cx, |c, cx| c.send_review(card, cx)) {
            return notify_err(window, cx, "Review not sent; the chat says why");
        }
        // Queued until the chat's ReviewSent (right after this, or when its turn ends).
        let ids: Vec<u64> = comments.iter().map(|c| c.id).collect();
        self.edit_comments(cx, |l| {
            for c in l.iter_mut().filter(|c| ids.contains(&c.id)) {
                c.status = Status::Queued;
                c.chat = Some(chat_id.clone());
            }
        });
        let n = comments.len();
        let s = if n == 1 { "" } else { "s" };
        window.push_notification(format!("Sent {n} comment{s} to the agent"), cx);
        self.close_sheet(window, cx);
        if publish {
            self.publish(&comments, note, true, window, cx);
        }
    }

    /// The sheet, over the bottom right of the diff.
    pub(super) fn render_sheet(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let s = self.review.sheet.as_ref()?;
        let theme = cx.theme();
        let comments = self.ws_comments();
        let files: HashSet<&str> = (s.rows.iter())
            .filter_map(|r| Some(comments.iter().find(|c| c.id == r.0)?.path.as_str()))
            .collect();
        let ticked = s.rows.iter().filter(|r| r.1).count();
        let focused = s.focus.is_focused(window);
        let pr = (s.rows.first())
            .and_then(|r| comments.iter().find(|c| c.id == r.0))
            .and_then(|c| self.pr_of(c, cx))
            .map(|(pr, _)| pr.number);
        let rows = s.rows.iter().enumerate().map(|(ix, &(id, on))| {
            let c = comments.iter().find(|c| c.id == id);
            let name = c.map_or(String::new(), |c| {
                let name = c.path.rsplit('/').next().unwrap_or(&c.path);
                format!("{name}:{}", c.lines().replace('–', "-"))
            });
            let text = c.map_or(String::new(), |c| match &c.suggestion {
                Some(sug) => format!(
                    "Suggested change · {}",
                    sug.lines().next().unwrap_or_default().trim()
                ),
                None => c.text.lines().next().unwrap_or_default().to_string(),
            });
            h_flex()
                .id(("sheet-row", ix))
                .h(px(36.))
                .px_2()
                .gap_3()
                .rounded(theme.radius)
                .when(ix == s.sel && focused, |d| d.bg(theme.list_active))
                .hover(|d| d.bg(theme.list_hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = &mut this.review.sheet {
                        s.sel = ix;
                    }
                    this.sheet_toggle(cx);
                }))
                .child(Checkbox::new(("sheet-tick", ix)).small().checked(on))
                .child(
                    div()
                        .w(px(140.))
                        .flex_none()
                        .truncate()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(name),
                )
                .child(div().flex_1().min_w_0().truncate().child(text))
        });
        let preview = s.preview.then(|| {
            let picked: Vec<Comment> = (s.rows.iter())
                .filter(|r| r.1)
                .filter_map(|r| comments.iter().find(|c| c.id == r.0).cloned())
                .collect();
            prompt(&picked, &s.note.read(cx).value())
        });
        let n = s.rows.len();
        Some(
            v_flex()
                .id("review-sheet")
                .absolute()
                .right(px(16.))
                .bottom(px(48.))
                .w(px(500.))
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .text_sm()
                // In the note: cmd-enter sends and Esc closes, as everywhere in the sheet.
                .capture_action(cx.listener(|this, a: &Enter, w, cx| {
                    if a.secondary {
                        cx.stop_propagation();
                        this.sheet_send(w, cx);
                    }
                }))
                .capture_action(cx.listener(|this, _: &Escape, w, cx| {
                    cx.stop_propagation();
                    this.close_sheet(w, cx);
                }))
                .child(
                    h_flex()
                        .px_4()
                        .pt_3()
                        .pb_2()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Send review"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{n} comment{} · {} file{}",
                                    if n == 1 { "" } else { "s" },
                                    files.len(),
                                    if files.len() == 1 { "" } else { "s" }
                                )),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_xs()
                                .text_color(faint(theme))
                                .child("space toggle · ↵ open"),
                        ),
                )
                // The keys live on the list, so the note below types j, k and space.
                .child(
                    v_flex()
                        .id("sheet-rows")
                        .key_context("ReviewSheet")
                        .track_focus(&s.focus)
                        .px_2()
                        .pb_2()
                        .on_action(cx.listener(|this, _: &ToggleRow, _, cx| this.sheet_toggle(cx)))
                        .on_action(
                            cx.listener(|this, _: &SelectNext, _, cx| this.sheet_move(1, cx)),
                        )
                        .on_action(
                            cx.listener(|this, _: &SelectPrev, _, cx| this.sheet_move(-1, cx)),
                        )
                        .on_action(cx.listener(|this, _: &Confirm, w, cx| this.sheet_open(w, cx)))
                        .on_action(
                            cx.listener(|this, _: &CancelEdit, w, cx| this.close_sheet(w, cx)),
                        )
                        .on_action(
                            cx.listener(|this, _: &SubmitForm, w, cx| this.sheet_send(w, cx)),
                        )
                        .children(rows),
                )
                .child(
                    v_flex()
                        .px_4()
                        .py_3()
                        .gap_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(Input::new(&s.note))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .size(px(10.))
                                        .rounded_full()
                                        .border_3()
                                        .border_color(theme.primary),
                                )
                                .child("Send as the next message in this chat"),
                        )
                        .when_some(pr, |d, number| {
                            d.child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Checkbox::new("sheet-publish")
                                            .small()
                                            .checked(s.publish)
                                            .label(format!(
                                                "Also publish to PR #{number} as a Comment review"
                                            ))
                                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                                if let Some(s) = &mut this.review.sheet {
                                                    s.publish = *on;
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("preview first"),
                                    ),
                            )
                        }),
                )
                .when_some(preview, |d, p| {
                    d.child(
                        div()
                            .id("sheet-preview")
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .whitespace_normal()
                            .child(p),
                    )
                })
                .child(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            Button::new("sheet-preview-toggle")
                                .ghost()
                                .small()
                                .label(if s.preview {
                                    "Hide prompt"
                                } else {
                                    "Preview prompt"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(s) = &mut this.review.sheet {
                                        s.preview = !s.preview;
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("sheet-esc")
                                .ghost()
                                .small()
                                .label("Esc")
                                .on_click(cx.listener(|this, _, w, cx| this.close_sheet(w, cx))),
                        )
                        .child(
                            Button::new("sheet-send")
                                .primary()
                                .small()
                                .disabled(ticked == 0)
                                .label(format!(
                                    "Send {ticked} comment{} ⌘↵",
                                    if ticked == 1 { "" } else { "s" }
                                ))
                                .on_click(cx.listener(|this, _, w, cx| this.sheet_send(w, cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}
