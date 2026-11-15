//! Own the picker lifetime and commit image files only with their agent state.
use super::*;
use crate::state::AgentIcon;

pub(super) struct Panel {
    pub picker: Entity<icon_picker::IconPicker>,
    anchor: Point<Pixels>,
    previous_focus: Option<FocusHandle>,
    _events: Subscription,
}

/// Names a new agent's image files until it has a slug. Nothing matches image files by prefix.
const DRAFT_PREFIX: &str = "new-agent";

impl Shark {
    /// The icon picker for agent `slug`, or with `None` for the open dialog's new agent, which
    /// holds the pick until Create.
    pub(super) fn pick_agent_icon(
        &mut self,
        slug: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = self.agent_editor.as_ref().and_then(|e| e.upgrade());
        let current = match slug {
            Some(slug) => match self.agent(slug) {
                Some(agent) => agent.icon.clone(),
                None => return,
            },
            None => match &editor {
                Some(editor) if editor.read(cx).slug.is_none() => {
                    editor.read(cx).pending.icon.clone()
                }
                _ => return,
            },
        };
        let slug = slug.map(str::to_string);
        let previous_focus = self
            .icon_picker
            .as_ref()
            .map(|panel| panel.previous_focus.clone())
            .unwrap_or_else(|| window.focused(cx));
        if let Some(panel) = self.icon_picker.take() {
            panel.picker.update(cx, |p, cx| p.cancel(cx));
        }
        let anchor = self
            .icon_anchors
            .borrow()
            .get(slug.as_deref().unwrap_or_default())
            .map(|b| point(b.left(), b.bottom() + px(4.)))
            .unwrap_or_else(|| window.mouse_position());
        let prefix = slug.clone().unwrap_or_else(|| DRAFT_PREFIX.into());
        let picker = cx.new(|cx| icon_picker::IconPicker::new(current, prefix, window, cx));
        let editor = editor.map(|e| e.downgrade());
        let events = cx.subscribe_in(&picker, window, move |s, _, event, w, cx| match event {
            icon_picker::Event::Apply(icon) => {
                let applied = match &slug {
                    Some(slug) => s.apply_agent_icon(slug, icon.clone(), w, cx),
                    None => {
                        let editor = editor.as_ref().and_then(|e| e.upgrade());
                        if let Some(editor) = editor {
                            let old = editor.update(cx, |e, cx| e.hold_icon(icon.clone(), cx));
                            if old != *icon {
                                s.drop_unused_icon(&old, w, cx);
                            }
                        } else {
                            s.drop_unused_icon(icon, w, cx);
                        }
                        true
                    }
                };
                if applied {
                    s.close_icon_picker(w, cx);
                }
            }
            icon_picker::Event::Dismiss => s.close_icon_picker(w, cx),
        });
        self.icon_picker = Some(Panel {
            picker: picker.clone(),
            anchor,
            previous_focus,
            _events: events,
        });
        picker.update(cx, |p, cx| p.focus(window, cx));
        cx.notify();
    }

    pub(super) fn paste_agent_icon(
        &mut self,
        slug: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_agent_icon(slug, window, cx);
        if let Some(panel) = &self.icon_picker {
            panel.picker.clone().update(cx, |p, cx| p.paste(window, cx));
        }
    }

    pub(super) fn apply_agent_icon(
        &mut self,
        slug: &str,
        icon: AgentIcon,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self.state.agents.iter().position(|a| a.slug == slug) else {
            return false;
        };
        let old = std::mem::replace(&mut self.state.agents[index].icon, icon.clone());
        if let Err(error) = self.state.save() {
            self.state.agents[index].icon = old;
            let mut message = format!("Couldn't save agent icon: {error}");
            if !self.state.agents.iter().any(|a| a.icon == icon)
                && let Err(cleanup) = crate::agent_icon::remove(&icon)
            {
                message.push_str(&format!("; couldn't remove the unused image: {cleanup}"));
            }
            notify_err(window, cx, message);
            return false;
        }
        if old != icon && !self.state.agents.iter().any(|a| a.icon == old) {
            if let Err(error) = crate::agent_icon::remove(&old) {
                notify_err(
                    window,
                    cx,
                    format!("Icon saved, but couldn't remove the previous image: {error}"),
                );
            }
        }
        self.publish_agent_identity(slug, cx);
        cx.notify();
        true
    }

    /// Removes `icon`'s image unless an agent uses it: a new agent's replaced or discarded one.
    pub(super) fn drop_unused_icon(
        &mut self,
        icon: &AgentIcon,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.agents.iter().any(|a| &a.icon == icon)
            && let Err(error) = crate::agent_icon::remove(icon)
        {
            notify_err(
                window,
                cx,
                format!("Couldn't remove an unused agent image: {error}"),
            );
        }
    }

    pub(super) fn publish_agent_identity(&mut self, slug: &str, cx: &mut Context<Self>) {
        // Its chats' buttons and tabs read the snapshot; handoff bars name it as the next agent.
        let _ = self.sync_agent(slug, false, cx);
        let tasks: Vec<_> = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                (task.agents.iter())
                    .filter_map(|a| self.agent(&a.slug))
                    .any(|a| a.next.as_deref() == Some(slug))
            })
            .map(|task| task.name.clone())
            .collect();
        for task in tasks {
            self.publish_task(&task, cx);
        }
    }

    fn close_icon_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.icon_picker.take() {
            panel.picker.update(cx, |p, cx| p.cancel(cx));
            self.restore_focus(panel.previous_focus, window, cx);
            cx.notify();
        }
    }

    pub(super) fn render_icon_picker(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.icon_picker.as_ref()?;
        let viewport = window.viewport_size();
        let left = panel
            .anchor
            .x
            .max(px(8.))
            .min((viewport.width - px(328.)).max(px(8.)));
        let top = panel
            .anchor
            .y
            .max(px(8.))
            .min((viewport.height - px(368.)).max(px(8.)));
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("agent-icon-overlay")
                        .occlude()
                        .w(viewport.width)
                        .h(viewport.height)
                        .on_any_mouse_down(cx.listener(|s, _, w, cx| {
                            cx.stop_propagation();
                            s.close_icon_picker(w, cx);
                        }))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .absolute()
                                .left(left)
                                .top(top)
                                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                .child(panel.picker.clone()),
                        ),
                ),
            )
            .with_priority(101)
            .into_any_element(),
        )
    }
}
