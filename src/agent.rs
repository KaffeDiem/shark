//! Drives `claude -p` over its stream-json protocol, one long-lived process per chat. Chats on a
//! codex model run `codex app-server` instead (`crate::codex`), mapped onto the same `Ev`s.
use crate::codex;
use crate::state::{ChatMeta, Workspace, support_dir};
use gpui_kit::base::async_util::{Receiver, unbounded};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
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
    /// A plan codex's plan mode proposed. claude's come as an ExitPlanMode `Permission`.
    Plan {
        id: String,
        text: String,
    },
    Exited(String),
}

pub struct Agent {
    pid: u32,
    /// Set once the reader thread has reaped the child, so we never signal a reused pid.
    exited: Arc<AtomicBool>,
    /// Shared with the reader thread, which answers codex's handshake.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Set for a `codex app-server` process.
    codex: Option<Arc<Mutex<codex::State>>>,
}

/// Characters of tool output kept, in memory and in the transcript; the UI shows no more.
pub(crate) const MAX_OUTPUT: usize = 20_000;

/// Byte offset of the `n`th character, if `s` is longer than that.
pub(crate) fn cut(s: &str, n: usize) -> Option<usize> {
    s.char_indices().nth(n).map(|(i, _)| i)
}

/// A tool result's output as shown: clipped, saying so. `total_chars` marks lines `compact`
/// already clipped.
pub(crate) fn output(b: &Value) -> String {
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

fn logs_dir() -> PathBuf {
    support_dir().join("logs")
}

pub fn transcript_path(chat: &str) -> PathBuf {
    support_dir()
        .join("transcripts")
        .join(format!("{chat}.jsonl"))
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
fn waiting(evs: &[Ev], plan: bool) -> Option<&'static str> {
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

fn append(path: &Option<PathBuf>, line: &str) {
    if let Some(path) = path {
        let _ = append_records(path, &[line], None);
    }
}

pub(crate) fn append_answer(path: &Path, id: &str, records: &[String; 2]) -> std::io::Result<()> {
    append_records(path, &[&records[0], &records[1]], Some(id))
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
    if let Some(id) = question {
        let stored = fs::read_to_string(path)?;
        if stored
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|v| {
                v["type"] == "user"
                    && v["message"]["content"].as_array().is_some_and(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
                    })
            })
        {
            return Ok(());
        }
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

pub fn spawn(ws: &Workspace, chat: &ChatMeta) -> std::io::Result<(Agent, Receiver<Ev>)> {
    if codex::is_codex(chat.model.as_deref()) {
        return codex::spawn(ws, chat);
    }
    let resume = chat.session_id.iter().flat_map(|id| ["--resume", id]);
    // Tools that need a person (ExitPlanMode, AskUserQuestion) ask us instead of failing.
    let prompts = ["--permission-prompt-tool", "stdio"];
    run(
        &ws.dir(),
        model_of(chat),
        &chat.id,
        prompts.into_iter().chain(effort(chat)).chain(resume),
        Some(transcript_path(&chat.id)),
    )
}

fn model_of(chat: &ChatMeta) -> &str {
    chat.model.as_deref().unwrap_or(crate::chat::MODELS[0])
}

fn effort(chat: &ChatMeta) -> [&'static str; 2] {
    ["--effort", crate::chat::effort_of(chat)]
}

const SIDE_PROMPT: &str = "The user is asking a quick side question about this conversation. \
    It is not part of the main task and you have no tools: answer briefly from what is already \
    in the conversation, and say so if you would need to look something up.";

/// A side question: a throwaway fork of the chat's session with no tools, saved nowhere, so the
/// chat itself never sees it. Works while the chat's own turn runs.
pub fn spawn_side(ws: &Workspace, chat: &ChatMeta) -> std::io::Result<(Agent, Receiver<Ev>)> {
    if codex::is_codex(chat.model.as_deref()) {
        return Err(std::io::Error::other("side questions need a Claude model"));
    }
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
        &ws.dir(),
        model_of(chat),
        &format!("{}.side", chat.id),
        args.into_iter().chain(effort(chat)).chain(fork),
        None,
    )
}

/// Shark's user docs, for answering questions about the app itself.
pub const README: &str = include_str!("../README.md");

/// A one-shot question about Shark, outside any workspace: no tools, no MCP, saved nowhere.
/// `system` is appended to claude's system prompt (e.g. `README` and the keymap).
pub fn spawn_ask(model: &str, system: &str) -> std::io::Result<(Agent, Receiver<Ev>)> {
    let args = [
        "--tools",
        "",
        "--strict-mcp-config",
        // No user hooks, skills, plugins or CLAUDE.md (`claude --help`, 2.1.309).
        // Not `--setting-sources ""`: that also drops auth set in settings.json (apiKeyHelper,
        // env), which --safe-mode keeps.
        "--safe-mode",
        "--no-session-persistence",
        "--append-system-prompt",
        system,
    ];
    run(&support_dir(), model, "ask", args, None)
}

fn run<'a>(
    cwd: &Path,
    model: &str,
    log_name: &str,
    extra: impl IntoIterator<Item = &'a str>,
    transcript: Option<PathBuf>,
) -> std::io::Result<(Agent, Receiver<Ev>)> {
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
        "--model",
        model,
    ]);
    cmd.args(extra);
    start(cmd, cwd, log_name, transcript, None)
}

