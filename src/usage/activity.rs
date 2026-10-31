//! Daily token totals from the agents' own local logs. Read-only; the aggregate stays in memory.
use super::{Provider, timestamp};
use objc2_foundation::{NSDate, NSTimeZone};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Weeks shown, and the days in them.
pub const WEEKS: i64 = 20;
pub const DAYS: usize = WEEKS as usize * 7;

/// Tokens per local day, oldest first; `first_day` is a [`local_day`] number.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Daily {
    pub first_day: i64,
    pub tokens: Vec<u64>,
}

/// Per-file results keyed by path, reused while the file's length and mtime are unchanged and
/// the time zone the days were bucketed in still applies.
#[derive(Default)]
pub struct Cache(HashMap<PathBuf, (u64, f64, Days)>, String);

/// (local day, tokens) pairs for one file.
type Days = Vec<(i64, u64)>;

/// Days since 1970-01-01 in the local time zone.
pub fn local_day(seconds: f64) -> i64 {
    let offset = NSTimeZone::localTimeZone()
        .secondsFromGMTForDate(&NSDate::dateWithTimeIntervalSince1970(seconds));
    ((seconds + offset as f64) / 86400.).floor() as i64
}

/// Tokens for the [`DAYS`] ending on `now`'s local day, from `$CLAUDE_CONFIG_DIR` or `$CODEX_HOME`.
/// Keep one [`Cache`] per provider: a scan drops entries it did not see.
pub fn scan(provider: Provider, cache: &mut Cache, now: f64) -> Daily {
    scan_in(provider, &super::config_dir(provider), cache, now)
}

pub(super) fn scan_in(provider: Provider, root: &Path, cache: &mut Cache, now: f64) -> Daily {
    #[cfg(debug_assertions)]
    let started = std::time::Instant::now();
    let today = local_day(now);
    let first_day = today - DAYS as i64 + 1;
    // Pick up a system time zone change; days bucketed in the old zone are parsed again.
    NSTimeZone::resetSystemTimeZone();
    let zone = NSTimeZone::localTimeZone().name().to_string();
    if cache.1 != zone {
        cache.0.clear();
        cache.1 = zone;
    }
    // Codex files sit under their start date, but a resumed session appends to its old file,
    // so every directory is walked and the mtime filter below skips the old ones.
    let mut files = vec![];
    walk(
        &root.join(match provider {
            Provider::Claude => "projects",
            Provider::Codex => "sessions",
        }),
        &mut files,
    );
    let mut old = std::mem::take(&mut cache.0);
    let mut hours = HashMap::new();
    let mut tokens = vec![0u64; DAYS];
    for path in files {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0., |d| d.as_secs_f64());
        if local_day(mtime) < first_day {
            continue;
        }
        let days = match old.remove(&path) {
            Some((len, time, days)) if len == meta.len() && time == mtime => days,
            _ => match File::open(&path) {
                Ok(file) => parse(provider, BufReader::new(file), &mut hours),
                Err(_) => continue,
            },
        };
        for &(day, n) in &days {
            if let Some(slot) = usize::try_from(day - first_day)
                .ok()
                .and_then(|i| tokens.get_mut(i))
            {
                *slot = slot.saturating_add(n);
            }
        }
        cache.0.insert(path, (meta.len(), mtime, days));
    }
    #[cfg(debug_assertions)]
    eprintln!(
        "{} activity: {} files in {:?}",
        provider.name(),
        cache.0.len(),
        started.elapsed()
    );
    Daily { first_day, tokens }
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&path, files),
            Ok(_) if path.extension().is_some_and(|e| e == "jsonl") => files.push(path),
            _ => {}
        }
    }
}

/// Local day of an ISO timestamp. Parsing dominated the scan, so each UTC hour is parsed once
/// unless it crosses a local midnight.
fn day_of(text: &str, hours: &mut HashMap<String, Option<i64>>) -> Option<i64> {
    let parse = |t: &str| timestamp(&t.into()).map(local_day);
    let Some(hour) = text.get(..13).filter(|_| text.ends_with('Z')) else {
        return parse(text);
    };
    let day = *hours.entry(hour.into()).or_insert_with(|| {
        let start = parse(&format!("{hour}:00:00Z"))?;
        (parse(&format!("{hour}:59:59Z"))? == start).then_some(start)
    });
    day.or_else(|| parse(text))
}

/// Tokens per local day in one log. Malformed and unrelated lines are skipped.
pub(super) fn parse(
    provider: Provider,
    reader: impl BufRead,
    hours: &mut HashMap<String, Option<i64>>,
) -> Vec<(i64, u64)> {
    let needle = match provider {
        Provider::Claude => "\"usage\"",
        Provider::Codex => "token_count",
    };
    let mut days: Vec<(i64, u64)> = vec![];
    let mut seen = HashMap::new();
    let mut last_total = Value::Null;
    // Split on bytes: a stray invalid UTF-8 line must not end the file early.
    for line in reader.split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        // Cheap prefilter: most lines are content, not usage.
        if !line.contains(needle) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let mut key = None;
        let n: u64 = match provider {
            Provider::Claude => {
                let (message, usage) = (&v["message"], &v["message"]["usage"]);
                if v["type"] != "assistant" || !usage.is_object() {
                    continue;
                }
                key = message["id"]
                    .is_string()
                    .then(|| (message["id"].to_string(), v["requestId"].to_string()));
                [
                    "input_tokens",
                    "output_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .map(|k| usage[k].as_u64().unwrap_or(0))
                .fold(0u64, u64::saturating_add)
            }
            Provider::Codex => {
                let info = &v["payload"]["info"];
                let total = &info["total_token_usage"];
                // Codex re-emits the last count unchanged; a repeated total adds nothing.
                if v["type"] != "event_msg"
                    || v["payload"]["type"] != "token_count"
                    || !info.is_object()
                    || total.is_object() && *total == last_total
                {
                    continue;
                }
                last_total = total.clone();
                let usage = &info["last_token_usage"];
                let get = |k: &str| usage[k].as_u64().unwrap_or(0);
                get("input_tokens")
                    .saturating_sub(get("cached_input_tokens"))
                    .saturating_add(get("output_tokens"))
            }
        };
        let Some(day) = v["timestamp"].as_str().and_then(|t| day_of(t, hours)) else {
            continue;
        };
        // A streamed message repeats on several lines, and only the last carries the final
        // `output_tokens`: count the growth on the day of its first line.
        let (day, n) = match key {
            Some(key) => {
                let (first_day, prev) = seen.entry(key).or_insert((day, 0));
                if n <= *prev {
                    continue;
                }
                let grew = n - *prev;
                *prev = n;
                (*first_day, grew)
            }
            None => (day, n),
        };
        match days.iter_mut().find(|(d, _)| *d == day) {
            Some(entry) => entry.1 = entry.1.saturating_add(n),
            None => days.push((day, n)),
        }
    }
    days
}
