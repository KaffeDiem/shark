//! Ask Shark: a question to claude about Shark, answered below Home's prompt.
use super::*;
use crate::agent::{self, Agent, Ev};
use gpui_kit::component::text::TextViewState;

const ASK_PROMPT: &str = "You answer questions about Shark, a macOS app for running Claude Code \
    on features that span several repos. Answer briefly, from the README and the shortcut list \
    below only; name the key or the cmd-k command to use. If they don't cover it, say so.";

/// claude's error, or what to do when it's the login one (it says "/login", a chat command).
fn ask_error(text: String) -> String {
    let t = text.to_lowercase();
    match t.contains("/login") || t.contains("not logged in") {
        true => "claude isn't logged in: run `claude` in a terminal and log in".into(),
        false if text.trim().is_empty() => "claude stopped without an answer".into(),
        false => text,
    }
}

/// One Ask Shark question and its streamed answer; dropping it stops claude.
pub(super) struct Ask {
    pub(super) question: String,
    pub(super) answer: Entity<TextViewState>,
    pub(super) done: bool,
    pub(super) error: Option<String>,
    /// Before `agent`: dropped first, so the kill's exit isn't read.
    _reader: Option<Task<()>>,
    pub(super) agent: Option<Agent>,
}

impl Ask {
    /// Stops reading before the kill, so its exit isn't read.
    fn stop(&mut self) {
        self.done = true;
        self._reader = None;
        self.agent = None;
    }
}

impl Shark {
    /// One question at a time: a new one replaces (and stops) the last.
    pub(super) fn ask(&mut self, question: String, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = (palette::keymap_summary(window).into_iter())
            .map(|(label, key)| format!("- {key}: {label}"))
            .collect();
        let system = format!(
            "{ASK_PROMPT}\n\n# Shortcuts\n{}\n\n# README\n{}",
            keys.join("\n"),
            agent::README
        );
        let mut ask = Ask {
            question: question.clone(),
            answer: cx.new(|cx| TextViewState::markdown("", cx)),
            done: false,
            error: None,
            agent: None,
            _reader: None,
        };
        // The setup rows for a missing claude show right below.
        let failed = |e| {
            format!(
                "Couldn't start {}.",
                crate::proc::spawn_err(Path::new("/"), "claude", e)
            )
        };
        let started = agent::spawn_ask("haiku", &system)
            .and_then(|(mut a, rx)| a.send(&question).map(|()| (a, rx)));
        match started {
            Ok((a, rx)) => {
                ask.agent = Some(a);
                ask._reader = Some(crate::chat::read_events(rx, Self::apply_ask, cx));
            }
            Err(e) => (ask.done, ask.error) = (true, Some(failed(e))),
        }
        self.home.ask = Some(ask);
        cx.notify();
    }

    pub(super) fn apply_ask(&mut self, ev: Ev, cx: &mut Context<Self>) {
        let Some(ask) = &mut self.home.ask else {
            return;
        };
        match ev {
            Ev::TextDelta(d) => ask.answer.update(cx, |m, cx| m.push_str(&d, cx)),
            Ev::Text(full) => ask.answer.update(cx, |m, cx| m.set_text(&full, cx)),
            Ev::Result { is_error, text, .. } => {
                // Kill it (it waits on stdin otherwise).
                ask.stop();
                if is_error {
                    // claude also streams the error as its answer: show it once.
                    ask.answer.update(cx, |m, cx| m.set_text("", cx));
                    ask.error = Some(ask_error(text));
                }
            }
            Ev::Exited(msg) if !ask.done => (ask.done, ask.error) = (true, Some(ask_error(msg))),
            _ => return,
        }
        cx.notify();
    }

    /// Stops an answer still being written; false when there is none.
    pub(super) fn cancel_ask(&mut self, cx: &mut Context<Self>) -> bool {
        match &mut self.home.ask {
            Some(ask) if !ask.done => {
                ask.stop();
                ask.error = Some("Stopped".into());
                cx.notify();
                true
            }
            _ => false,
        }
    }

    pub(super) fn render_ask(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.home.ask.as_ref()?;
        let theme = cx.theme();
        Some(
            v_flex()
                .p_3()
                .gap_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(IconName::MessageCircleQuestionMark).with_size(px(14.)))
                        .child("Ask Shark")
                        .child(div().flex_1())
                        .when(!ask.done, |d| d.child(Spinner::new().with_size(px(12.))))
                        .child(close_button("close-ask", "Dismiss").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.home.ask = None;
                                cx.notify()
                            },
                        ))),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(ask.question.clone()),
                )
                .child(
                    div()
                        .group("home-answer")
                        .text_size(px(14.))
                        .line_height(relative(1.5))
                        .child(crate::chat::markdown(&ask.answer, "home-answer", vec![]))
                        .when_some(ask.error.clone(), |d, e| {
                            d.child(div().text_xs().text_color(theme.danger_foreground).child(e))
                        }),
                )
                .when(!ask.done, |d| {
                    d.child(
                        h_flex()
                            .gap_3()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(crate::ui::hint("escape", "stop")),
                    )
                })
                .into_any_element(),
        )
    }
}
