//! Drives `codex app-server` over JSON-RPC on stdio, one long-lived process per chat, signed in
//! with the user's own `codex login` (a ChatGPT subscription or an API key). Its notifications
//! become the same `Ev`s claude's stream-json does; tool items take claude's tool names so the
//! chat summarizes them the same way.
use crate::agent::{self, Agent, Ev, MAX_OUTPUT, clip_strings, cut, output, text_of};
use crate::chat::ASK_IN_TEXT;
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

/// The thread messages go to, the running turn an interrupt names, and the settings each
/// `turn/start` carries.
#[derive(Default)]
pub struct State {
    thread: Option<String>,
    turn: Option<String>,
    /// Messages sent before the thread exists.
    pending: Vec<String>,
    next: u64,
    /// Plan mode: codex's `plan` collaboration mode, in a read-only sandbox.
    pub plan: bool,
    /// The Fast service tier, on or off; `None` leaves the thread on what codex's config says.
    pub fast: Option<bool>,
    /// The thread's model and reasoning effort, which a collaboration mode must name: a null
    /// effort would drop the configured one.
    model: String,
    effort: Value,
}

pub fn spawn(ws: &Workspace, chat: &ChatMeta) -> std::io::Result<(Agent, Receiver<Ev>)> {
    // SHARK_CODEX swaps in another binary, like SHARK_CLAUDE.
    let mut cmd = Command::new(std::env::var("SHARK_CODEX").unwrap_or("codex".into()));
    cmd.arg("app-server");
    let st = Arc::new(Mutex::new(State {
        plan: chat.plan,
        fast: chat.fast,
        ..Default::default()
    }));
    if !ws.dir().join("AGENTS.md").exists() {
        crate::git::write_claude_md(ws); // a workspace from before codex chats
    }
    let (agent, rx) = agent::start(
        cmd,
        &ws.dir(),
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
        json!({"id": "init", "method": "initialize", "params": {"clientInfo": {"name": "shark", "version": version}, "capabilities": {"experimentalApi": true}}}),
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
        agent::write(
            stdin,
            &json!({"id": s.next, "method": "turn/start", "params": turn_params(&s, &thread, &text)}),
        )?;
    }
    Ok(())
}

/// A `turn/start`'s params. Mode, sandbox and tier hold for later turns too; sent with a
/// message that joins a running turn, they take effect from the next one.
fn turn_params(s: &State, thread: &str, text: &str) -> Value {
    let (mode, sandbox) = match s.plan {
        true => ("plan", json!({"type": "readOnly", "networkAccess": false})),
        false => ("default", json!({"type": "dangerFullAccess"})),
    };
    // A null developer_instructions picks the mode's built-in prompt.
    let settings =
        json!({"model": s.model, "reasoning_effort": s.effort, "developer_instructions": null});
    let mut p = json!({
        "threadId": thread,
        "input": [{"type": "text", "text": text}],
        "collaborationMode": {"mode": mode, "settings": settings},
        "sandboxPolicy": sandbox,
    });
    if let Some(fast) = s.fast {
        p["serviceTier"] = if fast { "priority" } else { "default" }.into();
    }
    p
}

pub fn send(st: &Mutex<State>, stdin: &Mutex<ChildStdin>, text: &str) -> std::io::Result<()> {
    let mut s = st.lock().unwrap();
    s.pending.push(text.into());
    flush(&mut s, stdin)
}

/// Whether codex's config puts threads on the Fast tier: what a chat gets until it toggles.
// ponytail: top-level `service_tier` only; a profile or `-c` override isn't seen
pub fn fast_by_config() -> bool {
    let home = std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from);
    let home = home.unwrap_or_else(|| crate::state::home().join(".codex"));
    fast_in(&std::fs::read_to_string(home.join("config.toml")).unwrap_or_default())
}

