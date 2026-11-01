use super::{Entry, token};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    path::Path,
};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vector(pub [u64; 6]);
impl Vector {
    fn read(v: &Value) -> Self {
        Self(
            [
                "input_tokens",
                "cached_input_tokens",
                "cache_write_input_tokens",
                "output_tokens",
                "reasoning_output_tokens",
                "total_tokens",
            ]
            .map(|k| token(&v[k])),
        )
    }
    fn components(&self) -> u64 {
        self.0[0]
            .saturating_sub(self.0[1])
            .saturating_add(self.0[1])
            .saturating_add(self.0[3])
    }
    fn lower(&self, b: &Self) -> bool {
        self.0.iter().zip(b.0).any(|(a, b)| *a < b)
    }
    fn fingerprint(&self) -> String {
        self.0.map(|v| v.to_string()).join(",")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub cumulative: Vector,
    pub last: Vector,
}
impl State {
    fn fingerprint(&self) -> String {
        format!(
            "{}|{}",
            self.cumulative.fingerprint(),
            self.last.fingerprint()
        )
    }
}
#[derive(Clone, Debug)]
pub struct Event {
    pub entry: Entry,
    pub state: Option<State>,
    pub session: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct Rollout {
    pub path: String,
    pub session: Option<String>,
    pub parent: Option<String>,
    pub subagent: bool,
    pub events: Vec<Event>,
}
fn nonempty(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}
fn meta(v: &Value) -> Option<(Option<String>, Option<String>, bool)> {
    if v["type"] != "session_meta" {
        return None;
    }
    let p = &v["payload"];
    Some((
        nonempty(&p["id"]).or_else(|| nonempty(&p["session_id"])),
        nonempty(&p["forked_from_id"]).or_else(|| nonempty(&p["parent_thread_id"])),
        p["thread_source"] == "subagent" || !p["source"]["subagent"].is_null(),
    ))
}
#[cfg(test)]
pub fn parse(bytes: &[u8], path: &Path) -> Rollout {
    parse_reader(std::io::Cursor::new(bytes), path)
        .expect("reading an in-memory rollout cannot fail")
}

/// Stream transcripts without retaining prompts or allocating the whole session file.
/// Oversized individual records are skipped; a token event is ordinarily only a few KiB.
pub fn parse_reader(mut reader: impl std::io::BufRead, path: &Path) -> std::io::Result<Rollout> {
    let mut r = Rollout {
        path: path.to_string_lossy().into(),
        ..Rollout::default()
    };
    let mut model = "codex".to_string();
    let mut current = None;
    let mut previous = None;
    let mut saw_meta = false;
    let mut turn = 0;
    let mut line = Vec::new();
    while next_line(&mut reader, &mut line)? {
        if !super::mentions(&line, &[b"token_count", b"session_meta", b"turn_context"]) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some((id, parent, subagent)) = meta(&v) {
            if !saw_meta {
                r.session = id.clone();
                r.parent = parent;
                r.subagent = subagent;
                saw_meta = true
            }
            if id != current {
                previous = None
            }
            current = id;
            continue;
        }
        if v["type"] == "turn_context" || v["payload"]["type"] == "turn_context" {
            if let Some(m) = v["payload"]["model"].as_str() {
                model = m.into()
            }
            continue;
        }
        if v["payload"]["type"] != "token_count" {
            continue;
        }
        let info = &v["payload"]["info"];
        let last = &info["last_token_usage"];
        if !last.is_object() {
            continue;
        }
        let Some(date) = crate::usage::timestamp(&v["timestamp"]) else {
            continue;
        };
        let last = Vector::read(last);
        let cumulative = info["total_token_usage"]
            .is_object()
            .then(|| Vector::read(&info["total_token_usage"]));
        let state = cumulative.as_ref().map(|c| State {
            cumulative: c.clone(),
            last: last.clone(),
        });
        turn += 1;
        if state.is_some() && previous == state {
            continue;
        }
        previous = state.clone();
        let mut input = last.0[0].saturating_sub(last.0[1]);
        let read = last.0[1];
        let output = last.0[3];
        let total_only = input == 0 && read == 0 && output == 0 && last.0[5] > 0;
        if total_only
            && cumulative
                .as_ref()
                .is_none_or(|c| (c.components() == 0 && c.0[5] > 0) || last.0[5] == c.0[5])
        {
            input = last.0[5]
        }
        let entry = Entry {
            id: format!("codex|{}|{turn}", r.path),
            date,
            model: model.clone(),
            input,
            output,
            cache_read: read,
            cost_unavailable: total_only.then_some(true),
            ..Entry::default()
        };
        r.events.push(Event {
            entry,
            state,
            session: current.clone(),
        });
    }
    Ok(r)
}

fn next_line(reader: &mut impl std::io::BufRead, line: &mut Vec<u8>) -> std::io::Result<bool> {
    super::read_record(reader, line).map(|record| record.is_some())
}

fn fallback(r: &Rollout) -> usize {
    if r.parent.is_none() || r.subagent {
        return 0;
    }
    if r.events.is_empty() {
        return 0;
    }
    let mut n = 1;
    while n < r.events.len() && r.events[n].entry.date - r.events[n - 1].entry.date < 1. {
        n += 1
    }
    n
}
#[derive(Clone, Default)]
struct Resolved {
    entries: Vec<Entry>,
    history: Vec<Event>,
}
fn owned(r: &Rollout, replay: usize, mut history: Vec<Event>) -> Resolved {
    let mut entries = vec![];
    let mut previous: Option<Vector> = None;
    let mut owner = None;
    let mut epoch = 0;
    for event in r.events.iter().skip(replay) {
        let next_owner = if r.parent.is_some() {
            r.session.clone()
        } else {
            event.session.clone().or_else(|| r.session.clone())
        };
        if next_owner != owner {
            owner = next_owner;
            epoch = 0;
            previous = None
        }
        let prior = previous.clone();
        if let Some(s) = &event.state {
            if prior.as_ref().is_some_and(|p| s.cumulative.lower(p)) {
                epoch += 1
            }
            previous = Some(s.cumulative.clone())
        } else {
            previous = None
        }
        let mut e = event.entry.clone();
        if let Some(s) = &event.state {
            if e.total() == 0
                && s.last.components() == 0
                && s.last.0[5] > 0
                && prior.is_some_and(|p| s.cumulative.0[5] > p.0[5])
            {
                e.input = s.last.0[5];
                e.cost_unavailable = Some(true)
            }
            if let Some(owner) = &owner {
                e.id = format!("codex|{owner}|{epoch}|{}", s.fingerprint())
            }
        }
        entries.push(e);
        history.push(event.clone());
    }
    Resolved { entries, history }
}
fn resolve_one(
    i: usize,
    rs: &[Rollout],
    memo: &mut BTreeMap<usize, Resolved>,
    visiting: &mut BTreeSet<usize>,
) -> Resolved {
    if let Some(r) = memo.get(&i) {
        return r.clone();
    }
    let r = &rs[i];
    if !visiting.insert(i) {
        return owned(r, fallback(r), vec![]);
    }
    let mut best: Option<(usize, Vec<Event>)> = None;
    if let Some(parent) = &r.parent {
        for (j, p) in rs.iter().enumerate() {
            if i == j || p.session.as_ref() != Some(parent) {
                continue;
            }
            let p = resolve_one(j, rs, memo, visiting);
            let mut n = 0;
            let mut comparable = true;
            for (c, p) in r.events.iter().zip(&p.history) {
                match (&c.state, &p.state) {
                    (Some(c), Some(p)) if c == p => n += 1,
                    (Some(_), Some(_)) => break,
                    _ => {
                        comparable = false;
                        break;
                    }
                }
            }
            if comparable && n > best.as_ref().map_or(0, |b| b.0) {
                best = Some((n, p.history[..n].to_vec()))
            }
        }
    }
    let out = if let Some((n, history)) = best {
        owned(r, n, history)
    } else {
        owned(r, fallback(r), vec![])
    };
    visiting.remove(&i);
    memo.insert(i, out.clone());
    out
}
#[cfg(test)]
pub fn resolve(rollouts: Vec<Rollout>) -> Vec<Entry> {
    let included = rollouts.iter().map(|r| r.path.clone()).collect();
    resolve_included(rollouts, &included)
}
pub fn resolve_included(mut rollouts: Vec<Rollout>, included: &BTreeSet<String>) -> Vec<Entry> {
    rollouts.sort_by(|a, b| a.path.cmp(&b.path));
    let mut memo = BTreeMap::new();
    let mut entries = BTreeMap::<String, Entry>::new();
    for i in 0..rollouts.len() {
        if !included.contains(&rollouts[i].path) {
            continue;
        }
        for e in resolve_one(i, &rollouts, &mut memo, &mut BTreeSet::new()).entries {
            if entries.get(&e.id).is_none_or(|old| e.date < old.date) {
                entries.insert(e.id.clone(), e);
            }
        }
    }
    entries.into_values().collect()
}
/// Metadata-only bounded read. I/O failures remain errors so the index can retry them.
pub fn probe(path: &Path, limit: usize) -> io::Result<Option<String>> {
    let mut bytes = vec![];
    std::fs::File::open(path)?
        .take(limit as u64)
        .read_to_end(&mut bytes)?;
    for line in bytes.split(|b| *b == b'\n') {
        if std::str::from_utf8(line).is_err() {
            return Ok(None);
        }
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            if let Some((id, _, _)) = meta(&v) {
                return Ok(id);
            }
            if v["payload"]["type"] == "token_count" {
                return Ok(None);
            }
        } else if line.windows(11).any(|w| w == b"token_count") {
            return Ok(None);
        }
    }
    Ok(None)
}
