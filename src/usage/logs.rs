//! Inclusive local token accounting, separate from Shark's cache-excluding activity heatmap.
//! Pricing is an estimate from PokeTokenBar's pinned table, never a subscription invoice.
use super::{
    Provider,
    activity::{DAYS, Daily, local_day},
};
use objc2_foundation::NSTimeZone;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub mod cache;
pub mod claude;
pub mod codex;
pub mod cost;
pub mod pricing;
pub mod roots;
pub mod timing;
pub use cache::Cache;
pub use roots::Roots;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub id: String,
    pub date: f64,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub explicit_cost: Option<f64>,
    pub cost_is_estimate: Option<bool>,
    pub cost_unavailable: Option<bool>,
    pub session_ids: BTreeSet<String>,
}
impl Entry {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CostCoverage {
    pub reported: bool,
    pub estimated: bool,
    pub unknown: bool,
}
impl CostCoverage {
    pub fn merge(&mut self, other: Self) {
        self.reported |= other.reported;
        self.estimated |= other.estimated;
        self.unknown |= other.unknown;
    }
    pub fn has_known(self) -> bool {
        self.reported || self.estimated
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bucket {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub cost: f64,
    pub coverage: CostCoverage,
}
impl Bucket {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }
    pub fn add(&mut self, e: &Entry) {
        self.input = self.input.saturating_add(e.input);
        self.output = self.output.saturating_add(e.output);
        self.cache_write = self.cache_write.saturating_add(e.cache_write);
        self.cache_read = self.cache_read.saturating_add(e.cache_read);
        if e.total() == 0 {
            return;
        }
        if let Some(cost) = e.explicit_cost.filter(|c| c.is_finite() && *c >= 0.) {
            self.cost = (self.cost + cost).min(f64::MAX);
            if e.cost_is_estimate == Some(true) {
                self.coverage.estimated = true
            } else {
                self.coverage.reported = true
            }
        } else if e.cost_unavailable != Some(true) {
            if let Some(cost) =
                pricing::estimate(&e.model, e.input, e.output, e.cache_write, e.cache_read)
            {
                self.cost = (self.cost + cost).min(f64::MAX);
                self.coverage.estimated = true
            } else {
                self.coverage.unknown = true;
            }
        } else {
            self.coverage.unknown = true
        }
    }
    pub fn merge(&mut self, b: &Self) {
        self.input = self.input.saturating_add(b.input);
        self.output = self.output.saturating_add(b.output);
        self.cache_write = self.cache_write.saturating_add(b.cache_write);
        self.cache_read = self.cache_read.saturating_add(b.cache_read);
        self.cost = (self.cost + b.cost).min(f64::MAX);
        self.coverage.merge(b.coverage);
    }
    pub fn cost_text(&self, compact: bool) -> String {
        if self.coverage.unknown && !self.coverage.has_known() {
            return if compact { "$—" } else { "Unavailable" }.into();
        }
        if compact {
            crate::pokemon::format::compact_cost(self.cost)
        } else {
            format!("${:.2}", self.cost)
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub daily: Daily,
    pub entries: Vec<Entry>,
    pub totals: Bucket,
    /// A failed local scan is not evidence of zero usage.
    pub error: Option<String>,
}
impl Report {
    pub fn from_daily(daily: Daily) -> Self {
        let total = daily.tokens.iter().copied().fold(0, u64::saturating_add);
        Self {
            daily,
            totals: Bucket {
                input: total,
                coverage: CostCoverage {
                    unknown: total > 0,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Self::default()
        }
    }
    pub fn from_entries(entries: Vec<Entry>, now: f64) -> Self {
        let first_day = local_day(now) - DAYS as i64 + 1;
        let mut daily = Daily {
            first_day,
            tokens: vec![0; DAYS],
        };
        let mut totals = Bucket::default();
        for e in &entries {
            if let Ok(i) = usize::try_from(local_day(e.date) - first_day) {
                if let Some(n) = daily.tokens.get_mut(i) {
                    *n = n.saturating_add(e.total());
                    totals.add(e)
                }
            }
        }
        Self {
            daily,
            entries,
            totals,
            error: None,
        }
    }
    pub fn today(&self) -> u64 {
        self.daily.tokens.last().copied().unwrap_or(0)
    }
    /// The navigation heatmap's definition: everything except cache reads.
    pub fn heatmap(&self) -> Daily {
        let first_day = self.daily.first_day;
        let mut tokens = vec![0u64; DAYS];
        for e in &self.entries {
            if let Some(n) = usize::try_from(local_day(e.date) - first_day)
                .ok()
                .and_then(|i| tokens.get_mut(i))
            {
                let tokens = e
                    .input
                    .saturating_add(e.output)
                    .saturating_add(e.cache_write);
                *n = n.saturating_add(tokens);
            }
        }
        Daily { first_day, tokens }
    }
}
pub fn scan(provider: Provider, roots: &Roots, cache: &mut Cache, now: f64) -> Report {
    // Pick up a system time zone change before bucketing days.
    NSTimeZone::resetSystemTimeZone();
    let entries = cache.entries_since(provider, roots, now - (DAYS as f64 + 1.) * 86400.);
    let mut report = Report::from_entries(entries, now);
    report.error = cache.last_error.clone();
    report
}
/// Bounds leave headroom for sums from untrusted log values; negative/non-number counts are zero.
pub fn token(v: &Value) -> u64 {
    v.as_f64()
        .filter(|n| n.is_finite() && *n > 0.)
        .map(|n| n.min(1_000_000_000_000_000.) as u64)
        .unwrap_or(0)
}
/// Cheap prefilter before JSON parsing: most log lines are prompts and tool output.
fn mentions(line: &[u8], needles: &[&[u8]]) -> bool {
    needles
        .iter()
        .any(|n| line.windows(n.len()).any(|w| w == *n))
}
pub fn dedup_claude(entries: Vec<Entry>) -> Vec<Entry> {
    let mut map = BTreeMap::<String, Entry>::new();
    for e in entries {
        if let Some(old) = map.get_mut(&e.id) {
            let date = old.date.min(e.date);
            let mut sessions = old.session_ids.clone();
            sessions.extend(e.session_ids.iter().cloned());
            if e.total() > old.total() {
                *old = e
            }
            old.date = date;
            old.session_ids = sessions;
        } else {
            map.insert(e.id.clone(), e);
        }
    }
    map.into_values().collect()
}
#[cfg(test)]
mod tests;

/// Read a JSONL record while bounding allocations even for huge prompt/image records.
/// Empty bytes represent a skipped oversized record; byte counts still advance checkpoints.
fn read_record(
    reader: &mut impl std::io::BufRead,
    line: &mut Vec<u8>,
) -> std::io::Result<Option<(u64, bool)>> {
    const MAX_LINE: usize = 8 * 1024 * 1024;
    line.clear();
    let mut oversized = false;
    let mut consumed = 0u64;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok((consumed > 0).then_some((consumed, false)));
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let length = end.map_or(bytes.len(), |n| n + 1);
        consumed = consumed.saturating_add(length as u64);
        if !oversized {
            if line.len().saturating_add(length) <= MAX_LINE {
                line.extend_from_slice(&bytes[..length]);
            } else {
                line.clear();
                oversized = true;
            }
        }
        reader.consume(length);
        if end.is_some() {
            return Ok(Some((consumed, true)));
        }
    }
}