fn fast_in(config: &str) -> bool {
    (config.lines())
        .map(str::trim)
        .take_while(|l| !l.starts_with('['))
        .filter_map(|l| {
            l.strip_prefix("service_tier")?
                .trim_start()
                .strip_prefix('=')
        })
        .any(|v| matches!(v.trim().trim_matches(['"', '\'']), "fast" | "priority"))
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
/// message respawns it. False for a sub-agent's line, which the chat doesn't show or store.
pub fn track(st: &Mutex<State>, stdin: &Mutex<ChildStdin>, pid: u32, line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return true;
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
            // Plan mode asks its questions this way; claude's AskUserQuestion gets the same.
            "item/tool/requestUserInput" => {
                let answers: serde_json::Map<String, Value> = (v["params"]["questions"].as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|q| q["id"].as_str())
                    .map(|id| (id.into(), json!({"answers": [ASK_IN_TEXT]})))
                    .collect();
                json!({"id": v["id"], "result": {"answers": answers}})
            }
            _ => {
                json!({"id": v["id"], "error": {"code": -32601, "message": "not supported by shark"}})
            }
        };
        let _ = agent::write(stdin, &reply);
        return true;
    }
    let mut s = st.lock().unwrap();
    // Sub-agents run on threads of their own in the same server: their turns starting and
    // ending aren't the chat's, which goes on until its own thread's turn completes.
    if let (Some(t), Some(main)) = (v["params"]["threadId"].as_str(), &s.thread)
        && t != main
    {
        if !v["id"].is_null() {
            let _ = deny(stdin, &v["id"].to_string()); // an approval no one would see
        }
        return false;
    }
    match (id, method) {
        (Some("start" | "resume"), _) if v["error"].is_object() => crate::git::term(pid),
        (Some("start" | "resume"), _) => {
            let r = &v["result"];
            s.thread = r["thread"]["id"].as_str().map(Into::into);
            s.model = r["model"].as_str().unwrap_or_default().into();
            s.effort = r["reasoningEffort"].clone();
            let _ = flush(&mut s, stdin);
        }
        (_, Some("turn/started")) => s.turn = v["params"]["turn"]["id"].as_str().map(Into::into),
        (_, Some("turn/completed")) => s.turn = None,
        _ => {}
    }
    true
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
        // Plan mode's `<proposed_plan>`, cut out of the reply: a row like claude's
        // ExitPlanMode, then a version for the plan pane. The row comes first because the
        // reply after the plan streams before the plan is complete.
        (Some("item/started"), Some("plan")) => vec![Ev::ToolUse {
            id: s(&item["id"]),
            name: "ExitPlanMode".into(),
            input: json!({"plan": ""}),
        }],
        (Some("item/completed"), Some("plan")) => {
            let (id, text) = (s(&item["id"]), s(&item["text"]));
            vec![
                Ev::ToolResult {
                    id: id.clone(),
                    text: String::new(),
                    is_error: false,
                },
                Ev::Plan { id, text },
            ]
        }
        // A sub-agent, as the row claude's Agent tool gets, open until the sub-agent ends.
        // Its own thread's lines don't reach the chat (`track`).
        (Some("item/completed"), Some("subAgentActivity")) => {
            let id = s(&item["agentThreadId"]);
            match item["kind"].as_str() {
                Some("started") => vec![Ev::ToolUse {
                    id,
                    name: "Agent".into(),
                    input: json!({"description": sub_agent_name(&s(&item["agentPath"]))}),
                }],
                Some(kind @ ("completed" | "interrupted")) => vec![Ev::ToolResult {
                    id,
                    text: String::new(),
                    is_error: kind == "interrupted",
                }],
                _ => vec![],
            }
        }
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

