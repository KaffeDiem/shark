//! Drives `claude -p` over its stream-json protocol, one long-lived process per chat.
use crate::state::{ChatMeta, Workspace, support_dir};
use gpui_kit::base::async_util::{Receiver, unbounded};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

// ponytail: bypass because headless -p denies anything needing approval; add a host permission prompt in M2
const PERMISSION_MODE: &str = "bypassPermissions";

#[derive(Debug, PartialEq)]
pub enum Ev {
    /// The session as the CLI reports it at the start of each turn.
    Init {
        session_id: String,
        model: String,
        permission_mode: String,
        mcp: Vec<String>,
    },
    User(String),
    TextDelta(String),
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        text: String,
        is_error: bool,
    },
    Result {
        is_error: bool,
        text: String,
        cost: f64,
        secs: u64,
    },
    /// Backgrounded tasks (subagents, shells) still running after the turn's result, by description.
    Background(Vec<String>),
    /// The turn was cut short by an `interrupt` control request; its result follows.
    Interrupted,
    /// claude asks whether a tool may run (`--permission-prompt-tool stdio`) and waits for
    /// `allow`/`deny`. Live only: never stored, since an old request can't be answered.
    Permission {
        request_id: String,
        tool: String,
        tool_use_id: String,
        input: Value,
    },
    Exited(String),
}

pub struct Agent {
    pid: u32,
    /// Set once the reader thread has reaped the child, so we never signal a reused pid.
    exited: Arc<AtomicBool>,
    stdin: ChildStdin,
}

/// Characters of tool output kept, in memory and in the transcript; the UI shows no more.
const MAX_OUTPUT: usize = 20_000;

/// Byte offset of the `n`th character, if `s` is longer than that.
fn cut(s: &str, n: usize) -> Option<usize> {
    s.char_indices().nth(n).map(|(i, _)| i)
}

/// A tool result's output as shown: clipped, saying so. `total_chars` marks lines `compact`
/// already clipped.
fn output(b: &Value) -> String {
    let text = text_of(&b["content"]);
    let total = b["total_chars"].as_u64();
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

fn clip_strings(v: &mut Value) -> bool {
    match v {
        Value::String(s) => cut(s, MAX_INPUT).map(|i| s.truncate(i)).is_some(),
        Value::Array(a) => a.iter_mut().fold(false, |c, v| clip_strings(v) | c),
        Value::Object(o) => o.values_mut().fold(false, |c, v| clip_strings(v) | c),
        _ => false,
    }
}

/// The line as stored: tool results and inputs clipped, since the transcript is replayed on
/// every open.
fn compact(line: &str) -> Cow<'_, str> {
    let Ok(mut v) = serde_json::from_str::<Value>(line) else {
        return line.into();
    };
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
            (Some("user"), Some("tool_result")) => {
                let text = text_of(&b["content"]);
                if let Some(i) = cut(&text, MAX_OUTPUT) {
                    b["total_chars"] = text.chars().count().into();
                    b["content"] = text[..i].into();
                    changed = true;
                }
            }
            _ => {}
        }
    }
    match changed {
        true => v.to_string().into(),
        false => line.into(),
    }
}

pub fn transcript_path(chat: &str) -> PathBuf {
    support_dir()
        .join("transcripts")
        .join(format!("{chat}.jsonl"))
}

