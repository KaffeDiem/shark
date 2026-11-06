//! The model and thinking level: labels, the menu, and the picks.
use super::*;
use crate::models::{MODELS, effort_of, efforts};

/// A thinking level as people read it: "xhigh" → "Extra high".
pub(crate) fn effort_label(level: &str) -> String {
    match level {
        "xhigh" => "Extra high".into(),
        l => model_label(l),
    }
}

/// Shared menu contents: production actions and preview fixtures use the same model/effort rules.
pub(crate) fn model_entries(
    meta: &ChatMeta,
    choose_model: impl Fn(&'static str, &mut App) + Clone + 'static,
    choose_effort: impl Fn(&'static str, &mut App) + Clone + 'static,
) -> (Vec<crate::menu::Entry>, usize) {
    let current = meta.model();
    // Claude's models, then Codex's: switching between the two starts a new conversation.
    let mut entries = vec![];
    for (i, &m) in MODELS.iter().enumerate() {
        let is_codex = codex::is_codex(Some(m));
        if is_codex && !entries.iter().any(|e| matches!(e, crate::menu::Entry::Sep)) {
            entries.push(crate::menu::Entry::Sep);
        }
        let choose = choose_model.clone();
        let item = crate::menu::item(None, model_label(m), move |_, cx| {
            choose(m, cx);
        });
        // The backend where the name doesn't say it, and the ctrl-cmd-N pick.
        let keys = format!("⌃⌘{}", i + 1);
        let hint = if is_codex {
            format!("Codex  {keys}")
        } else {
            keys
        };
        entries.push(item.hint(hint).checked(m == current));
    }
    entries.push(crate::menu::Entry::Sep);
    let effort = effort_of(meta);
    let mut sel = 0;
    for &l in efforts(current) {
        if l == effort {
            sel = entries.len();
        }
        let choose = choose_effort.clone();
        let item = crate::menu::item(None, effort_label(l), move |_, cx| {
            choose(l, cx);
        });
        let hint = if matches!(l, "max" | "ultra") {
            "slower"
        } else {
            ""
        };
        entries.push(item.hint(hint).checked(l == effort));
    }
    (entries, sel)
}

/// A model id as people read it: "opus" → "Opus", "codex:gpt-6.1-sol" → "GPT-6.1 Sol".
pub(crate) fn model_label(id: &str) -> String {
    let cap = |w: &str| {
        let mut w = w.to_string();
        if let Some(c) = w.get_mut(..1) {
            c.make_ascii_uppercase();
        }
        w
    };
    match codex::model_id(Some(id)) {
        Some(m) => {
            let mut words = m.split('-');
            let head = match (words.next(), words.next()) {
                (Some("gpt"), Some(v)) => format!("GPT-{v}"),
                (a, b) => [a, b]
                    .into_iter()
                    .flatten()
                    .map(cap)
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            std::iter::once(head)
                .chain(words.map(cap))
                .collect::<Vec<_>>()
                .join(" ")
        }
        None => cap(id),
    }
}

impl Chat {
    pub fn model(&self) -> &str {
        self.meta.model()
    }

    pub(crate) fn codex(&self) -> bool {
        self.meta.is_codex()
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin

    #[test]
    fn model_menu_uses_supported_efforts_and_selects_the_effective_level() {
        use crate::menu::Entry;
        for (model, saved, selected, ultra) in [
            ("sonnet", "xhigh", "Extra high", false),
            ("sonnet", "ultra", "Max", false),
            ("codex:gpt-6.1-sol", "ultra", "Ultra", true),
        ] {
            let meta = ChatMeta {
                model: Some(model.into()),
                effort: Some(saved.into()),
                ..Default::default()
            };
            let (entries, sel) = model_entries(&meta, |_, _| {}, |_, _| {});
            assert!(
                matches!(&entries[sel], Entry::Item { label, checked: true, .. } if label == selected)
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|e| matches!(e, Entry::Item { checked: true, .. }))
                    .count(),
                2
            );
            assert!(
                entries
                    .iter()
                    .any(|e| matches!(e, Entry::Item { label, .. } if label == "Extra high"))
            );
            assert_eq!(
                entries
                    .iter()
                    .any(|e| matches!(e, Entry::Item { label, .. } if label == "Ultra")),
                ultra
            );
        }
    }

    #[test]
    fn labels_read_like_names() {
        let l = super::model_label;
        assert_eq!(l("opus"), "Opus");
        assert_eq!(l("codex:gpt-6.1-sol"), "GPT-6.1 Sol");
        assert_eq!(l("codex:gpt-6-astra"), "GPT-6 Astra");
    }
}
