use super::{Entry, dedup_claude, token};
use serde_json::Value;
use std::path::Path;
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    pub offset: u64,
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
#[cfg(test)]
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
        if !super::mentions(&line, &[b"\"usage\""]) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let m = &v["message"];
        let u = &m["usage"];
        if v["type"] != "assistant" || !u.is_object() {
            continue;
        }
        let Some(date) = crate::usage::timestamp(&v["timestamp"]) else {
            continue;
        };
        let (message, request) = (
            m["id"].as_str().unwrap_or(""),
            v["requestId"].as_str().unwrap_or(""),
        );
        result.entries.push(Entry {
            // Records with no identity cannot be duplicates of each other.
            id: if message.is_empty() && request.is_empty() {
                format!("anon|{}|{at}", path.display())
            } else {
                format!("{message}|{request}")
            },
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
