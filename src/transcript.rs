//! A chat's stored transcript and stderr log: what is clipped on the way in, appended,
//! read back, and what the stored events say about the chat's state.
use crate::agent::{Ev, parse_record, text_of};
use crate::codex;
use crate::state::{Workspace, support_dir};
use serde_json::Value;
use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

/// Characters of tool output kept, in memory and in the transcript; the UI shows no more.
pub(crate) const MAX_OUTPUT: usize = 20_000;

/// Byte offset of the `n`th character, if `s` is longer than that.
pub(crate) fn cut(s: &str, n: usize) -> Option<usize> {
    s.char_indices().nth(n).map(|(i, _)| i)
}

/// A tool result's output as shown: clipped, saying so. `total_chars` marks lines `compact`
/// already clipped.
pub(crate) fn output(content: &Value, total: &Value) -> String {
    let text = text_of(content);
    let total = total.as_u64();
    match (cut(&text, MAX_OUTPUT), total) {
        (None, None) => text,
        (at, total) => format!(
            "{}\n… truncated, {} chars total",
            &text[..at.unwrap_or(text.len())],
            total.unwrap_or(text.chars().count() as u64)
        ),
    }
}

/// Characters kept of each string in a stored tool input (Write's content, Edit's strings).
/// Only the summary line is shown, and that comes from the start.
const MAX_INPUT: usize = 2_000;

pub(crate) fn clip_strings(v: &mut Value) -> bool {
    match v {
        Value::String(s) => cut(s, MAX_INPUT).map(|i| s.truncate(i)).is_some(),
        Value::Array(a) => a.iter_mut().fold(false, |c, v| clip_strings(v) | c),
        Value::Object(o) => o.values_mut().fold(false, |c, v| clip_strings(v) | c),
        _ => false,
    }
}

/// The line as stored: tool results and inputs clipped, since the transcript is replayed on
/// every open.
pub(crate) fn compact(line: &str) -> Cow<'_, str> {
    let Ok(mut v) = serde_json::from_str::<Value>(line) else {
        return line.into();
    };
    if v["type"].is_null() {
        return match codex::compact(&mut v) {
            true => v.to_string().into(),
            false => line.into(),
        };
    }
    let mut changed = false;
    let kind = v["type"].clone();
    let Some(blocks) = v
        .pointer_mut("/message/content")
        .and_then(Value::as_array_mut)
    else {
        return line.into();
    };
    for b in blocks.iter_mut() {
        match (kind.as_str(), b["type"].as_str()) {
            (Some("assistant"), Some("tool_use")) => changed |= clip_strings(&mut b["input"]),
            (Some("user"), Some("tool_result")) => changed |= clip_output(b, "content"),
            _ => {}
        }
    }
    match changed {
        true => v.to_string().into(),
        false => line.into(),
    }
}

/// Cuts `v[key]` (a string, or content blocks joined) to what `output` shows, noting the full
/// length as `output` expects.
pub(crate) fn clip_output(v: &mut Value, key: &str) -> bool {
    let Some(c) = v.get_mut(key) else {
        return false;
    };
    let text = text_of(c);
    let Some(i) = cut(&text, MAX_OUTPUT) else {
        return false;
    };
    *c = text[..i].into();
    v["total_chars"] = text.chars().count().into();
    true
}

pub(crate) fn logs_dir() -> PathBuf {
    support_dir().join("logs")
}

/// A process's stderr log, named for its chat (or `ask`, or a side chat).
pub(crate) fn log_path(name: &str) -> PathBuf {
    logs_dir().join(format!("{name}.stderr.log"))
}

pub(crate) fn transcripts_dir() -> PathBuf {
    support_dir().join("transcripts")
}

pub fn transcript_path(chat: &str) -> PathBuf {
    transcripts_dir().join(format!("{chat}.jsonl"))
}

/// Deletes a chat's transcript and stderr log.
pub fn forget(chat: &str) {
    let _ = fs::remove_file(transcript_path(chat));
    let _ = fs::remove_file(log_path(chat));
}

/// When any of the workspace's chats last wrote to its transcript. Reads file metadata.
pub(crate) fn last_activity(ws: &Workspace) -> Option<SystemTime> {
    (ws.chats.iter())
        .filter_map(|c| fs::metadata(transcript_path(&c.id)).ok()?.modified().ok())
        .max()
}

pub const PLAN_TO_APPROVE: &str = "Plan to approve";
pub const QUESTION: &str = "Question to answer";

