//! Drives `codex app-server` over JSON-RPC on stdio, one long-lived process per chat, signed in
//! with the user's own `codex login` (a ChatGPT subscription or an API key). Its notifications
//! become the same `Ev`s claude's stream-json does; tool items take claude's tool names so the
//! chat summarizes them the same way.
use crate::agent::{self, Agent, Ev, MAX_OUTPUT, clip_strings, cut, output, text_of};
use crate::state::{ChatMeta, Workspace};
use gpui_kit::base::async_util::Receiver;
use serde_json::{Value, json};
use std::{
    process::{ChildStdin, Command},
    sync::{Arc, Mutex},
};

// ponytail: full access to match claude's bypassPermissions; workspace-write with each repo's
// common .git as a writable root when a sandbox is wanted
const SANDBOX: &str = "danger-full-access";

/// A codex model: `codex` (its configured default) or `codex:<model id>`.
pub fn is_codex(model: Option<&str>) -> bool {
    model.is_some_and(|m| m == "codex" || m.starts_with("codex:"))
}

/// The thread messages go to, and the running turn an interrupt names.
#[derive(Default)]
pub struct State {
    thread: Option<String>,
    turn: Option<String>,
    /// Messages sent before the thread exists.
    pending: Vec<String>,
    next: u64,
}

pub fn spawn(ws: &Workspace, chat: &ChatMeta) -> std::io::Result<(Agent, Receiver<Ev>)> {
    // SHARK_CODEX swaps in another binary, like SHARK_CLAUDE.
    let mut cmd = Command::new(std::env::var("SHARK_CODEX").unwrap_or("codex".into()));
    cmd.arg("app-server");
    let st = Arc::new(Mutex::new(State::default()));
    if !ws.dir().join("AGENTS.md").exists() {
        crate::git::write_claude_md(ws); // a workspace from before codex chats
    }
    let (agent, rx) = agent::start(
        cmd,
        ws,
        &chat.id,
        Some(agent::transcript_path(&chat.id)),
        Some(st),
    )?;
    let mut params = json!({"cwd": ws.dir(), "approvalPolicy": "never", "sandbox": SANDBOX});
    if let Some(m) = chat.model.as_deref().and_then(|m| m.strip_prefix("codex:")) {
        params["model"] = m.into();
    }
    // The server takes requests in order, so nothing waits for a reply here; `track` sends
    // the messages once the thread is there.
    let version = env!("CARGO_PKG_VERSION");
    let mut lines = vec![
        json!({"id": "init", "method": "initialize", "params": {"clientInfo": {"name": "shark", "version": version}}}),
        json!({"method": "initialized"}),
    ];
    lines.push(match &chat.session_id {
        Some(id) => {
            params["threadId"] = id.as_str().into();
            params["excludeTurns"] = true.into(); // the transcript has them
            json!({"id": "resume", "method": "thread/resume", "params": params})
        }
        None => json!({"id": "start", "method": "thread/start", "params": params}),
    });
    for l in &lines {
        agent.write_line(l)?;
    }
    Ok((agent, rx))
}

/// Sends what `pending` holds once the thread exists. A `turn/start` while a turn runs joins
/// that turn, as `turn/steer` would, without steer's race against the turn ending.
fn flush(s: &mut State, stdin: &Mutex<ChildStdin>) -> std::io::Result<()> {
    let Some(thread) = s.thread.clone() else {
        return Ok(());
    };
    for text in std::mem::take(&mut s.pending) {
        s.next += 1;
        let input = json!([{"type": "text", "text": text}]);
        agent::write(
            stdin,
            &json!({"id": s.next, "method": "turn/start", "params": {"threadId": thread, "input": input}}),
        )?;
    }
    Ok(())
}

pub fn send(st: &Mutex<State>, stdin: &Mutex<ChildStdin>, text: &str) -> std::io::Result<()> {
    let mut s = st.lock().unwrap();
    s.pending.push(text.into());
    flush(&mut s, stdin)
}

