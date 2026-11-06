//! The side-question card: a forked, tool-less claude answers above the composer.
use super::*;
use gpui_kit::component::Theme;

/// The card above the composer that holds a side question or the agent's questions; its border
/// shows the focus.
pub(super) fn aside_card(theme: &Theme, focused: bool) -> Div {
    v_flex()
        .w_full()
        .max_w(px(712.))
        .mx_auto()
        .mb_2()
        .p_3()
        .gap_2()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(if focused { theme.primary } else { theme.border })
        .bg(theme.muted)
}

/// A side question and its answer, in a card above the composer. Neither the question nor the
/// answer reaches the chat or its transcript.
pub(super) struct Side {
    /// The card's own question field when opened empty; `None` once asked.
    input: Option<Entity<TextareaState>>,
    pub(super) question: String,
    answer: Entity<TextViewState>,
    /// The answer as markdown, for copy and insert.
    pub(super) text: String,
    pub(super) done: bool,
    pub(super) error: Option<String>,
    /// claude echoed the question. A fork first runs turns of its own for task notifications
    /// left in the session; their output and result come before the echo and are not the answer.
    asked: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// Dropping the card stops the process and its reader.
    pub(super) agent: Option<Agent>,
    _reader: Option<Task<()>>,
    _sub: Option<Subscription>,
}

impl Side {
    fn new(question: String, cx: &mut App) -> Self {
        Side {
            input: None,
            question,
            answer: cx.new(|cx| TextViewState::markdown("", cx)),
            text: String::new(),
            done: false,
            error: None,
            asked: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            agent: None,
            _reader: None,
            _sub: None,
        }
    }
}

impl Chat {
    /// cmd-shift-enter: asks the draft as a side question, or opens the card's own field when
    /// the composer is empty.
    pub(super) fn ask_draft_aside(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return self.open_side(window, cx);
        }
        // The draft stays if the question didn't go out.
        if self.ask_side(text, window, cx) {
            self.composer
                .update(cx, |c, cx| c.set_value("", window, cx));
        }
    }

    /// Opens the side-question card with an empty field, keeping any draft in the composer.
    pub fn open_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .placeholder("Ask a side question (the chat won't see it)")
                .submit_on_enter(true)
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, input, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    let q = input.read(cx).value().trim().to_string();
                    if !q.is_empty() {
                        this.ask_side(q, window, cx);
                    }
                }
            },
        );
        input.update(cx, |i, cx| i.focus(window, cx));
        let mut side = Side::new(String::new(), cx);
        (side.input, side._sub) = (Some(input), Some(sub));
        self.side = Some(side);
        cx.notify();
    }

    /// Replaces any earlier side question; only the latest is kept. Returns whether it started;
    /// otherwise the card shows why.
    fn ask_side(&mut self, question: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut side = Side::new(question.clone(), cx);
        match agent::spawn_side(&self.ws, &self.meta) {
            Ok((mut a, rx)) => match a.send(&question) {
                Ok(()) => {
                    side.agent = Some(a);
                    side._reader = Some(read_events(rx, Self::apply_side, cx));
                }
                Err(e) => (side.done, side.error) = (true, Some(format!("send failed: {e}"))),
            },
            Err(e) => (side.done, side.error) = (true, Some(format!("no side question: {e}"))),
        }
        let started = !side.done;
        side.focus.focus(window, cx);
        self.side = Some(side);
        cx.notify();
        started
    }

    pub(super) fn apply_side(&mut self, ev: Ev, cx: &mut Context<Self>) {
        let Some(side) = &mut self.side else { return };
        match ev {
            Ev::User(_) => side.asked = true,
            // An error before the echo (a missing session) is about the question too; keep it.
            Ev::TextDelta(_)
            | Ev::Text(_)
            | Ev::Result {
                is_error: false, ..
            } if !side.asked => {
                return;
            }
            Ev::TextDelta(d) => {
                side.text.push_str(&d);
                side.answer.update(cx, |m, cx| m.push_str(&d, cx));
            }
            Ev::Text(full) => {
                side.answer.update(cx, |m, cx| m.set_text(&full, cx));
                side.text = full;
            }
            Ev::Result { is_error, text, .. } => {
                side.done = true;
                side.agent = None; // stays alive on stdin otherwise
                if is_error {
                    side.error = Some(text);
                }
            }
            Ev::Exited(msg) if !side.done => (side.done, side.error) = (true, Some(msg)),
            _ => return,
        }
        side.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn close_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.side.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    fn copy_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(side) = &self.side
            && !side.text.is_empty()
        {
            crate::ui::copy_with(side.text.clone(), "Copied the answer", window, cx);
        }
    }

    /// Puts the answer into the composer after any draft, to send or edit. Waits for the answer:
    /// taking the card early would drop the question.
    fn insert_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.side.as_ref().is_some_and(|s| s.done) {
            return;
        }
        let Some(side) = self.side.take() else { return };
        if !side.text.is_empty() {
            self.composer.update(cx, |c, cx| {
                let (draft, end) = (c.value().trim_end().len(), c.value().len());
                let sep = if draft == 0 { "" } else { "\n\n" };
                c.set_selected_range(draft..end, cx);
                c.replace(format!("{sep}{}", side.text), window, cx);
            });
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// The side-question card, just above the composer so the transcript stays in view.
    pub(super) fn render_side(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let side = self.side.as_ref()?;
        let theme = cx.theme();
        let focused = side.focus.is_focused(window)
            || (side.input.as_ref()).is_some_and(|i| i.focus_handle(cx).is_focused(window));
        let header = h_flex()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(Icon::new(IconName::MessageCircleQuestionMark).with_size(px(14.)))
            .child("Side question · not added to chat")
            .child(div().flex_1())
            .when(!side.done && side.input.is_none(), |d| {
                d.child(Spinner::new().with_size(px(12.)))
            })
            // Laid out from the start so the spinner doesn't move when the answer lands.
            .when(side.input.is_none(), |d| {
                d.child(
                    div().when(side.text.is_empty(), |d| d.invisible()).child(
                        Clipboard::new("copy-side")
                            .value(side.text.clone())
                            .tooltip("Copy answer (c)"),
                    ),
                )
            })
            .child(
                Button::new("close-side")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Dismiss (esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_side(window, cx))),
            );
        let body = match &side.input {
            Some(input) => div()
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    this.close_side(window, cx);
                    cx.stop_propagation();
                }))
                .child(Textarea::new(input).appearance(false).text_size(px(14.)))
                .into_any_element(),
            None => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(side.question.clone()),
                )
                .child(
                    div()
                        .id("side-answer")
                        .group("side-answer")
                        .key_context("SideAnswer")
                        .track_focus(&side.focus)
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .track_scroll(&side.scroll)
                        .text_size(px(14.))
                        .line_height(relative(1.5))
                        .on_action(cx.listener(|this, _: &CloseSide, window, cx| {
                            this.close_side(window, cx)
                        }))
                        .on_action(
                            cx.listener(|this, _: &CopySide, window, cx| {
                                this.copy_side(window, cx)
                            }),
                        )
                        .on_action(cx.listener(|this, _: &InsertSide, window, cx| {
                            this.insert_side(window, cx)
                        }))
                        .child(markdown(&side.answer, "side-answer", link_roots(&self.ws)))
                        .when_some(side.error.clone(), |d, e| {
                            d.child(div().text_xs().text_color(theme.danger_foreground).child(e))
                        }),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(hint("escape", "dismiss"))
                        .child(hint("c", "copy"))
                        .child(hint("enter", "insert into message")),
                )
                .into_any_element(),
        };
        Some(
            aside_card(theme, focused)
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }
}
