//! User recency is independent of agent output, probes and window activation.
use super::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Shark {
    pub(super) fn record_interaction(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.archiving.contains(name) {
            return;
        }
        let Some(ws) = self.state.workspaces.iter_mut().find(|ws| ws.name == name) else {
            return;
        };
        ws.last_interacted_at = Some(now_ms());
        // One trailing save per second, even under continuous typing or scrolling.
        if self.interaction_save.is_none() {
            self.interaction_save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                this.update(cx, |s, cx| {
                    s.flush_interaction();
                    cx.notify();
                })
                .ok();
            }));
        }
    }

    pub(super) fn flush_interaction(&mut self) {
        if self.interaction_save.take().is_some() {
            let _ = self.state.save();
        }
    }

    fn content_interaction(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        if !self.home.on
            && !w.has_active_dialog(cx)
            && !w
                .context_stack()
                .iter()
                .any(|context| context.contains("SharkMenu"))
            && self.menu.is_none()
            && self.app_switcher.is_none()
            && let Some(name) = self.active.clone()
        {
            self.record_interaction(&name, cx);
        }
    }

    pub(super) fn interaction_key(
        &mut self,
        key: &Keystroke,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Opening/browsing navigation surfaces isn't an interaction with the old workspace.
        let navigation = key.modifiers.platform
            && matches!(
                key.key.as_str(),
                "o" | "k"
                    | "p"
                    | "/"
                    | ","
                    | "0"
                    | "1"
                    | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
                    | "j"
                    | "n"
            );
        if !navigation
            && !matches!(key.key.as_str(), "platform" | "shift" | "control" | "alt")
            && self.content_focus.contains_focused(w, cx)
        {
            self.content_interaction(w, cx);
        }
    }

    pub(super) fn interaction_content(
        &self,
        content: AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let owner = cx.entity().downgrade();
        div()
            .id("workspace-content")
            .size_full()
            .min_w_0()
            .track_focus(&self.content_focus)
            .capture_any_mouse_down(cx.listener(|s, _, w, cx| s.content_interaction(w, cx)))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, w, _| {
                        let owner = owner.clone();
                        // Capture before nested scrolling widgets consume the wheel event.
                        w.on_mouse_event(move |ev: &ScrollWheelEvent, phase, w, cx| {
                            if phase == DispatchPhase::Capture && bounds.contains(&ev.position) {
                                owner.update(cx, |s, cx| s.content_interaction(w, cx)).ok();
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(content)
    }
}