/// `/root/wait_twenty` → `wait twenty`.
fn sub_agent_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or_default()
        .replace('_', " ")
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
    const SESSION: &str = r##"{"id": "init", "result": {"userAgent": "shark/0.164.0", "codexHome": "/Users/u/.codex"}}
{"error": {"code": -32600, "message": "no rollout found for thread id 0000"}, "id": "resume"}
{"id": "start", "result": {"thread": {"id": "th1", "sessionId": "th1"}, "model": "gpt-6.1-sol", "reasoningEffort": "high", "serviceTier": "priority"}}
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
{"error": {"code": -32600, "message": "bad input"}, "id": 2}
{"id": "resume", "result": {"thread": {"id": "th1"}, "model": "gpt-6.1-sol", "serviceTier": null}}
{"method": "item/started", "params": {"item": {"type": "plan", "id": "t6-plan", "text": ""}, "threadId": "th1", "turnId": "t6"}}
{"method": "item/plan/delta", "params": {"threadId": "th1", "turnId": "t6", "itemId": "t6-plan", "delta": "#"}}
{"method": "item/tool/requestUserInput", "id": 0, "params": {"threadId": "th1", "turnId": "t6", "itemId": "call_1", "questions": [{"id": "filename", "header": "Filename", "question": "Which file?", "isOther": true, "isSecret": false, "options": [{"label": "hello.txt", "description": ""}]}], "isBlocking": true}}
{"method": "item/completed", "params": {"item": {"type": "plan", "id": "t6-plan", "text": "# Add hello.txt\n\n1. Create it.\n"}, "threadId": "th1", "turnId": "t6"}}
{"method": "item/started", "params": {"item": {"type": "subAgentActivity", "model": "gpt-6.1-sol", "id": "call_9", "kind": "started", "agentThreadId": "sub1", "agentPath": "/root/wait_twenty"}, "threadId": "th1", "turnId": "t7"}}
{"method": "item/completed", "params": {"item": {"type": "subAgentActivity", "model": "gpt-6.1-sol", "id": "call_9", "kind": "started", "agentThreadId": "sub1", "agentPath": "/root/wait_twenty"}, "threadId": "th1", "turnId": "t7"}}
{"method": "item/started", "params": {"item": {"type": "collabAgentToolCall", "id": "call_a", "tool": "wait", "status": "inProgress", "senderThreadId": "th1", "receiverThreadIds": [], "agentsStates": {}}, "threadId": "th1", "turnId": "t7"}}
{"method": "item/completed", "params": {"item": {"type": "subAgentActivity", "model": null, "id": "subagent-completed-x", "kind": "completed", "agentThreadId": "sub1", "agentPath": "/root/wait_twenty"}, "threadId": "th1", "turnId": "t7"}}"##;

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
                // A plan's row as it starts; its text, in deltas, counts once complete.
                Ev::ToolUse {
                    id: "t6-plan".into(),
                    name: "ExitPlanMode".into(),
                    input: json!({"plan": ""}),
                },
                Ev::ToolResult {
                    id: "t6-plan".into(),
                    text: String::new(),
                    is_error: false,
                },
                Ev::Plan {
                    id: "t6-plan".into(),
                    text: "# Add hello.txt\n\n1. Create it.\n".into(),
                },
                // A sub-agent's row, by its thread, from its start to its end.
                Ev::ToolUse {
                    id: "sub1".into(),
                    name: "Agent".into(),
                    input: json!({"description": "wait twenty"}),
                },
                Ev::ToolResult {
                    id: "sub1".into(),
                    text: String::new(),
                    is_error: false,
                },
            ]
        );
    }

    /// Each `turn/start` names the mode, sandbox and tier, and the thread's model and effort.
    #[test]
    fn turns_carry_plan_mode_and_fast() {
        let mut s = State {
            model: "gpt-6.1-sol".into(),
            effort: json!("high"),
            ..Default::default()
        };
        let p = turn_params(&s, "th1", "hi");
        assert_eq!(p["threadId"], "th1");
        assert_eq!(p["input"], json!([{"type": "text", "text": "hi"}]));
        assert_eq!(
            p["collaborationMode"],
            json!({"mode": "default", "settings": {"model": "gpt-6.1-sol", "reasoning_effort": "high", "developer_instructions": null}})
        );
        assert_eq!(p["sandboxPolicy"], json!({"type": "dangerFullAccess"}));
        assert!(p.get("serviceTier").is_none(), "codex's config decides");
        s.plan = true;
        s.fast = Some(true);
        let p = turn_params(&s, "th1", "hi");
        assert_eq!(p["collaborationMode"]["mode"], "plan");
        assert_eq!(
            p["collaborationMode"]["settings"]["reasoning_effort"],
            "high"
        );
        assert_eq!(
            p["sandboxPolicy"],
            json!({"type": "readOnly", "networkAccess": false}),
            "plan mode edits nothing"
        );
        assert_eq!(p["serviceTier"], "priority");
        s.fast = Some(false);
        assert_eq!(turn_params(&s, "th1", "hi")["serviceTier"], "default");
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
    const FAKE: &str = r##"#!/bin/sh
while read -r l; do
  case "$l" in
  *'"initialize"'*) case "$l" in *'"experimentalApi":true'*) ;; *) echo "no experimentalApi: $l" >&2; exit 4;; esac;;
  *'"thread/resume"'*) echo '{"id":"resume","error":{"code":-32600,"message":"no rollout found"}}';;
  *'"thread/start"'*) echo '{"id":"start","result":{"thread":{"id":"th1"},"model":"m1","reasoningEffort":"high","serviceTier":"priority"}}';;
  *'"turn/start"'*'"mode":"plan"'*)
    for want in '"readOnly"' '"serviceTier":"default"' '"model":"m1"' '"reasoning_effort":"high"'; do
      case "$l" in *"$want"*) ;; *) echo "plan turn lacks $want: $l" >&2; exit 5;; esac
    done
    echo '{"method":"turn/started","params":{"threadId":"th1","turn":{"id":"t2"}}}'
    echo '{"id":10,"method":"item/tool/requestUserInput","params":{"threadId":"th1","questions":[{"id":"q1","question":"Which?"}]}}'
    read -r reply
    case "$reply" in *'"id":10'*'"q1"'*'Ask in plain'*) ;; *) echo "bad answer: $reply" >&2; exit 6;; esac
    echo '{"method":"item/started","params":{"item":{"type":"plan","id":"t2-plan","text":""}}}'
    echo '{"method":"item/completed","params":{"item":{"type":"plan","id":"t2-plan","text":"# P"}}}'
    echo '{"method":"turn/completed","params":{"threadId":"th1","turn":{"id":"t2","status":"completed","durationMs":1000}}}';;
  *'"turn/start"'*)
    echo '{"method":"turn/started","params":{"threadId":"th1","turn":{"id":"t1"}}}'
    echo '{"id":9,"method":"mcpServer/elicitation/request","params":{"threadId":"th1"}}'
    read -r reply
    case "$reply" in *'"id":9'*decline*) ;; *) echo "bad reply: $reply" >&2; exit 3;; esac
    echo '{"method":"item/started","params":{"item":{"type":"userMessage","id":"u","content":[{"type":"text","text":"hi"}]}}}'
    echo '{"method":"item/completed","params":{"item":{"type":"subAgentActivity","id":"c","kind":"started","agentThreadId":"sub1","agentPath":"/root/helper"}}}'
    echo '{"method":"turn/started","params":{"threadId":"sub1","turn":{"id":"s1"}}}'
    echo '{"method":"item/completed","params":{"threadId":"sub1","item":{"type":"agentMessage","id":"sm","text":"sub done"}}}'
    echo '{"method":"turn/completed","params":{"threadId":"sub1","turn":{"id":"s1","status":"completed","durationMs":1000}}}'
    echo '{"method":"item/completed","params":{"item":{"type":"subAgentActivity","id":"d","kind":"completed","agentThreadId":"sub1","agentPath":"/root/helper"}}}'
    echo '{"method":"item/completed","params":{"item":{"type":"agentMessage","id":"m","text":"hello"}}}'
    echo '{"method":"turn/completed","params":{"threadId":"th1","turn":{"id":"t1","status":"completed","durationMs":2000}}}';;
  esac