/// Errs before `turn/started` (the server refuses it until then), so the caller stops the
/// process instead.
pub fn interrupt(st: &Mutex<State>, stdin: &Mutex<ChildStdin>) -> std::io::Result<()> {
    let s = st.lock().unwrap();
    match (&s.thread, &s.turn) {
        (Some(thread), Some(turn)) => agent::write(
            stdin,
            &json!({"id": "interrupt", "method": "turn/interrupt", "params": {"threadId": thread, "turnId": turn}}),
        ),
        _ => Err(std::io::Error::other("no turn to interrupt yet")),
    }
}

/// The server requests the chat answers (by denying): approvals, which `approvalPolicy: never`
/// should keep from coming. `track` answers the rest itself.
const APPROVALS: [&str; 2] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
];

/// Declines an approval request.
pub fn deny(stdin: &Mutex<ChildStdin>, request_id: &str) -> std::io::Result<()> {
    let id: Value = serde_json::from_str(request_id).unwrap_or(request_id.into());
    agent::write(stdin, &json!({"id": id, "result": {"decision": "decline"}}))
}

/// Keeps `State` current from the server's lines, on the reader thread. A thread that can't
/// start or resume ends the process, as claude's does: `parse` reports why, and the next
/// message respawns it.
pub fn track(st: &Mutex<State>, stdin: &Mutex<ChildStdin>, pid: u32, line: &str) {
    // Most lines are deltas and hooks: skip them before parsing or locking.
    if !line.contains("\"id\"") && !line.contains("\"turn/") {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let (id, method) = (v["id"].as_str(), v["method"].as_str());
    // A request nothing here can answer would hold up its turn: refuse it.
    if let Some(m) = method
        && !v["id"].is_null()
        && !APPROVALS.contains(&m)
    {
        let reply = match m {
            "mcpServer/elicitation/request" => {
                json!({"id": v["id"], "result": {"action": "decline"}})
            }
            _ => {
                json!({"id": v["id"], "error": {"code": -32601, "message": "not supported by shark"}})
            }
        };
        let _ = agent::write(stdin, &reply);
        return;
    }
    let mut s = st.lock().unwrap();
    match (id, method) {
        (Some("start" | "resume"), _) if v["error"].is_object() => crate::git::term(pid),
        (Some("start" | "resume"), _) => {
            s.thread = v["result"]["thread"]["id"].as_str().map(Into::into);
            let _ = flush(&mut s, stdin);
        }
        (_, Some("turn/started")) => s.turn = v["params"]["turn"]["id"].as_str().map(Into::into),
        (_, Some("turn/completed")) => s.turn = None,
        _ => {}
    }
}

pub fn parse(v: &Value) -> Vec<Ev> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let p = &v["params"];
    let item = &p["item"];
    match (v["method"].as_str(), item["type"].as_str()) {
        (Some("turn/started"), _) => vec![Ev::Init {
            session_id: s(&p["threadId"]),
            model: "codex".into(),
            permission_mode: SANDBOX.into(),
            mcp: vec![],
        }],
        (Some("item/agentMessage/delta"), _) => vec![Ev::TextDelta(s(&p["delta"]))],
        (Some("item/started"), Some("userMessage")) => vec![Ev::User(text_of(&item["content"]))],
        (Some("item/completed"), Some("agentMessage")) => vec![Ev::Text(s(&item["text"]))],
        (Some("item/started"), Some(kind)) => tool_use(kind, item).into_iter().collect(),
        (Some("item/completed"), Some(kind)) if tool_use(kind, item).is_some() => {
            vec![Ev::ToolResult {
                id: s(&item["id"]),
                text: result_text(kind, item),
                is_error: matches!(item["status"].as_str(), Some("failed" | "declined"))
                    || item["exitCode"].as_i64().is_some_and(|c| c != 0),
            }]
        }
        (Some("turn/completed"), _) => {
            let turn = &p["turn"];
            let secs = turn["durationMs"].as_u64().unwrap_or(0) / 1000;
            let result = |is_error, text| Ev::Result {
                is_error,
                text,
                cost: 0.0, // a subscription has no per-turn price
                secs,
            };
            match turn["status"].as_str() {
                Some("interrupted") => vec![Ev::Interrupted, result(false, String::new())],
                Some("failed") => vec![result(true, error_text(&turn["error"]["message"]))],
                _ => vec![result(false, String::new())],
            }
        }
        (Some(method), _) if APPROVALS.contains(&method) => vec![Ev::Permission {
            request_id: v["id"].to_string(),
            tool: method.into(),
            tool_use_id: s(&p["itemId"]),
            input: p.clone(),
        }],
        // The chat resets a session claude reports gone by this prefix.
        (None, _)
            if v["id"] == "resume" && s(&v["error"]["message"]).starts_with("no rollout found") =>
        {
            vec![Ev::Result {
                is_error: true,
                text: format!(
                    "No conversation found with session ID ({})",
                    s(&v["error"]["message"])
                ),
                cost: 0.0,
                secs: 0,
            }]
        }
        // Errors for what the user asked: a message, or the thread. An interrupt that lost
        // the race with its turn's end isn't one.
        (None, _)
            if v["error"].is_object()
                && (v["id"].is_u64() || v["id"] == "start" || v["id"] == "resume") =>
        {
            vec![Ev::Result {
                is_error: true,
                text: s(&v["error"]["message"]),
                cost: 0.0,
                secs: 0,
            }]
        }
        _ => vec![],
    }
}

