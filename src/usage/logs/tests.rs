use super::*;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
mod cache;
mod claude;
mod codex;
mod cost;
mod pricing;
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "shark-logs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }
    fn roots(&self) -> Roots {
        Roots(vec![self.0.clone()])
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn assistant(
    id: &str,
    model: &str,
    input: u64,
    output: u64,
    write: u64,
    read: u64,
    day: u8,
) -> Value {
    json!({"type":"assistant","timestamp":format!("2026-09-{day:02}T12:00:00Z"),"requestId":format!("r-{id}"),"message":{"id":id,"model":model,"usage":{"input_tokens":input,"output_tokens":output,"cache_creation_input_tokens":write,"cache_read_input_tokens":read}}})
}
fn lines(values: &[Value]) -> Vec<u8> {
    let mut out = vec![];
    for v in values {
        out.extend(serde_json::to_vec(v).unwrap());
        out.push(b'\n')
    }
    out
}
fn bucket(es: &[Entry]) -> Bucket {
    let mut b = Bucket::default();
    for e in es {
        b.add(e)
    }
    b
}
fn between(r: &Report, start: f64, end: f64) -> Bucket {
    bucket(
        &r.entries
            .iter()
            .filter(|e| e.date >= start && e.date < end)
            .cloned()
            .collect::<Vec<_>>(),
    )
}
fn daily_buckets(r: &Report) -> std::collections::BTreeMap<i64, Bucket> {
    let mut days = std::collections::BTreeMap::<i64, Bucket>::new();
    for e in &r.entries {
        days.entry(crate::usage::activity::local_day(e.date))
            .or_default()
            .add(e)
    }
    days
}
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-10, "{a} != {b}")
}
fn epoch(day: u8) -> f64 {
    crate::usage::timestamp(&json!(format!("2026-09-{day:02}T12:00:00Z"))).unwrap()
}
fn meta(id: &str, parent: Option<&str>, subagent: bool) -> Value {
    json!({"type":"session_meta","payload":{"id":id,"forked_from_id":parent,"thread_source":if subagent{"subagent"}else{"cli"}}})
}
fn event(second: f64, cumulative: Option<[u64; 6]>, last: [u64; 6]) -> Value {
    fn vector(v: [u64; 6]) -> Value {
        json!({"input_tokens":v[0],"cached_input_tokens":v[1],"cache_write_input_tokens":v[2],"output_tokens":v[3],"reasoning_output_tokens":v[4],"total_tokens":v[5]})
    }
    json!({"type":"event_msg","timestamp":format!("2026-09-15T12:00:{second:06.3}Z"),"payload":{"type":"token_count","info":{"total_token_usage":cumulative.map(vector),"last_token_usage":vector(last)}}})
}
fn ctx(model: &str) -> Value {
    json!({"type":"turn_context","payload":{"model":model}})
}
fn rollout(name: &str, vs: &[Value]) -> super::codex::Rollout {
    super::codex::parse(&lines(vs), Path::new(name))
}