/// What one of the workspace's chats waits on the user for, by how its last turn ended: a plan
/// to approve before a question. `None` while a turn runs. Reads each chat's transcript tail
/// and plans: call it off the UI thread.
pub(crate) fn waiting_on_user(ws: &Workspace) -> Option<&'static str> {
    (ws.chats.iter())
        .filter_map(|c| {
            let plan = c.plan && crate::plan::unapproved(&c.id);
            waiting(&read_tail(&transcript_path(&c.id)), plan)
        })
        .min_by_key(|l| *l != PLAN_TO_APPROVE)
}

/// How a chat's events end: a finished turn, with a plan pending or a question in its last text.
pub(crate) fn waiting(evs: &[Ev], plan: bool) -> Option<&'static str> {
    let (mut text, mut ended) = ("", false);
    for ev in evs {
        match ev {
            Ev::User(_) | Ev::Init { .. } => (text, ended) = ("", false),
            Ev::ToolUse { .. } | Ev::Interrupted => text = "",
            Ev::Text(t) => text = t,
            Ev::Result { is_error, .. } => ended = !is_error,
            _ => {}
        }
    }
    let asks = text.trim_end().trim_end_matches(['*', '_']).ends_with('?');
    if !ended {
        None
    } else if plan {
        Some(PLAN_TO_APPROVE)
    } else {
        asks.then_some(QUESTION)
    }
}

/// The last events of a transcript, enough for its last turn's end.
fn read_tail(path: &Path) -> Vec<Ev> {
    let len = fs::metadata(path).map_or(0, |m| m.len());
    // The result line repeats the final text, so the window holds two copies of it.
    read_range(path, len.saturating_sub(128 * 1024), len, false)
}

/// Events stored in a transcript, up to `len` bytes: lines written after that belong to a
/// process started while this was read, and arrive live.
pub fn read_history(path: &Path, len: u64) -> Vec<Ev> {
    read_range(path, 0, len, true)
}

fn read_range(path: &Path, mut start: u64, end: u64, include_completed_answers: bool) -> Vec<Ev> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let wanted = end.saturating_sub(start);
    loop {
        let mut buf = vec![];
        if let Ok(mut f) = File::open(path) {
            let _ = f.seek(SeekFrom::Start(start));
            let _ = f.take(end.saturating_sub(start)).read_to_end(&mut buf);
        }
        let (mut retained, mut skipped) = (0, false);
        // Read from the middle of the file: the first line is cut, drop it.
        let events = (String::from_utf8_lossy(&buf).lines())
            .skip(usize::from(start > 0))
            .filter_map(|line| {
                let v: Value = serde_json::from_str(line).ok()?;
                // Retried answers must neither change attention nor crowd the last turn out.
                if !include_completed_answers && v["shark_completed_answer"] == true {
                    skipped = true;
                    None
                } else {
                    retained += line.len() as u64 + 1;
                    Some(v)
                }
            })
            .flat_map(parse_record)
            .collect();
        if start == 0 || !skipped || retained >= wanted {
            return events;
        }
        start = end.saturating_sub(end.saturating_sub(start).saturating_mul(2));
    }
}

// Every transcript writer uses this lock; a failed rollback must be recovered before another append.
static TRANSCRIPT_WRITES: std::sync::LazyLock<Mutex<std::collections::HashMap<PathBuf, u64>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

pub(crate) fn append(path: &Option<PathBuf>, line: &str) {
    if let Some(path) = path {
        let _ = append_records(path, &[line], None);
    }
}

pub(crate) fn append_answer(path: &Path, id: &str, records: &[String; 2]) -> std::io::Result<()> {
    append_records(path, &[&records[0], &records[1]], Some(id))
}

/// Whether the transcript already holds the tool result answering question `id`.
fn has_answer(path: &Path, id: &str) -> std::io::Result<bool> {
    Ok(fs::read_to_string(path)?
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|v| {
            v["type"] == "user"
                && v["message"]["content"].as_array().is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
                })
        }))
}

