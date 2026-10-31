//! Pinned upstream API-equivalent estimates; exact model IDs only.
use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate(pub [f64; 4]);
pub fn model_key(model: &str) -> String {
    let key = model.trim().to_lowercase();
    let key = ["openai/", "anthropic/", "google/", "models/"]
        .iter()
        .find_map(|p| key.strip_prefix(p))
        .unwrap_or(&key);
    match key {
        "gpt-5.6" => "gpt-5.6-sol".into(),
        "claude-sonnet-4" => "claude-sonnet-4-20250514".into(),
        "claude-opus-4" => "claude-opus-4-20250514".into(),
        "claude-sonnet-4-5" => "claude-sonnet-4-5-20250929".into(),
        "claude-haiku-4-5" => "claude-haiku-4-5-20251001".into(),
        "gpt-5-2025-08-07" => "gpt-5".into(),
        "gpt-5.1-2025-11-13" => "gpt-5.1".into(),
        "gpt-5.2-2025-12-11" => "gpt-5.2".into(),
        "gpt-5.4-2026-03-05" => "gpt-5.4".into(),
        "gpt-5.5-2026-04-23" => "gpt-5.5".into(),
        other => other.into(),
    }
}
pub fn rate(model: &str) -> Option<Rate> {
    Some(Rate(match model_key(model).as_str() {
        "claude-opus-5" => [5.0, 25.0, 6.25, 0.5],
        "claude-opus-5-5" => [4.0, 20.0, 5.0, 0.2],
        "claude-sonnet-5" => [2.0, 10.0, 2.5, 0.2],
        "claude-sonnet-5-5" => [2.0, 10.0, 2.5, 0.1],
        "claude-opus-4-20250514" => [15.0, 75.0, 18.75, 1.5],
        "claude-sonnet-4-20250514" => [3.0, 15.0, 3.75, 0.3],
        "claude-sonnet-4-5-20250929" => [3.0, 15.0, 3.75, 0.3],
        "claude-opus-4-8" => [5.0, 25.0, 6.25, 0.5],
        "claude-opus-4-7" => [5.0, 25.0, 6.25, 0.5],
        "claude-sonnet-4-6" => [3.0, 15.0, 3.75, 0.3],
        "claude-haiku-4-5-20251001" => [1.0, 5.0, 1.25, 0.1],
        "claude-fable-5" => [10.0, 50.0, 12.5, 1.0],
        "claude-fable-5-1" => [10.0, 50.0, 12.5, 0.25],
        "gpt-6-astra" => [10.0, 50.0, 12.5, 1.0],
        "gpt-6-sol" => [2.0, 10.0, 2.5, 0.2],
        "gpt-6.1-sol" => [2.0, 10.0, 2.5, 0.1],
        "gpt-6-luna" => [0.1, 0.5, 0.125, 0.01],
        "gpt-5.6-sol" => [4.0, 20.0, 5.0, 0.4],
        "gpt-5.6-terra" => [2.0, 12.0, 2.5, 0.2],
        "gpt-5.6-luna" => [0.2, 1.2, 0.25, 0.02],
        "gpt-5" => [1.25, 10.0, 0.0, 0.125],
        "gpt-5-codex" => [1.25, 10.0, 0.0, 0.125],
        "gpt-5.1" => [1.25, 10.0, 0.0, 0.125],
        "gpt-5.1-codex" => [1.25, 10.0, 0.0, 0.125],
        "gpt-5.2" => [1.75, 14.0, 0.0, 0.175],
        "gpt-5.2-codex" => [1.75, 14.0, 0.0, 0.175],
        "gpt-5.3-codex" => [1.75, 14.0, 0.0, 0.175],
        "gpt-5.4" => [2.5, 15.0, 0.0, 0.25],
        "gpt-5.5" => [5.0, 30.0, 0.0, 0.5],
        "gemini-2.5-pro" => [1.25, 10.0, 0.0, 0.125],
        "gemini-2.5-flash" => [0.3, 2.5, 0.0, 0.03],
        "gemini-2.5-flash-lite" => [0.1, 0.4, 0.0, 0.01],
        "gemini-2.0-flash" => [0.1, 0.4, 0.0, 0.025],
        _ => return None,
    }))
}
pub fn estimate(model: &str, input: u64, output: u64, write: u64, read: u64) -> Option<f64> {
    let key = model_key(model);
    let Rate([i, o, w, r]) = rate(&key)?;
    if write > 0 && w == 0. {
        return None;
    }
    let prompt = input as f64 + write as f64 + read as f64;
    let long = (matches!(
        key.as_str(),
        "gpt-6-astra"
            | "gpt-6-sol"
            | "gpt-6.1-sol"
            | "gpt-6-luna"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-5.5"
            | "gpt-5.4"
    ) && prompt > 272000.)
        || (key == "gemini-2.5-pro" && prompt > 200000.);
    Some(
        ((input as f64 * i + write as f64 * w + read as f64 * r) * if long { 2. } else { 1. }
            + output as f64 * o * if long { 1.5 } else { 1. })
            / 1000000.,
    )
}
pub fn first_unpriced_sighting(model: &str) -> bool {
    static SEEN: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(model_key(model))
}