done
"##;

    #[cfg(feature = "test-support")] // shares the UI tests' home
    /// Runs the chat's process against a fake `codex app-server` until it exits or a turn ends.
    fn run_fake(session_id: Option<&str>, tag: &str, plan: bool) -> Vec<Ev> {
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
            plan,
            fast: plan.then_some(false),
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
    /// A sub-agent's turn, on its own thread, neither ends the chat's turn nor shows in it
    /// beyond its row.
    #[test]
    fn runs_a_turn() {
        let evs = run_fake(None, "turn", false);
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
                Ev::ToolUse {
                    id: "sub1".into(),
                    name: "Agent".into(),
                    input: json!({"description": "helper"}),
                },
                Ev::ToolResult {
                    id: "sub1".into(),
                    text: String::new(),
                    is_error: false,
                },
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
        let evs = run_fake(Some("old"), "gone", false);
        assert!(
            matches!(&evs[..], [Ev::Result { is_error: true, text, .. }, Ev::Exited(_)]
                if text.starts_with("No conversation found with session ID")),
            "{evs:?}"
        );
    }

    #[cfg(feature = "test-support")] // shares the UI tests' home
    /// A chat in plan mode with Fast off starts its turns that way, and plan mode's question
    /// is answered so the turn goes on to its plan.
    #[test]
    fn plans_a_turn() {
        let evs = run_fake(None, "plan", true);
        assert!(
            matches!(&evs[..], [Ev::Init { .. }, Ev::ToolUse { name, .. }, Ev::ToolResult { .. }, Ev::Plan { id, text }, Ev::Result { is_error: false, .. }]
                if name == "ExitPlanMode" && id == "t2-plan" && text == "# P"),
            "{evs:?}"
        );
    }

    #[test]
    fn reads_fast_from_config() {
        assert!(fast_in("model = \"m\"\nservice_tier = \"fast\"\n"));
        assert!(fast_in("service_tier='priority'"));
        assert!(!fast_in("service_tier = \"flex\""));
        assert!(!fast_in(""));
        assert!(
            !fast_in("[profiles.x]\nservice_tier = \"fast\""),
            "a profile's isn't the default"
        );
    }

    #[test]
    fn detects_codex_models() {
        assert!(is_codex(Some("codex")) && is_codex(Some("codex:gpt-6-astra")));
        assert!(!is_codex(None) && !is_codex(Some("opus")) && !is_codex(Some("codexx")));
    }
}
