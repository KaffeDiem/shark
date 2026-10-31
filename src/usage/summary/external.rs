//! Optional decoder for future providers that return ccusage-compatible summary JSON.
//! Built-in quota queries and local transcript readers do not run ccusage.
use super::*;
use serde_json::Value;
#[derive(Clone, Debug)]
pub struct Row {
    pub period: String,
    pub bucket: Bucket,
    pub total_tokens: u64,
}
#[derive(Clone, Debug)]
pub struct Block {
    pub id: String,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub active: bool,
    pub bucket: Bucket,
    pub total_tokens: u64,
    pub tokens_per_minute: Option<f64>,
}
#[derive(Clone, Debug, Default)]
pub struct ExternalReport {
    pub daily: Vec<Row>,
    pub weekly: Vec<Row>,
    pub monthly: Vec<Row>,
    pub blocks: Vec<Block>,
}
fn bucket(v: &Value) -> Result<Bucket, serde_json::Error> {
    let mut v = v.clone();
    if v.get("cacheReadTokens").is_none() {
        if let Some(cached) = v.get("cachedInputTokens").cloned() {
            v["cacheReadTokens"] = cached
        }
    }
    serde_json::from_value(v)
}
fn total(v: &Value, b: &Bucket) -> u64 {
    v["totalTokens"].as_u64().unwrap_or_else(|| b.total())
}
fn rows(v: &Value, key: &str) -> Result<Vec<Row>, serde_json::Error> {
    v[key]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let bucket = bucket(v)?;
            let period = ["date", "period", "week", "month"]
                .into_iter()
                .find_map(|key| v[key].as_str())
                .unwrap_or("")
                .into();
            let total_tokens = total(v, &bucket);
            Ok(Row {
                period,
                bucket,
                total_tokens,
            })
        })
        .collect()
}
impl ExternalReport {
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let v: Value = serde_json::from_slice(bytes)?;
        let blocks = v["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| {
                let bucket = bucket(v)?;
                let total_tokens = total(v, &bucket);
                Ok(Block {
                    id: v["id"].as_str().unwrap_or("").into(),
                    start: crate::usage::timestamp(&v["startTime"]),
                    end: crate::usage::timestamp(&v["endTime"]),
                    active: v["isActive"].as_bool().unwrap_or(false),
                    tokens_per_minute: v["tokensPerMinute"]
                        .as_f64()
                        .or_else(|| v["burnRate"]["tokensPerMinute"].as_f64())
                        .filter(|v| v.is_finite() && *v >= 0.),
                    bucket,
                    total_tokens,
                })
            })
            .collect::<Result<_, serde_json::Error>>()?;
        Ok(Self {
            daily: rows(&v, "daily")?,
            weekly: rows(&v, "weekly")?,
            monthly: rows(&v, "monthly")?,
            blocks,
        })
    }
    /// Turn dated summaries into the same generic report consumed by Shark's provider API.
    pub fn into_report(self, source: &str, now: f64) -> Report {
        let entries = self
            .daily
            .into_iter()
            .filter_map(|row| {
                let date = NaiveDate::parse_from_str(&row.period, "%Y-%m-%d").ok()?;
                let date = Local
                    .from_local_datetime(&date.and_hms_opt(12, 0, 0)?)
                    .earliest()?
                    .timestamp() as f64;
                let b = row.bucket;
                let known = b.coverage.has_known();
                let mut e = Entry {
                    id: format!("{source}|{}", row.period),
                    date,
                    model: format!("{source}/summary"),
                    input: b.input,
                    output: b.output,
                    cache_write: b.cache_write,
                    cache_read: b.cache_read,
                    explicit_cost: known.then_some(b.cost),
                    cost_is_estimate: Some(b.coverage.estimated),
                    cost_unavailable: Some(b.coverage.unknown),
                    ..Default::default()
                };
                if e.total() != row.total_tokens {
                    e.input = row.total_tokens;
                    e.output = 0;
                    e.cache_write = 0;
                    e.cache_read = 0;
                    e.cost_unavailable = Some(true)
                }
                Some(e)
            })
            .collect();
        Report::from_entries(entries, now)
    }
}
pub fn forecast_depletion(
    block_tokens: u64,
    tokens_per_minute: f64,
    utilization: f64,
    now: f64,
) -> Option<f64> {
    if !(5. ..100.).contains(&utilization)
        || block_tokens == 0
        || !tokens_per_minute.is_finite()
        || tokens_per_minute < 10000.
        || !now.is_finite()
    {
        return None;
    }
    let minutes = (100. - utilization) * (block_tokens as f64 / utilization) / tokens_per_minute;
    (minutes.is_finite() && minutes < 1440.).then_some(now + minutes * 60.)
}