/// A failed turn's message, which is often the API's JSON error body.
fn error_text(m: &Value) -> String {
    let m = m.as_str().unwrap_or_default();
    serde_json::from_str::<Value>(m)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(Into::into))
        .unwrap_or(m.into())
}

/// A tool item as the claude tool the chat already knows how to summarize.
fn tool_use(kind: &str, item: &Value) -> Option<Ev> {
    let (name, input) = match kind {
        "commandExecution" => ("Bash".into(), json!({"command": item["command"]})),
        "fileChange" => (
            "Edit".into(),
            json!({"file_path": item["changes"][0]["path"]}),
        ),
        "webSearch" => ("WebSearch".into(), json!({"query": item["query"]})),
        "mcpToolCall" => (
            format!(
                "mcp__{}__{}",
                item["server"].as_str().unwrap_or_default(),
                item["tool"].as_str().unwrap_or_default()
            ),
            item["arguments"].clone(),
        ),
        _ => return None,
    };
    Some(Ev::ToolUse {
        id: item["id"].as_str().unwrap_or_default().into(),
        name,
        input,
    })
}

fn result_text(kind: &str, item: &Value) -> String {
    match kind {
        "commandExecution" => output(
            &json!({"content": item["aggregatedOutput"], "total_chars": item["total_chars"]}),
        ),
        "fileChange" => (item["changes"].as_array().into_iter().flatten())
            .map(|c| output(&json!({"content": c["diff"], "total_chars": c["total_chars"]})))
            .collect::<Vec<_>>()
            .join("\n"),
        "mcpToolCall" if item["error"].is_object() => text_of(&item["error"]["message"]),
        "mcpToolCall" => output(&json!({"content": item["result"]["content"]})),
        _ => String::new(),
    }
}

/// Clips a line for the transcript, like `agent::compact`: a finished turn repeats its items,
/// and command output and diffs can be long.
pub fn compact(v: &mut Value) -> bool {
    let mut changed = false;
    if let Some(items) = v.pointer_mut("/params/turn/items")
        && items.as_array().is_some_and(|a| !a.is_empty())
    {
        *items = json!([]);
        changed = true;
    }
    if let Some(item) = v.pointer_mut("/params/item") {
        changed |= clip_output(item, "aggregatedOutput");
        for c in item["changes"].as_array_mut().into_iter().flatten() {
            changed |= clip_output(c, "diff");
        }
        if let Some(x) = item.get_mut("arguments") {
            changed |= clip_strings(x);
        }
    }
    changed
}

