//! Per-chat handoff context and the cards above the composer.
use super::*;
use crate::state::{AGENT_CONTEXT, ContextNote, SIBLINGS_CONTEXT};
use std::collections::BTreeMap;

pub(crate) struct HandoffTarget {
    pub slug: String,
    pub name: String,
    pub icon: crate::state::AgentIcon,
}

impl Chat {
    pub(crate) fn context(&self) -> BTreeMap<String, ContextNote> {
        let mut notes = self.ws.context();
        notes.retain(|key, _| {
            key.strip_prefix(AGENT_CONTEXT)
                .or_else(|| key.strip_prefix(SIBLINGS_CONTEXT))
                .is_none_or(|slug| Some(slug) == self.meta.agent.as_deref())
        });
        if let Some(current) = &self.ws.agent
            && self.meta.agent.as_deref() != Some(current.slug.as_str())
        {
            let text = format!(
                "[shark] This task was handed to {}. The root CLAUDE.md and AGENTS.md now describe {}; keep working as before in this chat.",
                current.name, current.name
            );
            notes.insert(
                format!("stage:{}", current.slug),
                ContextNote {
                    fingerprint: crate::files::fingerprint(&text),
                    text,
                },
            );
        }
        if let Some(handoff) = &self.meta.handoff {
            let text = format!(
                "[shark] {} handed this task to you. This is the result of their chat \"{}\"; you have none of its other context. The files are in the task folder.\n\n{}",
                handoff.from, handoff.title, handoff.text
            );
            notes.insert(
                format!("handoff:{}", self.meta.id),
                ContextNote {
                    fingerprint: crate::files::fingerprint(&text),
                    text,
                },
            );
        }
        notes
    }

    pub(super) fn pending_handoff(&self) -> bool {
        self.meta.handoff.is_some()
            && !self
                .meta
                .delivered
                .contains_key(&format!("handoff:{}", self.meta.id))
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
        let source = self.ws.agent.as_ref().map_or("Agent", |a| a.name.as_str());
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

    pub(super) fn render_handoff_quote(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.pending_handoff() {
            return None;
        }
        let quote = self.meta.handoff.as_ref()?;
        let text = if self.handoff_expanded {
            quote.text.clone()
        } else {
            quote.text.lines().take(3).collect::<Vec<_>>().join("\n")
        };
        Some(
            side::aside_card(cx.theme(), false)
                .id("handoff-quote")
                .test_support()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .flex_1()
                                .child(format!("From {} · \"{}\"", quote.from, quote.title)),
                        )
                        .child(
                            Button::new("handoff-expand")
                                .ghost()
                                .xsmall()
                                .icon(if self.handoff_expanded {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .tooltip("Expand or collapse quote")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.handoff_expanded = !this.handoff_expanded;
                                    cx.notify();
                                })),
                        )
                        .child(
                            crate::ui::close_button("handoff-remove", "Remove quote").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.meta.handoff = None;
                                    this.composer.update(cx, |c, cx| {
                                        c.set_placeholder(
                                            "Message the agent (@ to add files, / for skills)",
                                            window,
                                            cx,
                                        )
                                    });
                                    cx.emit(ChatEvent::Meta);
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .id("handoff-text")
                        .max_h(px(if self.handoff_expanded { 400. } else { 60. }))
                        .overflow_y_scroll()
                        .text_sm()
                        .child(text),
                )
                .into_any_element(),
        )
    }
}