/// Starts `cmd` in `cwd` and turns its stdout lines into `Ev`s, storing them in
/// `transcript`. `codex` holds a `codex app-server`'s state, which the reader keeps current.
pub(crate) fn start(
    mut cmd: Command,
    cwd: &Path,
    log_name: &str,
    transcript: Option<PathBuf>,
    codex: Option<Arc<Mutex<codex::State>>>,
) -> std::io::Result<(Agent, Receiver<Ev>)> {
    let logs = logs_dir();
    fs::create_dir_all(&logs)?;
    fs::create_dir_all(support_dir().join("transcripts"))?;
    let log = logs.join(format!("{log_name}.stderr.log"));
    let mut child = cmd
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&log)?) // a piped but unread stderr could block the child
        .spawn()?;
    let (tx, rx) = unbounded();
    let stdout = child.stdout.take().unwrap();
    let exited = Arc::new(AtomicBool::new(false));
    let reaped = exited.clone();
    let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
    let agent = Agent {
        pid: child.id(),
        exited,
        stdin: stdin.clone(),
        codex: codex.clone(),
    };
    let (name, pid) = (if codex.is_some() { "codex" } else { "claude" }, child.id());
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(st) = &codex
                && !codex::track(st, &stdin, pid, &line)
            {
                continue;
            }
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
            format!("{name} exited ({status}) {}", tail.join("\n"))
                .trim()
                .into(),
        ));
    });
    Ok((agent, rx))
}

impl Agent {
    pub fn send(&mut self, text: &str) -> std::io::Result<()> {
        if let Some(st) = &self.codex {
            return codex::send(st, &self.stdin, text);
        }
        let line = json!({"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": null});
        self.write(&line)
    }

    /// Ends the current turn and keeps the process and its background tasks. claude rejects
    /// the call in flight and sends the turn's result.
    pub fn interrupt(&mut self) -> std::io::Result<()> {
        if let Some(st) = &self.codex {
            return codex::interrupt(st, &self.stdin);
        }
        self.write(&json!({"type": "control_request", "request_id": "interrupt", "request": {"subtype": "interrupt"}}))
    }

    /// `plan`, or back to the mode the process started in. claude switches at once, mid-turn
    /// too; codex from the next message.
    pub fn set_plan_mode(&mut self, on: bool) -> std::io::Result<()> {
        if let Some(st) = &self.codex {
            st.lock().unwrap().plan = on;
            return Ok(());
        }
        let mode = if on { "plan" } else { PERMISSION_MODE };
        self.write(&json!({"type": "control_request", "request_id": format!("mode-{mode}"), "request": {"subtype": "set_permission_mode", "mode": mode}}))
    }