/// Events stored in a transcript, up to `len` bytes: lines written after that belong to a
/// process started while this was read, and arrive live.
pub fn read_history(path: &PathBuf, len: u64) -> Vec<Ev> {
    use std::io::Read as _;
    let mut buf = vec![];
    if let Ok(f) = File::open(path) {
        let _ = f.take(len).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .flat_map(parse)
        .collect()
}

fn append(path: &Option<PathBuf>, line: &str) {
    if let Some(path) = path
        && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

pub fn spawn(ws: &Workspace, chat: &ChatMeta) -> std::io::Result<(Agent, Receiver<Ev>)> {
    let resume = chat.session_id.iter().flat_map(|id| ["--resume", id]);
    // Tools that need a person (ExitPlanMode, AskUserQuestion) ask us instead of failing.
    let prompts = ["--permission-prompt-tool", "stdio"];
    run(
        ws,
        chat,
        &chat.id,
        prompts.into_iter().chain(resume),
        Some(transcript_path(&chat.id)),
    )
}

const SIDE_PROMPT: &str = "The user is asking a quick side question about this conversation. \
    It is not part of the main task and you have no tools: answer briefly from what is already \
    in the conversation, and say so if you would need to look something up.";

/// A side question: a throwaway fork of the chat's session with no tools, saved nowhere, so the
/// chat itself never sees it. Works while the chat's own turn runs.
pub fn spawn_side(ws: &Workspace, chat: &ChatMeta) -> std::io::Result<(Agent, Receiver<Ev>)> {
    let fork = chat
        .session_id
        .iter()
        .flat_map(|id| ["--resume", id, "--fork-session"]);
    let args = [
        "--tools",
        "",
        "--strict-mcp-config", // no MCP servers either
        "--no-session-persistence",
        "--append-system-prompt",
        SIDE_PROMPT,
    ];
    run(
        ws,
        chat,
        &format!("{}.side", chat.id),
        args.into_iter().chain(fork),
        None,
    )
}

fn run<'a>(
    ws: &Workspace,
    chat: &ChatMeta,
    log_name: &str,
    extra: impl IntoIterator<Item = &'a str>,
    transcript: Option<PathBuf>,
) -> std::io::Result<(Agent, Receiver<Ev>)> {
    let logs = support_dir().join("logs");
    fs::create_dir_all(&logs)?;
    fs::create_dir_all(support_dir().join("transcripts"))?;
    let log = logs.join(format!("{log_name}.stderr.log"));
    // SHARK_CLAUDE swaps in another binary; the UI tests use a fake that swallows stdin.
    let mut cmd = Command::new(std::env::var("SHARK_CLAUDE").unwrap_or("claude".into()));
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ])
    .args([
        "--verbose",
        "--include-partial-messages",
        // Echoes each user message when claude reads it: a message sent mid-turn is queued
        // until then. The echo is what the transcript stores.
        "--replay-user-messages",
        "--permission-mode",
        PERMISSION_MODE,
    ]);
    if let Some(m) = &chat.model {
        cmd.args(["--model", m]);
    }
    cmd.args(extra);
    let mut child = cmd
        .current_dir(ws.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&log)?) // a piped but unread stderr could block the child
        .spawn()?;
    let (tx, rx) = unbounded();
    let stdout = child.stdout.take().unwrap();
    let exited = Arc::new(AtomicBool::new(false));
    let reaped = exited.clone();
    let agent = Agent {
        pid: child.id(),
        exited,
        stdin: child.stdin.take().unwrap(),
    };
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let evs = parse(&line);
            if (evs.iter()).any(|e| !matches!(e, Ev::TextDelta(_) | Ev::Permission { .. })) {
                append(&transcript, &compact(&line)); // deltas are redundant with the full assistant message
            }
            for ev in evs {
                let _ = tx.send_blocking(ev);
            }
        }
        let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
        reaped.store(true, Ordering::SeqCst);
        let err = fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = err.lines().collect();
        let tail = &lines[lines.len().saturating_sub(5)..];
        let _ = tx.send_blocking(Ev::Exited(
            format!("claude exited ({status}) {}", tail.join("\n"))
                .trim()
                .into(),
        ));
    });
    Ok((agent, rx))
}

impl Agent {
    pub fn send(&mut self, text: &str) -> std::io::Result<()> {
        let line = json!({"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": null});
        self.write(&line)
    }

