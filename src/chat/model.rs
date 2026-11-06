//! The model and thinking level: labels, the menu, and the picks.
use super::*;
use crate::models::{effort_of, efforts};

impl Chat {
    pub fn model(&self) -> &str {
        self.meta.model()
    }

    pub(crate) fn codex(&self) -> bool {
        self.meta.is_codex()
    }

    /// The process handling live events; model selection configures only its successor.
    pub(crate) fn active_codex(&self) -> bool {
        self.agent.as_ref().map_or_else(
            || codex::is_codex(self.agent_model.as_deref()),
            Agent::is_codex,
        )
    }

    /// The thinking level this chat runs at (see `effort_of`).
    pub fn effort(&self) -> &'static str {
        effort_of(&self.meta)
    }

    /// The levels this chat's model takes, lowest first.
    pub fn efforts(&self) -> &'static [&'static str] {
        efforts(self.model())
    }

    /// The model button's label: "Opus · High".
    pub(crate) fn model_chip(&self) -> String {
        format!(
            "{} · {}",
            model_label(self.model()),
            effort_label(self.effort())
        )
    }

    /// cmd-/ or the model button: the models, then the thinking levels, the current ones
    /// checked; type a letter to jump (again to cycle). The highlight starts on the current
    /// level, the pick changed most. A pick takes effect on the next send.
    pub fn open_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model_chat = cx.entity().downgrade();
        let effort_chat = model_chat.clone();
        let (entries, sel) = model_entries(
            &self.meta,
            move |m, cx| {
                model_chat.update(cx, |c, cx| c.set_model(m, cx)).ok();
            },
            move |l, cx| {
                effort_chat.update(cx, |c, cx| c.set_effort(l, cx)).ok();
            },
        );
        let b = self.model_at.get();
        let at = point(b.left(), b.top() - px(4.));
        let m = crate::menu::open_at(entries, at, Anchor::BottomLeft, window, cx);
        m.update(cx, |m, _| m.select(sel));
        crate::menu::hold(self, m, |c| &mut c.model_menu, cx);
    }

    /// The user's pick: the process restarts on it at the next send. A thinking level the new
    /// model lacks becomes its highest.
    pub fn set_model(&mut self, m: &str, cx: &mut Context<Self>) {
        let model = Some(m.to_string());
        if self.meta.model != model {
            let codex = codex::is_codex(model.as_deref());
            // A session belongs to one CLI: the other can't resume it.
            if codex != self.meta.is_codex() && self.meta.session_id.is_some() {
                self.meta.session_id = None;
                self.push(Block::Info(format!("{m} starts a new conversation")), cx);
            }
            self.meta.model = model;
            if self.meta.effort.is_some() {
                self.meta.effort = Some(self.effort().into());
            }
            cx.emit(ChatEvent::Meta);
            cx.notify();
        }
        cx.emit(ChatEvent::Picked);
    }

    /// The user's pick of thinking level (one the model lacks becomes its highest). Claude
    /// restarts on it at the next send, like a model change; codex sends it with the next
    /// message.
    pub fn set_effort(&mut self, level: &str, cx: &mut Context<Self>) {
        let was = self.meta.effort.replace(level.into());
        let level = self.effort();
        self.meta.effort = Some(level.into());
        if let Some(a) = &mut self.agent {
            a.set_effort(level);
        }
        if was != self.meta.effort {
            cx.emit(ChatEvent::Meta);
            cx.notify();
        }
        cx.emit(ChatEvent::Picked);
    }

    /// One thinking level up (`d` > 0) or down, stopping at the ends; the level it's on now.
    pub fn step_effort(&mut self, d: isize, cx: &mut Context<Self>) -> &'static str {
        let levels = self.efforts();
        let ix = levels.iter().position(|l| *l == self.effort()).unwrap_or(0);
        let ix = ix.saturating_add_signed(d).min(levels.len() - 1);
        self.set_effort(levels[ix], cx);
        levels[ix]
    }

    /// codex's Fast service tier: its own default until toggled here.
    pub fn fast(&self) -> bool {
        self.meta.fast.unwrap_or(self.fast_by_config)
    }

    /// Fast on or off for a codex chat (callers check), from its next message (a running turn
    /// keeps its tier).
    pub fn set_fast(&mut self, on: bool, cx: &mut Context<Self>) {
        self.meta.fast = Some(on);
        if let Some(a) = &mut self.agent {
            a.set_fast(on);
        }
        cx.emit(ChatEvent::Meta);
        cx.notify();
    }
}