/// Cuts `v[key]` to what `output` shows, noting the full length as `output` expects.
fn clip_output(v: &mut Value, key: &str) -> bool {
    let Some(Value::String(out)) = v.get_mut(key) else {
        return false;
    };
    let Some(i) = cut(out, MAX_OUTPUT) else {
        return false;
    };
    let total = out.chars().count();
    out.truncate(i);
    v["total_chars"] = total.into();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    // Recorded from codex-cli 0.164.0 (`codex app-server`), trimmed.
    const SESSION: &str = r#"{"id": "init", "result": {"userAgent": "shark/0.164.0", "codexHome": "/Users/u/.codex"}}
{"error": {"code": -32600, "message": "no rollout found for thread id 0000"}, "id": "resume"}
{"id": "start", "result": {"thread": {"id": "th1", "sessionId": "th1"}, "model": "gpt-6.1-sol"}}
{"method": "hook/started", "params": {"threadId": "th1", "turnId": "t1"}}
{"method": "turn/started", "params": {"threadId": "th1", "turn": {"id": "t1", "items": [], "status": "inProgress"}}}
{"method": "item/started", "params": {"item": {"type": "userMessage", "id": "u1", "content": [{"type": "text", "text": "run echo hi", "text_elements": []}]}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/completed", "params": {"item": {"type": "userMessage", "id": "u1", "content": [{"type": "text", "text": "run echo hi", "text_elements": []}]}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/started", "params": {"item": {"type": "agentMessage", "id": "m1", "text": "", "phase": "commentary"}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/agentMessage/delta", "params": {"threadId": "th1", "turnId": "t1", "itemId": "m1", "delta": "Run"}}
{"method": "item/completed", "params": {"item": {"type": "agentMessage", "id": "m1", "text": "Running it.", "phase": "commentary"}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/started", "params": {"item": {"type": "commandExecution", "id": "exec-1", "command": "/bin/zsh -lc 'echo hi'", "cwd": "/tmp/s", "status": "inProgress", "aggregatedOutput": null, "exitCode": null}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/completed", "params": {"item": {"type": "commandExecution", "id": "exec-1", "command": "/bin/zsh -lc 'echo hi'", "cwd": "/tmp/s", "status": "completed", "aggregatedOutput": "hi\n", "exitCode": 0}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/started", "params": {"item": {"type": "reasoning", "id": "rs1", "summary": [], "content": []}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/started", "params": {"item": {"type": "fileChange", "id": "fc1", "status": "inProgress", "changes": [{"path": "/tmp/s/a.txt", "kind": {"type": "add"}, "diff": "hi\n"}]}, "threadId": "th1", "turnId": "t1"}}
{"method": "item/completed", "params": {"item": {"type": "fileChange", "id": "fc1", "status": "failed", "changes": [{"path": "/tmp/s/a.txt", "kind": {"type": "add"}, "diff": "hi\n"}]}, "threadId": "th1", "turnId": "t1"}}
{"method": "thread/status/changed", "params": {"threadId": "th1", "status": {"type": "idle"}}}
{"method": "turn/completed", "params": {"threadId": "th1", "turn": {"id": "t1", "items": [{"type": "agentMessage", "id": "m1", "text": "Running it."}], "status": "completed", "error": null, "durationMs": 11273}}}
{"method": "turn/completed", "params": {"threadId": "th1", "turn": {"id": "t2", "items": [], "status": "interrupted", "error": null, "durationMs": 4203}}}
{"method": "turn/completed", "params": {"threadId": "th1", "turn": {"id": "t3", "items": [], "status": "failed", "error": {"message": "{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The 'nope' model is not supported when using Codex with a ChatGPT account.\",\"code\":\"model_not_found\"}}", "codexErrorInfo": "other"}, "durationMs": 1000}}}
{"method": "turn/completed", "params": {"threadId": "th1", "turn": {"id": "t5", "items": [], "status": "failed", "error": {"message": "usage limit reached"}, "durationMs": 0}}}
{"id": 7, "method": "item/commandExecution/requestApproval", "params": {"threadId": "th1", "turnId": "t4", "itemId": "exec-2", "command": "rm -rf /"}}
{"id": 8, "method": "mcpServer/elicitation/request", "params": {"threadId": "th1"}}
{"error": {"code": -32600, "message": "no active turn to interrupt"}, "id": "interrupt"}
{"error": {"code": -32600, "message": "config error"}, "id": "resume"}
{"error": {"code": -32600, "message": "bad input"}, "id": 2}"#;

    #[test]
    fn parses_app_server() {
        let evs: Vec<Ev> = SESSION.lines().flat_map(agent::parse).collect();
        let done = |is_error: bool, text: &str, secs| Ev::Result {
            is_error,
            text: text.into(),
            cost: 0.0,
            secs,
        };
        assert_eq!(
            evs,
            vec![
                done(
                    true,
                    "No conversation found with session ID (no rollout found for thread id 0000)",
                    0
                ),
                Ev::Init {
                    session_id: "th1".into(),
                    model: "codex".into(),
                    permission_mode: SANDBOX.into(),
                    mcp: vec![],
                },
                Ev::User("run echo hi".into()),
                Ev::TextDelta("Run".into()),
                Ev::Text("Running it.".into()),
                Ev::ToolUse {
                    id: "exec-1".into(),
                    name: "Bash".into(),
                    input: json!({"command": "/bin/zsh -lc 'echo hi'"}),
                },
                Ev::ToolResult {
                    id: "exec-1".into(),
                    text: "hi\n".into(),
                    is_error: false,
                },
                Ev::ToolUse {
                    id: "fc1".into(),
                    name: "Edit".into(),
                    input: json!({"file_path": "/tmp/s/a.txt"}),
                },
                Ev::ToolResult {
                    id: "fc1".into(),
                    text: "hi\n".into(),
                    is_error: true,
                },
                done(false, "", 11),
                Ev::Interrupted,
                done(false, "", 4),
                done(
                    true,
                    "The 'nope' model is not supported when using Codex with a ChatGPT account.",
                    1
                ),
                done(true, "usage limit reached", 0),
                Ev::Permission {
                    request_id: "7".into(),
                    tool: "item/commandExecution/requestApproval".into(),
                    tool_use_id: "exec-2".into(),
                    input: json!({"threadId": "th1", "turnId": "t4", "itemId": "exec-2", "command": "rm -rf /"}),
                },
                done(true, "config error", 0), // not gone: the session stays
                done(true, "bad input", 0),
            ]
        );
    }

    #[test]
    fn clips_stored_lines() {
        let long = "é".repeat(MAX_OUTPUT + 5);
        let line = json!({"method": "item/completed", "params": {"item": {"type": "commandExecution", "id": "x", "aggregatedOutput": long, "exitCode": 0}}}).to_string();
        let live = agent::parse(&line);
        let stored = agent::compact(&line);
        assert_eq!(
            agent::parse(&stored),
            live,
            "replay shows what the live turn did"
        );
        assert!(stored.contains(&format!("\"total_chars\":{}", MAX_OUTPUT + 5)));
        let diff = "+x\n".repeat(MAX_OUTPUT);
        let line = json!({"method": "item/completed", "params": {"item": {"type": "fileChange", "id": "f", "status": "completed", "changes": [{"path": "/a", "diff": diff}]}}}).to_string();
        let live = agent::parse(&line);
        assert_eq!(
            agent::parse(&agent::compact(&line)),
            live,
            "a long diff replays the same"
        );
        assert!(matches!(&live[..], [Ev::ToolResult { text, .. }] if text.contains("truncated")));
        let done = SESSION.lines().nth(16).unwrap();
        assert!(
            !agent::compact(done).contains("Running it."),
            "a turn's items are dropped"
        );
        let short = SESSION.lines().nth(11).unwrap();
        assert!(matches!(agent::compact(short), Cow::Borrowed(_)));
    }

    #[test]
    fn mixed_history_replays() {
        let claude = r#"{"type": "user", "message": {"role": "user", "content": "hi"}, "parent_tool_use_id": null}"#;
        let codex = SESSION.lines().nth(5).unwrap();
        let path = std::env::temp_dir().join(format!("shark-mixed-{}.jsonl", std::process::id()));
        std::fs::write(&path, format!("{claude}\n{codex}\n")).unwrap();
        let evs = agent::read_history(&path, u64::MAX);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            evs,
            vec![Ev::User("hi".into()), Ev::User("run echo hi".into())]
        );
    }

    #[cfg(feature = "test-support")] // shares the UI tests' home
    const FAKE: &str = r#"#!/bin/sh
while read -r l; do
  case "$l" in
  *'"thread/resume"'*) echo '{"id":"resume","error":{"code":-32600,"message":"no rollout found"}}';;
  *'"thread/start"'*) echo '{"id":"start","result":{"thread":{"id":"th1"}}}';;
  *'"turn/start"'*)
    echo '{"method":"turn/started","params":{"threadId":"th1","turn":{"id":"t1"}}}'
    echo '{"id":9,"method":"mcpServer/elicitation/request","params":{"threadId":"th1"}}'
    read -r reply
    case "$reply" in *'"id":9'*decline*) ;; *) echo "bad reply: $reply" >&2; exit 3;; esac
    echo '{"method":"item/started","params":{"item":{"type":"userMessage","id":"u","content":[{"type":"text","text":"hi"}]}}}'
    echo '{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":"hello"}}}'
    echo '{"method":"turn/completed","params":{"threadId":"th1","turn":{"id":"t1","status":"completed","durationMs":2000}}}';;
  esac