    /// Ends the current turn and keeps the process and its background tasks. claude rejects
    /// the call in flight and sends the turn's result.
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        self.write(&json!({"type": "control_request", "request_id": "interrupt", "request": {"subtype": "interrupt"}}))
    }

    /// `plan`, or back to the mode the process started in. claude switches at once, mid-turn too.
    pub fn set_plan_mode(&mut self, on: bool) -> std::io::Result<()> {
        let mode = if on { "plan" } else { PERMISSION_MODE };
        self.write(&json!({"type": "control_request", "request_id": format!("mode-{mode}"), "request": {"subtype": "set_permission_mode", "mode": mode}}))
    }

    /// Answers an `Ev::Permission`: the tool doesn't run, and claude reads `message` as the
    /// reason.
    pub fn deny(&mut self, request_id: &str, message: &str) -> std::io::Result<()> {
        self.write(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": {"behavior": "deny", "message": message}}}))
    }

    fn write(&mut self, v: &Value) -> std::io::Result<()> {
        self.stdin.write_all(format!("{v}\n").as_bytes())?;
        self.stdin.flush()
    }

    /// SIGTERM ends the turn and background tasks; the next send respawns with `--resume`.
    pub fn stop(&self) {
        if !self.exited.load(Ordering::SeqCst) {
            crate::git::term(self.pid);
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b.as_str().or(b["text"].as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn parse(line: &str) -> Vec<Ev> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    // ponytail: subagent messages are dropped, nest them under their Task tool block when needed
    if !v["parent_tool_use_id"].is_null() {
        return vec![];
    }
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match v["type"].as_str().unwrap_or_default() {
        "system" if v["subtype"] == "init" => vec![Ev::Init {
            session_id: s(&v["session_id"]),
            model: s(&v["model"]),
            permission_mode: s(&v["permissionMode"]),
            mcp: blocks_of(&v["mcp_servers"])
                .map(|m| s(&m["name"]))
                .collect(),
        }],
        "system" if v["subtype"] == "background_tasks_changed" => vec![Ev::Background(
            blocks_of(&v["tasks"])
                .map(|t| match t["description"].as_str() {
                    Some(d) => d.to_string(),
                    None => format!("{} {}", s(&t["task_type"]), s(&t["task_id"])),
                })
                .collect(),
        )],
        "stream_event" if v["event"]["delta"]["type"] == "text_delta" => {
            vec![Ev::TextDelta(s(&v["event"]["delta"]["text"]))]
        }
        "assistant" => blocks(&v)
            .filter_map(|b| match b["type"].as_str()? {
                "text" => Some(Ev::Text(s(&b["text"]))),
                "tool_use" => Some(Ev::ToolUse {
                    id: s(&b["id"]),
                    name: s(&b["name"]),
                    input: b["input"].clone(),
                }),
                _ => None,
            })
            .collect(),
        // Only the user's own messages: claude also replays ones it wrote itself, like
        // background task notifications, and those carry an `origin`.
        "user" if v["message"]["content"].is_string() && v["origin"].is_null() => {
            vec![Ev::User(s(&v["message"]["content"]))]
        }
        "user" => blocks(&v)
            .filter_map(|b| match b["type"].as_str()? {
                "tool_result" => Some(Ev::ToolResult {
                    id: s(&b["tool_use_id"]),
                    text: output(b),
                    is_error: b["is_error"].as_bool().unwrap_or(false),
                }),
                // "[Request interrupted by user]" or "… for tool use]"
                "text" if s(&b["text"]).starts_with("[Request interrupted by user") => {
                    Some(Ev::Interrupted)
                }
                _ => None,
            })
            .collect(),
        "control_request" if v["request"]["subtype"] == "can_use_tool" => vec![Ev::Permission {
            request_id: s(&v["request_id"]),
            tool: s(&v["request"]["tool_name"]),
            tool_use_id: s(&v["request"]["tool_use_id"]),
            input: v["request"]["input"].clone(),
        }],
        "result" => vec![Ev::Result {
            is_error: v["is_error"].as_bool().unwrap_or(false),
            text: if v["result"].is_string() {
                s(&v["result"])
            } else {
                text_of(&v["errors"])
            },
            cost: v["total_cost_usd"].as_f64().unwrap_or(0.0),
            secs: v["duration_ms"].as_u64().unwrap_or(0) / 1000,
        }],
        _ => vec![],
    }
}

fn blocks(v: &Value) -> impl Iterator<Item = &Value> {
    blocks_of(&v["message"]["content"])
}

fn blocks_of(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from claude 2.1.309, trimmed.
    const SESSION: &str = r#"{"type": "system", "subtype": "hook_started", "session_id": "s1"}
{"type": "system", "subtype": "init", "session_id": "s1", "model": "claude-opus-5-5[1m]", "permissionMode": "bypassPermissions", "mcp_servers": [{"name": "mobbin", "status": "pending", "source": "user"}, {"name": "paper", "status": "failed", "source": "user"}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a1","task_type":"local_agent"},{"task_id":"b2","run_id":"r","task_type":"local_bash","description":"Run test suite"}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type": "user", "message": {"role": "user", "content": "read a.txt"}, "parent_tool_use_id": null}
{"type": "user", "message": {"role": "user", "content": "<task-notification>done</task-notification>"}, "origin": {"kind": "task-notification"}, "parent_tool_use_id": null}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/tmp/a.txt"}}]}, "parent_tool_use_id": null}
{"type": "user", "message": {"role": "user", "content": [{"tool_use_id": "toolu_1", "type": "tool_result", "content": "1\thello\n2\t"}]}, "parent_tool_use_id": null}
{"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}, "parent_tool_use_id": null}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "nested"}]}, "parent_tool_use_id": "toolu_9"}
{"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "h"}}, "parent_tool_use_id": null}
{"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{"}}, "parent_tool_use_id": null}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": "x"}, {"type": "text", "text": "hello"}]}, "parent_tool_use_id": null}
{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","display_name":"ExitPlanMode","input":{"plan":"P","planFilePath":"/p.md"},"tool_use_id":"toolu_2","requires_user_interaction":true}}
{"type":"control_response","response":{"subtype":"success","request_id":"mode-plan","response":{"mode":"plan"}}}
{"type":"result","subtype":"success","is_error":false,"result":"hello","total_cost_usd":0.5,"duration_ms":48210,"session_id":"s1"}
{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Invalid API key"],"total_cost_usd":0}
not json"#;

    #[test]
    fn parses_stream_json() {
        let evs: Vec<Ev> = SESSION.lines().flat_map(parse).collect();
        assert_eq!(
            evs,
            vec![
                Ev::Init {
                    session_id: "s1".into(),
                    model: "claude-opus-5-5[1m]".into(),
                    permission_mode: "bypassPermissions".into(),
                    mcp: vec!["mobbin".into(), "paper".into()],
                },
                Ev::Background(vec!["local_agent a1".into(), "Run test suite".into()]),
                Ev::Background(vec![]),
                Ev::User("read a.txt".into()),
                Ev::ToolUse {
                    id: "toolu_1".into(),
                    name: "Read".into(),
                    input: json!({"file_path": "/tmp/a.txt"})
                },
                Ev::ToolResult {
                    id: "toolu_1".into(),
                    text: "1\thello\n2\t".into(),
                    is_error: false
                },
                Ev::Interrupted,
                Ev::TextDelta("h".into()),
                Ev::Text("hello".into()),
                Ev::Permission {
                    request_id: "r1".into(),
                    tool: "ExitPlanMode".into(),
                    tool_use_id: "toolu_2".into(),
                    input: json!({"plan": "P", "planFilePath": "/p.md"}),
                },
                Ev::Result {
                    is_error: false,
                    text: "hello".into(),
                    cost: 0.5,
                    secs: 48
                },
                Ev::Result {
                    is_error: true,
                    text: "Invalid API key".into(),
                    cost: 0.0,
                    secs: 0
                },
            ]
        );
    }

    #[test]
    fn clips_stored_tool_output() {
        let long = "é".repeat(MAX_OUTPUT + 5);
        let line = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t", "content": [{"type": "text", "text": long}]}
        ]}})
        .to_string();
        let note = format!("\n… truncated, {} chars total", MAX_OUTPUT + 5);
        let [Ev::ToolResult { text: live, .. }] = &parse(&line)[..] else {
            panic!("{line}");
        };
        let stored = compact(&line);
        let [Ev::ToolResult { text, .. }] = &parse(&stored)[..] else {
            panic!("{stored}");
        };
        assert_eq!(text, live, "replay shows what the live turn did");
        assert_eq!(
            text.strip_suffix(&note).unwrap().chars().count(),
            MAX_OUTPUT
        );
        let short = SESSION.lines().nth(6).unwrap();
        assert!(
            matches!(compact(short), Cow::Borrowed(_)),
            "short output is stored as-is"
        );
        let write = json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t", "name": "Write", "input": {"file_path": "/a", "content": "x".repeat(MAX_INPUT * 3)}}
        ]}})
        .to_string();
        let stored = compact(&write);
        let [Ev::ToolUse { input, .. }] = &parse(&stored)[..] else {
            panic!("{stored}");
        };
        assert_eq!(input["content"].as_str().unwrap().len(), MAX_INPUT);
        assert_eq!(input["file_path"], "/a");
    }

    #[test]
    fn history_stops_at_the_snapshot_length() {
        let path = std::env::temp_dir().join(format!("shark-hist-{}.jsonl", std::process::id()));
        let first = SESSION.lines().nth(4).unwrap(); // the "read a.txt" user turn
        fs::write(&path, format!("{first}\n{first}\n")).unwrap();
        let evs = read_history(&path, first.len() as u64 + 1);
        fs::remove_file(&path).unwrap();
        assert_eq!(evs, vec![Ev::User("read a.txt".into())]);
    }
}