fn append_records(path: &Path, records: &[&str], question: Option<&str>) -> std::io::Result<()> {
    let mut recovery = TRANSCRIPT_WRITES
        .lock()
        .map_err(|_| std::io::Error::other("Transcript writer lock poisoned"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    if let Some(len) = recovery.get(path).copied() {
        file.set_len(len)?;
        file.sync_all()?;
        recovery.remove(path);
    }
    if let Some(id) = question
        && has_answer(path, id)?
    {
        return Ok(());
    }
    let start = file.metadata()?.len();
    let bytes = records.iter().map(|s| format!("{s}\n")).collect::<String>();
    let result = file.write_all(bytes.as_bytes()).and_then(|_| {
        if question.is_some() {
            file.sync_all()
        } else {
            Ok(())
        }
    });
    if result.is_err() {
        recovery.insert(path.to_path_buf(), start);
        if file.set_len(start).and_then(|_| file.sync_all()).is_ok() {
            recovery.remove(path);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const USER_TURN: &str = r#"{"type": "user", "message": {"role": "user", "content": "read a.txt"}, "parent_tool_use_id": null}"#;
    const SHORT_RESULT: &str = r#"{"type": "user", "message": {"role": "user", "content": [{"tool_use_id": "toolu_1", "type": "tool_result", "content": "1\thello\n2\t"}]}, "parent_tool_use_id": null}"#;

    #[test]
    fn forget_removes_transcript_and_log() {
        let id = "forget-test";
        fs::create_dir_all(logs_dir()).unwrap();
        fs::create_dir_all(transcript_path(id).parent().unwrap()).unwrap();
        let files = [transcript_path(id), log_path(id)];
        for f in &files {
            fs::write(f, "x").unwrap();
        }
        forget(id);
        assert!(files.iter().all(|f| !f.exists()));
    }

    #[test]
    fn completed_answer_appends_are_atomic_recoverable_and_deduplicated() {
        let path =
            std::env::temp_dir().join(format!("shark-answer-append-{}.jsonl", std::process::id()));
        fs::write(&path, "").unwrap();
        let pair = [
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"q","name":"AskUserQuestion","input":{}}]}}).to_string(),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"q","content":"redacted"}]}}).to_string(),
        ];
        // A partial append whose rollback previously failed must be removed before any writer runs.
        fs::write(&path, format!("{}\n{{partial", pair[0])).unwrap();
        TRANSCRIPT_WRITES.lock().unwrap().insert(path.clone(), 0);
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let barrier = &barrier;
                let path = &path;
                let pair = &pair;
                scope.spawn(move || {
                    barrier.wait();
                    append_answer(path, "q", pair).unwrap();
                });
            }
            barrier.wait();
            append(
                &Some(path.clone()),
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"reader"}]}}"#,
            );
        });
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains(&format!("{}\n{}\n", pair[0], pair[1])));
        assert_eq!(saved.lines().count(), 3);
        assert!(!saved.contains("partial"));
        append_answer(&path, "q", &pair).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn waiting_on_user_by_how_the_turn_ended() {
        let done = |is_error| Ev::Result {
            is_error,
            text: String::new(),
            cost: 0.,
            secs: 0,
        };
        let turn = |text: &str| vec![Ev::User("go".into()), Ev::Text(text.into()), done(false)];
        assert_eq!(waiting(&turn("Shall I push it?"), false), Some(QUESTION));
        assert_eq!(waiting(&turn("Want **both**?**"), false), Some(QUESTION));
        assert_eq!(waiting(&turn("Done."), false), None);
        assert_eq!(waiting(&turn("Plan's up?"), true), Some(PLAN_TO_APPROVE));
        let mut running = turn("Shall I?");
        running.push(Ev::User("yes".into()));
        assert_eq!(waiting(&running, true), None, "a turn runs");
        let mut follow_up = turn("Continue?");
        follow_up.push(Ev::Init {
            session_id: "s".into(),
            model: "m".into(),
            permission_mode: "p".into(),
            mcp: vec![],
        });
        assert_eq!(waiting(&follow_up, false), None, "an automatic turn runs");
        let failed = vec![Ev::Text("Why?".into()), done(true)];
        assert_eq!(waiting(&failed, false), None);
        let tooled = vec![
            Ev::Text("Look?".into()),
            Ev::ToolUse {
                id: "t".into(),
                name: "Read".into(),
                input: Value::Null,
            },
            done(false),
        ];
        assert_eq!(
            waiting(&tooled, false),
            None,
            "the question wasn't its last word"
        );
    }

    #[test]
    fn read_tail_drops_a_cut_line() {
        let path = std::env::temp_dir().join(format!("shark-tail-{}.jsonl", std::process::id()));
        // The tail starts inside this line; a cut user line must not reset the turn.
        let long = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{}"}}}}"#,
            "x".repeat(160 * 1024)
        );
        let turn = [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Shall I push?"}]}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"","total_cost_usd":0,"duration_ms":1}"#,
        ];
        fs::write(&path, format!("{long}\n{}\n", turn.join("\n"))).unwrap();
        let evs = read_tail(&path);
        assert!(!evs.iter().any(|e| matches!(e, Ev::User(_))), "{evs:?}");
        assert_eq!(waiting(&evs, false), Some(QUESTION));
        // Short files are read whole, first line included.
        fs::write(&path, turn.join("\n")).unwrap();
        assert_eq!(read_tail(&path).len(), 2);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn retried_answers_preserve_attention_for_a_newer_question() {
        let path = std::env::temp_dir().join(format!(
            "shark-answer-attention-{}.jsonl",
            std::process::id()
        ));
        let pair = [
            json!({"type":"assistant","shark_completed_answer":true,"message":{"content":[{"type":"tool_use","id":"old-question","name":"AskUserQuestion","input":{}}]}}).to_string(),
            json!({"type":"user","shark_completed_answer":true,"message":{"content":[{"type":"tool_result","tool_use_id":"old-question","content":"Answered"}]}}).to_string(),
        ];
        fs::create_dir(&path).unwrap();
        assert!(append_answer(&path, "old-question", &pair).is_err());
        fs::remove_dir(&path).unwrap();
        fs::write(
            &path,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Should I proceed?\"}]}}\n",
                "{\"type\":\"result\",\"is_error\":false}\n"
            ),
        )
        .unwrap();
        append_answer(&path, "old-question", &pair).unwrap();
        assert_eq!(waiting(&read_tail(&path), false), Some(QUESTION));
        let history = read_history(&path, u64::MAX);
        assert!(matches!(&history[2], Ev::ToolUse { id, .. } if id == "old-question"));
        assert!(matches!(&history[3], Ev::ToolResult { id, .. } if id == "old-question"));

        // Several recovered answers can occupy the whole tail window.
        for i in 0..8 {
            let id = format!("long-question-{i}");
            let pair = [
                json!({"type":"assistant","shark_completed_answer":true,"message":{"content":[{"type":"tool_use","id":id,"name":"AskUserQuestion","input":{}}]}}).to_string(),
                json!({"type":"user","shark_completed_answer":true,"message":{"content":[{"type":"tool_result","tool_use_id":id,"content":"x".repeat(MAX_OUTPUT)}]}}).to_string(),
            ];
            append_answer(&path, &id, &pair).unwrap();
        }
        assert_eq!(waiting(&read_tail(&path), false), Some(QUESTION));

        // A real tool call still means the earlier question was not the agent's last word.
        append(
            &Some(path.clone()),
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"live","name":"AskUserQuestion","input":{}}]}}"#,
        );
        assert_eq!(waiting(&read_tail(&path), false), None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn clips_stored_tool_output() {
        let long = "é".repeat(MAX_OUTPUT + 5);
        let line = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t", "content": [{"type": "text", "text": long}]}
        ]}})
        .to_string();
        let note = format!("\n… truncated, {} chars total", MAX_OUTPUT + 5);
        let [Ev::ToolResult { text: live, .. }] = &crate::agent::parse(&line)[..] else {
            panic!("{line}");
        };
        let stored = compact(&line);
        let [Ev::ToolResult { text, .. }] = &crate::agent::parse(&stored)[..] else {
            panic!("{stored}");
        };
        assert_eq!(text, live, "replay shows what the live turn did");
        assert_eq!(
            text.strip_suffix(&note).unwrap().chars().count(),
            MAX_OUTPUT
        );
        let short = SHORT_RESULT;
        assert!(
            matches!(compact(short), Cow::Borrowed(_)),
            "short output is stored as-is"
        );
        let write = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t", "name": "Write", "input": {"file_path": "/a", "content": "x".repeat(MAX_INPUT * 3)}}
        ]}})
        .to_string();
        let stored = compact(&write);
        let [Ev::ToolUse { input, .. }] = &crate::agent::parse(&stored)[..] else {
            panic!("{stored}");
        };
        assert_eq!(input["content"].as_str().unwrap().len(), MAX_INPUT);
        assert_eq!(input["file_path"], "/a");
    }

    #[test]
    fn history_stops_at_the_snapshot_length() {
        let path = std::env::temp_dir().join(format!("shark-hist-{}.jsonl", std::process::id()));
        let first = USER_TURN;
        fs::write(&path, format!("{first}\n{first}\n")).unwrap();
        let evs = read_history(&path, first.len() as u64 + 1);
        fs::remove_file(&path).unwrap();
        assert_eq!(evs, vec![Ev::User("read a.txt".into())]);
    }

    #[test]
    fn has_answer_finds_only_the_matching_result() {
        let path =
            std::env::temp_dir().join(format!("shark-has-answer-{}.jsonl", std::process::id()));
        fs::write(&path, format!("{SHORT_RESULT}\nnot json\n")).unwrap();
        assert!(has_answer(&path, "toolu_1").unwrap());
        assert!(!has_answer(&path, "toolu_2").unwrap());
        fs::remove_file(&path).unwrap();
        assert!(has_answer(&path, "toolu_1").is_err());
    }
}
