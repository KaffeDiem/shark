//! The models a chat can run and the thinking levels each takes.
use crate::codex;
use crate::state::ChatMeta;

/// Every chat names one: claude gets `--model`, `codex:<model id>` runs `codex app-server` with
/// that model (see `codex::is_codex`).
pub(crate) const MODELS: &[&str] = &[
    "fable",
    "opus",
    "sonnet",
    "haiku",
    "codex:gpt-6.1-sol",
    "codex:gpt-6-astra",
];

/// Thinking levels, lowest first: claude's `--effort`, codex's `effort` (both models take all six,
/// per `model/list`). Haiku takes `--effort` too.
const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const CODEX_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];
/// A chat's level until one is picked.
pub(crate) const EFFORT: &str = "high";

/// The thinking levels `model` takes, lowest first.
pub(crate) fn efforts(model: &str) -> &'static [&'static str] {
    match codex::is_codex(Some(model)) {
        true => CODEX_EFFORTS,
        false => CLAUDE_EFFORTS,
    }
}

/// The level a chat runs at: its own (or `high`). One the model lacks is its highest if it was
/// `ultra`, else `high`, so an unknown value never picks the costliest.
pub(crate) fn effort_of(meta: &ChatMeta) -> &'static str {
    let levels = efforts(meta.model());
    let want = meta.effort.as_deref().unwrap_or(EFFORT);
    match levels.iter().find(|l| **l == want) {
        Some(l) => l,
        None if want == "ultra" => levels.last().unwrap(),
        None => levels
            .iter()
            .find(|l| **l == EFFORT)
            .unwrap_or(levels.last().unwrap()),
    }
}

impl ChatMeta {
    /// The chat's model: `MODELS[0]` until one is picked.
    pub(crate) fn model(&self) -> &str {
        self.model.as_deref().unwrap_or(MODELS[0])
    }

    /// Whether the chat runs on codex rather than claude.
    pub(crate) fn is_codex(&self) -> bool {
        codex::is_codex(self.model.as_deref())
    }
}

/// A thinking level as people read it: "xhigh" → "Extra high".
pub(crate) fn effort_label(level: &str) -> String {
    match level {
        "xhigh" => "Extra high".into(),
        l => model_label(l),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_and_effort_defaults() {
        let mut meta = ChatMeta::default();
        assert_eq!(
            (meta.model(), meta.is_codex(), effort_of(&meta)),
            (MODELS[0], false, "high")
        );
        meta.model = Some("codex:gpt-6.1-sol".into());
        meta.effort = Some("ultra".into());
        assert_eq!((meta.is_codex(), effort_of(&meta)), (true, "ultra"));
        meta.model = Some("opus".into());
        assert_eq!(effort_of(&meta), "max");
    }
    #[test]
    fn labels_read_like_names() {
        let l = super::model_label;
        assert_eq!(l("opus"), "Opus");
        assert_eq!(l("codex:gpt-6.1-sol"), "GPT-6.1 Sol");
        assert_eq!(l("codex:gpt-6-astra"), "GPT-6 Astra");
    }
}