/// Live check against the real CLI: `cargo test live_claude -- --ignored`. Costs a few cents.
#[cfg(test)]
#[test]
#[ignore]
fn live_claude() {
    use crate::{git, state::Repo};
    let root = std::env::temp_dir().join(format!("shark-live-{}", std::process::id()));
    let mut repos = vec![];
    for (name, lang) in [("api", "Python"), ("web", "TypeScript")] {
        let p = root.join("src").join(name);
        fs::create_dir_all(&p).unwrap();
        git::git(&p, &["init", "-q", "-b", "main"]).unwrap();
        fs::write(
            p.join("CLAUDE.md"),
            format!("The {name} repo is written in {lang}.\n"),
        )
        .unwrap();
        git::git(&p, &["add", "."]).unwrap();
        git::git(
            &p,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "i",
            ],
        )
        .unwrap();
        repos.push(Repo {
            name: name.into(),
            path: p,
        });
    }
    let mut ws = git::create_workspace(&root.join("ws/live"), "live", "live", &repos).unwrap();
    ws.name = format!("live-test-{}", std::process::id());
    std::os::unix::fs::symlink(
        root.join("ws/live"),
        crate::state::workspaces_dir().join(&ws.name),
    )
    .unwrap();
    let (mut agent, rx) = spawn(&ws, &ws.chats[0]).unwrap();
    agent
        .send("Without using any tools: which language is each repo written in? One line.")
        .unwrap();
    let mut text = String::new();
    while let Ok(ev) = rx.recv_blocking() {
        match ev {
            Ev::Text(t) => text += &t,
            Ev::Result { .. } => break,
            Ev::Exited(e) => panic!("{e}"),
            _ => {}
        }
    }
    drop(agent);
    let _ = fs::remove_file(crate::state::workspaces_dir().join(&ws.name));
    let _ = fs::remove_file(transcript_path(&ws.chats[0].id));
    assert!(
        text.contains("Python") && text.contains("TypeScript"),
        "imports not loaded: {text}"
    );
}
