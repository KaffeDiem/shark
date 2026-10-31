//! Evolution line presentation keeps unresolved branches hidden.
use super::{Companion, EvoLine};
#[derive(Clone, Debug, PartialEq)]
pub enum LineContent {
    Species(u16),
    Mystery,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineState {
    Done,
    Current,
    Future,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LineItem {
    pub content: LineContent,
    pub state: LineState,
}
pub fn realized_line_items(path: &[u16], stage: usize) -> Vec<LineItem> {
    path.iter()
        .enumerate()
        .map(|(i, &id)| LineItem {
            content: LineContent::Species(id),
            state: if i == stage {
                LineState::Current
            } else {
                LineState::Done
            },
        })
        .collect()
}
impl Companion {
    pub fn line_nodes(&self) -> Vec<LineItem> {
        let Some(mon) = &self.active else {
            return vec![];
        };
        let path = mon
            .path
            .iter()
            .copied()
            .take(mon.stage.saturating_add(1))
            .collect::<Vec<_>>();
        let mut items = realized_line_items(&path, mon.stage);
        if let Some(mut node) = mon.line.tree.node(mon.species_id()) {
            while node.children.len() == 1 {
                node = &node.children[0];
                items.push(LineItem {
                    content: LineContent::Species(node.species_id),
                    state: LineState::Future,
                });
            }
            if node.children.len() > 1 {
                items.push(LineItem {
                    content: LineContent::Mystery,
                    state: LineState::Future,
                });
            }
        }
        items
    }
}
pub fn normalized_names(
    names: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    names
        .iter()
        .filter_map(|(code, name)| {
            let code = code.trim().replace('_', "-").to_lowercase();
            let name = name.trim();
            (!code.is_empty() && !name.is_empty()).then(|| (code, name.to_owned()))
        })
        .collect()
}

pub fn resolve_name(
    names: &std::collections::BTreeMap<String, String>,
    preferred: &[&str],
) -> Option<String> {
    let names = normalized_names(names);
    preferred.iter().copied().chain(["en"]).find_map(|code| {
        names
            .get(&code.trim().replace('_', "-").to_lowercase())
            .cloned()
    })
}

impl EvoLine {
    pub fn localized_name(&self, id: u16, language: &str) -> String {
        let Some(names) = self.names.get(&id) else {
            return format!("#{id}");
        };
        let codes = match language {
            "ja" => vec!["ja-hrkt", "ja"],
            "pt" => vec!["pt-br", "pt"],
            other => vec![other],
        };
        resolve_name(names, &codes).unwrap_or_else(|| format!("#{id}"))
    }
}