    /// codex's Fast service tier, from the next message. claude has none: a chat switched to
    /// codex takes it from its meta when it respawns.
    pub fn set_fast(&mut self, on: bool) {
        if let Some(st) = &self.codex {
            st.lock().unwrap().fast = Some(on);
        }
    }

    /// codex's thinking level, from the next message. claude takes `--effort` only at start, so
    /// the chat restarts it instead.
    pub fn set_effort(&mut self, level: &str) {
        if let Some(st) = &self.codex {
            st.lock().unwrap().effort = level.into();
        }
    }

    /// Answers an `Ev::Permission`: the tool doesn't run, and claude reads `message` as the
    /// reason.
    pub fn deny(&mut self, request_id: &str, message: &str) -> std::io::Result<()> {
        if self.codex.is_some() {
            return codex::deny(&self.stdin, request_id);
        }
        self.write(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": {"behavior": "deny", "message": message}}}))
    }

    /// Answers an `Ev::Permission` by letting the tool run: claude's with `input` in place of
    /// its own (AskUserQuestion's plus `answers`), codex's with `input` as the request's result.
    pub fn allow(&mut self, request_id: &str, input: Value) -> std::io::Result<()> {
        if self.codex.is_some() {
            return codex::reply(&self.stdin, request_id, input);
        }
        self.write(&json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": {"behavior": "allow", "updatedInput": input}}}))
    }

    fn write(&mut self, v: &Value) -> std::io::Result<()> {
        write(&self.stdin, v)
    }

    pub(crate) fn write_line(&self, v: &Value) -> std::io::Result<()> {
        write(&self.stdin, v)
    }

    /// SIGTERM ends the turn and background tasks; the next send respawns with `--resume`.
    pub fn stop(&self) {
        if !self.exited.load(Ordering::SeqCst) {
            crate::git::term(self.pid);
        }
    }
}

pub(crate) fn write(stdin: &Mutex<ChildStdin>, v: &Value) -> std::io::Result<()> {
    let mut stdin = stdin.lock().unwrap();
    stdin.write_all(format!("{v}\n").as_bytes())?;
    stdin.flush()
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn text_of(v: &Value) -> String {
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
    serde_json::from_str(line)
        .map(parse_record)
        .unwrap_or_default()
}

fn parse_record(v: Value) -> Vec<Ev> {
    // claude's lines all carry a `type`; codex's JSON-RPC never does.
    if v["type"].is_null() {
        return codex::parse(&v);
    }
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
    live(None);
}

/// The same through `codex app-server` and its `codex login`: `cargo test live_codex -- --ignored`.
#[cfg(test)]
#[test]
#[ignore]
fn live_codex() {
    live(Some("codex:gpt-6.1-sol"));
}

/// Each repo's instructions reach the agent through the workspace's root docs.
#[cfg(test)]
fn live(model: Option<&str>) {
    use crate::{git, state::Repo};
    let tag = model.map_or("claude", |_| "codex");
    let root = std::env::temp_dir().join(format!("shark-live-{tag}-{}", std::process::id()));
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
    let on = git::on_branch(&repos, "live");
    let mut ws = git::create_workspace(&root.join("ws/live"), "live", "live", &on).unwrap();
    ws.name = format!("live-test-{tag}-{}", std::process::id());
    ws.chats[0].model = model.map(Into::into);
    ws.chats[0].id = ws.name.clone(); // the default id would be a real chat's transcript
    std::os::unix::fs::symlink(
        root.join("ws/live"),
        crate::state::workspaces_dir().join(&ws.name),
    )
    .unwrap();
    let (mut agent, rx) = spawn(&ws, &ws.chats[0]).unwrap();
    // claude has the repos' files imported; codex follows AGENTS.md's pointers to read them.
    let how = match model {
        None => "Without using any tools",
        Some(_) => "Reading only what your instructions point you to",
    };
    agent
        .send(&format!(
            "{how}: which language is each repo written in? One line."
        ))
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
    let _ = fs::remove_file(logs_dir().join(format!("{}.stderr.log", ws.chats[0].id)));
    assert!(
        text.contains("Python") && text.contains("TypeScript"),
        "imports not loaded: {text}"
    );
}
