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
}
