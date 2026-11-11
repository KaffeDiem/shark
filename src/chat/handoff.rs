//! The chat's agent: its context notes, its button in the composer, and the handoff bar.
use super::*;
use crate::state::{AGENT_CONTEXT, ContextNote};
use std::collections::BTreeMap;

pub(crate) struct HandoffTarget {
    pub slug: String,
    pub name: String,
    pub icon: crate::state::AgentIcon,
}

impl Chat {
    /// `Task::context`, with only this chat's agent's notes.
    pub(crate) fn context(&self) -> BTreeMap<String, ContextNote> {
        let mut notes = self.ws.context();
        notes.retain(|key, _| {
            (key.strip_prefix(AGENT_CONTEXT))
                .is_none_or(|slug| Some(slug) == self.meta.agent.as_deref())
        });
        notes
    }

    /// The agent is still the chat's to pick: nothing sent or queued, no session. After that,
    /// another agent means a handoff to a new chat.
    pub fn agent_open(&self) -> bool {
        self.turns() == 0 && self.meta.session_id.is_none() && !self.busy && self.queue.is_empty()
    }

    /// Puts agent `slug` on a chat that hasn't started, with the model and thinking level it
    /// starts on (`State::defaults_for`); `None` for both keeps the chat's. Whether it changed.
    pub fn set_agent(
        &mut self,
        slug: Option<String>,
        defaults: Option<(String, Option<String>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.agent_open() {
            return false;
        }
        let was = self.meta.clone();
        self.meta.agent = slug;
        if let Some((model, effort)) = defaults {
            self.meta.model = Some(model);
            self.meta.effort = effort;
        }
        if self.meta == was {
            return false;
        }
        self.handoff_dismissed = false;
        cx.emit(ChatEvent::Meta);
        cx.notify();
        true
    }

    /// The agent button, first in the composer's row: the chat's agent, or Agent.
    pub(super) fn render_agent_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        // A deleted agent's chats go on without it.
        let link = (self.meta.agent.as_deref()).and_then(|s| self.ws.agent_link(s));
        let slug = link.map(|a| a.slug.as_str());
        let name = link.map_or_else(|| "Agent".to_string(), |a| a.name.clone());
        let tip = match (slug, self.agent_open()) {
            (Some(_), true) => format!("{name}: this chat's instructions, skills and defaults"),
            (Some(_), false) => format!("{name} · hand off to another agent in a new chat"),
            (None, true) => "Pick an agent for this chat".into(),
            (None, false) => "Hand off to an agent in a new chat".into(),
        };
        let at = self.agent_at.clone();
        let button = Button::new("chat-agent")
            .ghost()
            .xsmall()
            .dropdown_caret(true)
            .flex_shrink(1.)
            .min_w_0()
            .accessibility_label(match slug {
                Some(_) => format!("Agent: {name}"),
                None => "Agent: none".into(),
            })
            .tooltip(tip)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::AgentMenu)));
        let button = match link {
            Some(link) => button.child(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .child(crate::app::agent_icon_view::agent_icon(&link.icon).size(px(14.)))
                    .child(div().min_w_0().truncate().child(name)),
            ),
            None => button.icon(IconName::AtSign).label(name),
        };
        div()
            .relative()
            .min_w_0()
            .flex_shrink(1.)
            .child(crate::ui::toolbar("chat-agent", button))
            .child(
                canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .into_any_element()
    }

    pub(super) fn show_handoff(&self) -> bool {
        self.next_agent.is_some()
            && self.idle()
            && self.turns() > 0
            && !self.replaying
            && !self.handoff_dismissed
    }

    pub(super) fn render_handoff(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_handoff() {
            return None;
        }
        let target = self.next_agent.as_ref()?;
        let source = (self.meta.agent.as_deref())
            .and_then(|slug| self.ws.agent_link(slug))
            .map_or("Agent", |a| a.name.as_str());
        Some(
            side::aside_card(cx.theme(), false)
                .id("handoff-bar")
                .test_support()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(div().flex_1().child(format!("{source} is done?")))
                        .child(
                            Button::new("handoff-go")
                                .small()
                                .ghost()
                                .accessibility_label(format!("Hand off to {}", target.name))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .child(crate::app::agent_icon_view::agent_icon(
                                            &target.icon,
                                        ))
                                        .child(format!("Hand off to {} ⌘⌥↵", target.name)),
                                )
                                .tooltip("Hand off · ⌘⌥↵")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.emit(ChatEvent::HandOff {
                                        to: this.next_agent.as_ref().map(|a| a.slug.clone()),
                                    });
                                })),
                        )
                        .child(
                            Button::new("handoff-other")
                                .small()
                                .ghost()
                                .label("Other…")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(ChatEvent::HandOff { to: None })
                                })),
                        )
                        .child(
                            crate::ui::close_button("handoff-dismiss", "Dismiss").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.handoff_dismissed = true;
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}
