use super::{Entry, dedup_claude, token};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    pub ledger: BTreeMap<String, f64>,
    pub offset: u64,
}
fn model_key(s: &str) -> &str {
    s.split('[').next().unwrap_or(s)
}
pub fn session(path: &Path) -> String {
    if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|s| s == "subagents")
    {
        return path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
pub fn parse(bytes: &[u8], path: &Path, base: u64) -> Parsed {
    parse_reader(std::io::Cursor::new(bytes), path, base)
        .expect("reading an in-memory log cannot fail")
}
pub fn parse_reader(
    mut reader: impl std::io::BufRead,
    path: &Path,
    base: u64,
) -> std::io::Result<Parsed> {
    let mut result = Parsed {
        offset: base,
        ..Parsed::default()
    };
    let session = session(path);
    let mut at = base;
    let mut line = Vec::new();
    while let Some((consumed, terminated)) = super::read_record(&mut reader, &mut line)? {
        at = at.saturating_add(consumed);
        if terminated {
            result.offset = at
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if v["type"] == "cost-state" {
            if let Some(models) = v["modelUsage"].as_object() {
                let mut ledger = BTreeMap::new();
                for (name, raw) in models {
                    if let Some(cost) = raw["costUSD"]
                        .as_f64()
                        .filter(|c| c.is_finite() && *c >= 0.)
                    {
                        *ledger.entry(model_key(name).to_owned()).or_insert(0.) += cost;
                    }
                }
                if !ledger.is_empty() {
                    result.ledger = ledger
                }
            }
            continue;
        }
        let m = &v["message"];
        let u = &m["usage"];
        if v["type"] != "assistant" || !u.is_object() {
            continue;
        }
        let Some(date) = crate::usage::timestamp(&v["timestamp"]) else {
            continue;
        };
        result.entries.push(Entry {
            id: format!(
                "{}|{}",
                m["id"].as_str().unwrap_or(""),
                v["requestId"].as_str().unwrap_or("")
            ),
            date,
            model: m["model"].as_str().unwrap_or("unknown").into(),
            input: token(&u["input_tokens"]),
            output: token(&u["output_tokens"]),
            cache_write: token(&u["cache_creation_input_tokens"]),
            cache_read: token(&u["cache_read_input_tokens"]),
            session_ids: [session.clone()].into(),
            ..Entry::default()
        });
    }
    result.entries = dedup_claude(result.entries);
    Ok(result)
}
pub fn apply_ledger(entries: &mut [Entry], ledger: &BTreeMap<String, f64>) {
    for e in entries.iter_mut() {
        e.explicit_cost = None;
        e.cost_is_estimate = None
    }
    for (model, cost) in ledger {
        let indices: Vec<_> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.total() > 0 && model_key(&e.model) == model)
            .map(|(i, _)| i)
            .collect();
        let total: f64 = indices.iter().map(|i| entries[*i].total() as f64).sum();
        let mut remaining = *cost;
        for (n, i) in indices.iter().enumerate() {
            let share = if n + 1 == indices.len() {
                remaining
            } else {
                cost * entries[*i].total() as f64 / total
            };
            entries[*i].explicit_cost = Some(share.max(0.));
            entries[*i].cost_is_estimate = Some(false);
            remaining -= share;
        }
    }
}