done
"#;

    #[cfg(feature = "test-support")] // shares the UI tests' home
    /// Runs the chat's process against a fake `codex app-server` until it exits or a turn ends.
    fn run_fake(session_id: Option<&str>, tag: &str) -> Vec<Ev> {
        static FAKE_PATH: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        FAKE_PATH.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("shark-fake-codex-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let fake = dir.join("codex");
            std::fs::write(&fake, FAKE).unwrap();
            std::process::Command::new("chmod")
                .arg("+x")
                .arg(&fake)
                .status()
                .unwrap();
            // SAFETY: set once, before any test spawns codex.
            unsafe { std::env::set_var("SHARK_CODEX", &fake) };
        });
        let root = std::env::temp_dir().join(format!("shark-codex-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ws = Workspace {
            name: format!("codex-{tag}-{}", std::process::id()),
            ..Default::default()
        };
        let meta = ChatMeta {
            id: ws.name.clone(),
            model: Some("codex:gpt-6-astra".into()),
            session_id: session_id.map(Into::into),
            ..Default::default()
        };
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(crate::state::workspaces_dir()).unwrap();
        let _ = std::fs::remove_file(ws.dir());
        std::os::unix::fs::symlink(&root, ws.dir()).unwrap();
        let (mut a, rx) = agent::spawn(&ws, &meta).unwrap();
        a.send("hi").unwrap(); // before the thread exists: held until it does
        let mut evs = vec![];
        while let Ok(ev) = rx.recv_blocking() {
            let end = matches!(
                ev,
                Ev::Exited(_)
                    | Ev::Result {
                        is_error: false,
                        ..
                    }
            );
            evs.push(ev);
            if end {
                break;
            }
        }
        drop(a);
        let _ = std::fs::remove_file(ws.dir());
        let _ = std::fs::remove_file(agent::transcript_path(&meta.id));
        evs
    }

    #[cfg(feature = "test-support")] // shares the UI tests' home
    #[test]
    fn runs_a_turn() {
        let evs = run_fake(None, "turn");
        assert_eq!(
            evs,
            vec![
                Ev::Init {
                    session_id: "th1".into(),
                    model: "codex".into(),
                    permission_mode: SANDBOX.into(),
                    mcp: vec![],
                },
                Ev::User("hi".into()),
                Ev::Text("hello".into()),
                Ev::Result {
                    is_error: false,
                    text: String::new(),
                    cost: 0.0,
                    secs: 2,
                },
            ]
        );
    }

    #[cfg(feature = "test-support")] // shares the UI tests' home
    /// A thread that's gone reads as claude's gone session, and the process ends so the next
    /// message starts a new one.
    #[test]
    fn gone_thread_ends_the_process() {
        let evs = run_fake(Some("old"), "gone");
        assert!(
            matches!(&evs[..], [Ev::Result { is_error: true, text, .. }, Ev::Exited(_)]
                if text.starts_with("No conversation found with session ID")),
            "{evs:?}"
        );
    }

    #[test]
    fn detects_codex_models() {
        assert!(is_codex(Some("codex")) && is_codex(Some("codex:gpt-6-astra")));
        assert!(!is_codex(None) && !is_codex(Some("opus")) && !is_codex(Some("codexx")));
    }
}
