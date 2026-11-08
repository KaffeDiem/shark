//! Own the picker lifetime and commit image files only with their agent state.
use super::*;
use crate::state::AgentIcon;

pub(super) struct Panel {
    pub picker: Entity<icon_picker::IconPicker>,
    anchor: Point<Pixels>,
    previous_focus: Option<FocusHandle>,
    _events: Subscription,
}

impl Shark {
    pub(super) fn pick_agent_icon(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.agent(slug) else {
            return;
        };
        let current = agent.icon.clone();
        let slug = slug.to_string();
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
            .get(&slug)
            .map(|b| point(b.left(), b.bottom() + px(4.)))
            .unwrap_or_else(|| window.mouse_position());
        let picker = cx.new(|cx| icon_picker::IconPicker::new(current, slug.clone(), window, cx));
        let events = cx.subscribe_in(&picker, window, move |s, _, event, w, cx| match event {
            icon_picker::Event::Apply(icon) => {
                if s.apply_agent_icon(&slug, icon.clone(), w, cx) {
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
        slug: &str,
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

    pub(super) fn publish_agent_identity(&mut self, slug: &str, cx: &mut Context<Self>) {
        let tasks: Vec<_> = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                task.agent
                    .as_ref()
                    .and_then(|a| self.agent(&a.slug))
                    .is_some_and(|a| a.next.as_deref() == Some(slug))
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
