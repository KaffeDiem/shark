//! Drives `claude -p` over its stream-json protocol, one long-lived process per chat. Chats on a
//! codex model run `codex app-server` instead (`crate::codex`), mapped onto the same `Ev`s.
use crate::codex;
use crate::state::{ChatMeta, Task, support_dir};
use crate::transcript::{
    append, compact, log_path, logs_dir, output, transcript_path, transcripts_dir,
};
use gpui_kit::base::async_util::{Receiver, unbounded};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

// ponytail: bypass because headless -p denies anything needing approval; add a host permission prompt in M2
const PERMISSION_MODE: &str = "bypassPermissions";

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundTask {
    pub id: String,
    pub kind: String,
    pub description: String,
}

/// Told to the agent when the user closes the card (or sends a message instead).
pub(crate) const ANSWER_IN_CHAT: &str = "The user closed these questions to answer in the chat \
    instead. End your turn now without assuming any answers; their reply comes next.";

#[derive(Debug, PartialEq)]
pub struct Question {
    /// codex's; claude keys answers by the question's text.
    pub(crate) id: String,
    pub header: String,
    pub text: String,
    /// Label and description.
    pub options: Vec<(String, String)>,
    pub multi: bool,
    /// A free-text answer is allowed: always for claude, `isOther` for codex.
    pub other: bool,
    pub secret: bool,
}

/// A live question request, normalized before it reaches the questionnaire UI.
#[derive(Debug, PartialEq)]
pub struct Questions {
    pub request_id: String,
    pub tool_use_id: String,
    pub questions: Vec<Question>,
    pub(crate) input: Value,
    /// The transport has no tool result for these answers, so Shark must record one.
    pub record_history: bool,
}

impl Questions {
    pub(crate) fn new(
        request_id: String,
        tool_use_id: String,
        input: Value,
        record_history: bool,
    ) -> Self {
        Self {
            request_id,
            tool_use_id,
            questions: questions(&input),
            input,
            record_history,
        }
    }
}

/// Picked labels and optional free text for one question.
pub type Answer = (Vec<String>, Option<String>);

/// The questions in a tool input, claude's or codex's.
pub(crate) fn questions(input: &Value) -> Vec<Question> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    (input["questions"].as_array().into_iter().flatten())
        .map(|q| {
            let options: Vec<(String, String)> = (q["options"].as_array().into_iter().flatten())
                .map(|o| (s(&o["label"]), s(&o["description"])))
                .collect();
            Question {
                id: s(&q["id"]),
                header: s(&q["header"]),
                text: s(&q["question"]),
                multi: q["multiSelect"].as_bool().unwrap_or(false),
                other: q["isOther"].as_bool().unwrap_or(true) || options.is_empty(),
                secret: q["isSecret"].as_bool().unwrap_or(false),
                options,
            }
        })
        .collect()
}

/// The questions answered, each with its picks and the user's own text joined with ", ".
pub(crate) fn given<'a>(
    qs: &'a [Question],
    answers: &'a [Answer],
) -> impl Iterator<Item = (&'a Question, &'a Answer, String)> {
    (qs.iter().zip(answers))
        .filter(|(_, a)| !a.0.is_empty() || a.1.is_some())
        .map(|(q, a)| {
            (
                q,
                a,
                a.0.iter()
                    .map(String::as_str)
                    .chain(a.1.as_deref())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        })
}

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
    /// A line the CLI wrote itself, like a compaction's; shown as an info row, not a message.
    Notice(String),
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
        secs: u64,
    },
    /// Backgrounded tasks (subagents, shells) still running after the turn's result.
    Background(Vec<BackgroundTask>),
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
    /// Questions awaiting answers, live only.
    Questions(Questions),
    /// A plan codex's plan mode proposed. claude's come as an ExitPlanMode `Permission`.
    Plan {
        id: String,
        text: String,
    },
    Exited(String),
}

/// What claude's result says of a session it no longer has; codex's gone-thread error imitates it.
pub const SESSION_GONE: &str = "No conversation found with session ID";

pub struct Harness {
    pid: u32,
    /// Set once the reader thread has reaped the child, so we never signal a reused pid.
    exited: Arc<AtomicBool>,
    /// Shared with the reader thread, which answers codex's handshake.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Set for a `codex app-server` process.
    codex: Option<Arc<Mutex<codex::State>>>,
}

/// Claude resumes its tool with the original input plus answers keyed by question text.
fn question_reply(request: &Questions, answers: &[Answer]) -> Value {
    let by_text: serde_json::Map<String, Value> = given(&request.questions, answers)
        .map(|(q, _, all)| (q.text.clone(), all.into()))
        .collect();
    let mut input = request.input.clone();
    input["answers"] = by_text.into();
    input
}

/// Pids of the running agent processes, with their `exited` flags. Quitting stops them: macOS
/// terminate doesn't run `Drop`.
static LIVE: Mutex<Vec<(u32, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

/// SIGTERMs every running agent process (chats, side chats, the Home ask).
pub fn stop_all() {
    term_each(&LIVE.lock().unwrap());
}

fn term_each(live: &[(u32, Arc<AtomicBool>)]) {
    for (pid, exited) in live {
        if !exited.load(Ordering::SeqCst) {
            crate::proc::term(*pid);
        }
    }
}

pub fn spawn(ws: &Task, chat: &ChatMeta) -> std::io::Result<(Harness, Receiver<Ev>)> {
    if chat.is_codex() {
        return codex::spawn(ws, chat);
    }
    let skills = crate::git::claude_skill_args(ws);
    let resume = chat.session_id.iter().flat_map(|id| ["--resume", id]);
    // Tools that need a person (ExitPlanMode, AskUserQuestion) ask us instead of failing.
    let prompts = ["--permission-prompt-tool", "stdio"];
    run(
        &ws.dir(),
        chat.model(),
        &chat.id,
        (prompts.into_iter().chain(effort(chat)).chain(resume))
            .chain(skills.iter().map(String::as_str)),
        Some(transcript_path(&chat.id)),
    )
}

/// The flags of a throwaway, toolless run: no tools, no MCP servers, nothing saved.
fn bare(system: &str) -> [&str; 6] {
    [
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--append-system-prompt",
        system,
    ]
}

fn effort(chat: &ChatMeta) -> [&'static str; 2] {
    ["--effort", crate::models::effort_of(chat)]
}

const SIDE_PROMPT: &str = "The user is asking a quick side question about this conversation. \
    It is not part of the main task and you have no tools: answer briefly from what is already \
    in the conversation, and say so if you would need to look something up.";

/// A side question: a throwaway fork of the chat's session with no tools, saved nowhere, so the
/// chat itself never sees it. Works while the chat's own turn runs.
pub fn spawn_side(ws: &Task, chat: &ChatMeta) -> std::io::Result<(Harness, Receiver<Ev>)> {
    if chat.is_codex() {
        return Err(std::io::Error::other("side questions need a Claude model"));
    }
    let fork = chat
        .session_id
        .iter()
        .flat_map(|id| ["--resume", id, "--fork-session"]);
    run(
        &ws.dir(),
        chat.model(),
        &format!("{}.side", chat.id),
        bare(SIDE_PROMPT)
            .into_iter()
            .chain(effort(chat))
            .chain(fork),
        None,
    )
}

/// Shark's user docs, for answering questions about the app itself.
pub const README: &str = concat!(
    include_str!("../README.md"),
    "\n\n",
    include_str!("../docs/manual.md")
);

/// A one-shot question about Shark, outside any task: no tools, no MCP, saved nowhere.
/// `system` is appended to claude's system prompt (e.g. `README` and the keymap).
pub fn spawn_ask(model: &str, system: &str) -> std::io::Result<(Harness, Receiver<Ev>)> {
    // --safe-mode: no user hooks, skills, plugins or CLAUDE.md (`claude --help`, 2.1.309). Not
    // `--setting-sources ""`: that also drops auth set in settings.json (apiKeyHelper, env).
    let args = bare(system).into_iter().chain(["--safe-mode"]);
    run(&support_dir(), model, "ask", args, None)
}

fn run<'a>(
    cwd: &Path,
    model: &str,
    log_name: &str,
    extra: impl IntoIterator<Item = &'a str>,
    transcript: Option<PathBuf>,
) -> std::io::Result<(Harness, Receiver<Ev>)> {
    // SHARK_CLAUDE swaps in another binary; the UI tests use a fake that swallows stdin.
    let mut cmd = Command::new(crate::proc::claude_bin());
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
) -> std::io::Result<(Harness, Receiver<Ev>)> {
    fs::create_dir_all(logs_dir())?;
    fs::create_dir_all(transcripts_dir())?;
    let log = log_path(log_name);
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
    LIVE.lock().unwrap().push((child.id(), exited.clone()));
    let agent = Harness {
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
            if (evs.iter()).any(|e| {
                !matches!(
                    e,
                    Ev::TextDelta(_) | Ev::Permission { .. } | Ev::Questions(_)
                )
            }) {
                append(&transcript, &compact(&line)); // deltas are redundant with the full assistant message
            }
            for ev in evs {
                let _ = tx.send_blocking(ev);
            }
        }
        let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
        reaped.store(true, Ordering::SeqCst);
        LIVE.lock().unwrap().retain(|(p, _)| *p != pid);
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

impl Harness {
    /// The running process's backend, independent of the next selected model.
    pub fn is_codex(&self) -> bool {
        self.codex.is_some()
    }

    /// Answers or dismisses a normalized request in this process's wire format.
    pub fn answer_questions(
        &mut self,
        request: &Questions,
        answers: Option<&[Answer]>,
    ) -> std::io::Result<()> {
        if self.is_codex() {
            return self.allow(
                &request.request_id,
                codex::question_reply(&request.questions, answers),
            );
        }
        let Some(answers) = answers else {
            return self.deny(&request.request_id, ANSWER_IN_CHAT);
        };
        self.allow(&request.request_id, question_reply(request, answers))
    }

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

    pub(crate) fn write(&self, v: &Value) -> std::io::Result<()> {
        write(&self.stdin, v)
    }

    /// SIGTERM ends the turn and background tasks; the next send respawns with `--resume`.
    pub fn stop(&self) {
        if !self.exited.load(Ordering::SeqCst) {
            crate::proc::term(self.pid);
        }
    }
}

pub(crate) fn write(stdin: &Mutex<ChildStdin>, v: &Value) -> std::io::Result<()> {
    let mut stdin = stdin.lock().unwrap();
    stdin.write_all(format!("{v}\n").as_bytes())?;
    stdin.flush()
}

impl Drop for Harness {
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

pub(crate) fn parse_record(v: Value) -> Vec<Ev> {
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
                .map(|t| BackgroundTask {
                    id: s(&t["task_id"]),
                    kind: s(&t["task_type"]),
                    description: match t["description"].as_str() {
                        Some(d) => d.to_string(),
                        None => format!("{} {}", s(&t["task_type"]), s(&t["task_id"])),
                    },
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
            user_line(&v).into_iter().collect()
        }
        "user" => blocks(&v)
            .filter_map(|b| match b["type"].as_str()? {
                "tool_result" => Some(Ev::ToolResult {
                    id: s(&b["tool_use_id"]),
                    text: output(&b["content"], &b["total_chars"]),
                    is_error: b["is_error"].as_bool().unwrap_or(false),
                }),
                // "[Request interrupted by user]" or "… for tool use]"
                "text" if s(&b["text"]).starts_with("[Request interrupted by user") => {
                    Some(Ev::Interrupted)
                }
                _ => None,
            })
            .collect(),
        "control_request"
            if v["request"]["subtype"] == "can_use_tool"
                && v["request"]["tool_name"] == "AskUserQuestion" =>
        {
            vec![Ev::Questions(Questions::new(
                s(&v["request_id"]),
                s(&v["request"]["tool_use_id"]),
                v["request"]["input"].clone(),
                false,
            ))]
        }
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
            secs: v["duration_ms"].as_u64().unwrap_or(0) / 1000,
        }],
        _ => vec![],
    }
}

/// A string-content user line: the user's message, or one the CLI wrote on their behalf. A
/// compaction's summary (synthetic) and a command's stdout become notices; a slash command's
/// echo (`<command-name>/x</command-name>…<command-args>a</command-args>`) is the `/x a` typed.
fn user_line(v: &Value) -> Option<Ev> {
    let t = v["message"]["content"].as_str()?;
    let tag = |name: &str| {
        let rest = &t[t.find(&format!("<{name}>"))? + name.len() + 2..];
        Some(rest[..rest.find(&format!("</{name}>"))?].trim())
    };
    if v["isSynthetic"] == true {
        Some(Ev::Notice("Conversation compacted".into()))
    } else if let Some(out) = tag("local-command-stdout") {
        (!out.is_empty()).then(|| Ev::Notice(out.into()))
    } else if let Some(name) = tag("command-name") {
        let args = tag("command-args").unwrap_or_default();
        Some(Ev::User(format!("{name} {args}").trim().into()))
    } else {
        Some(Ev::User(t.into()))
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

    /// Quitting stops what `start` registered, and a reaped process leaves the registry.
    #[test]
    fn stop_all_signals_registered_children() {
        // `start` writes its log under the shared test home, which `test_home` wipes.
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let (agent, rx) = start(cmd, &std::env::temp_dir(), "stop-all-test", None, None).unwrap();
        let entry = (agent.pid, agent.exited.clone());
        assert!(LIVE.lock().unwrap().iter().any(|(p, _)| *p == agent.pid));
        // Only this child: another test's agent shouldn't die with it.
        term_each(&[entry]);
        let msg = loop {
            match rx.recv_blocking() {
                Ok(Ev::Exited(m)) => break m,
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
        };
        assert!(msg.contains("SIGTERM"), "{msg}");
        assert!(!LIVE.lock().unwrap().iter().any(|(p, _)| *p == agent.pid));
        crate::transcript::forget("stop-all-test");
        assert!(!log_path("stop-all-test").exists());
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
                Ev::Background(vec![
                    BackgroundTask {
                        id: "a1".into(),
                        kind: "local_agent".into(),
                        description: "local_agent a1".into(),
                    },
                    BackgroundTask {
                        id: "b2".into(),
                        kind: "local_bash".into(),
                        description: "Run test suite".into(),
                    },
                ]),
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
                    secs: 48
                },
                Ev::Result {
                    is_error: true,
                    text: "Invalid API key".into(),
                    secs: 0
                },
            ]
        );
    }

    #[test]
    fn answers_go_back_in_each_agents_shape() {
        let input = json!({"questions": [
            {"question": "Which DB?", "header": "DB", "multiSelect": false, "options": [{"label": "Postgres", "description": "SQL"}, {"label": "Redis", "description": ""}]},
            {"question": "Which tests?", "header": "Tests", "multiSelect": true, "options": [{"label": "Unit", "description": ""}, {"label": "E2E", "description": ""}]},
            {"question": "Name?", "header": "Name", "multiSelect": false, "options": [{"label": "a", "description": ""}, {"label": "b", "description": ""}]},
        ]});
        let qs = questions(&input);
        assert_eq!(
            (qs.len(), qs[1].multi, qs[0].other),
            (3, true, true),
            "claude always takes text"
        );
        assert_eq!(qs[0].options[0], ("Postgres".into(), "SQL".into()));
        let answers: Vec<Answer> = vec![
            (vec!["Postgres".into()], None),
            (vec!["Unit".into(), "E2E".into()], Some("smoke".into())),
            (vec![], None), // skipped
        ];
        let request = Questions::new("r".into(), "q".into(), input.clone(), false);
        let r = question_reply(&request, &answers);
        assert_eq!(
            r["questions"], input["questions"],
            "claude needs its questions back"
        );
        assert_eq!(
            r["answers"],
            json!({"Which DB?": "Postgres", "Which tests?": "Unit, E2E, smoke"})
        );
        assert_eq!(
            crate::transcript::answer_pairs(&qs, &answers),
            r#""Which DB?"="Postgres", "Which tests?"="Unit, E2E, smoke""#
        );

        let input = json!({"questions": [
            {"id": "file", "header": "File", "question": "Which file?", "isOther": true, "isSecret": false, "options": [{"label": "a.txt", "description": ""}]},
            {"id": "ok", "header": "Run", "question": "Run it?", "isOther": false, "isSecret": true, "options": [{"label": "Accept", "description": ""}]},
        ]});
        let qs = questions(&input);
        assert_eq!(
            (qs[0].other, qs[1].other, qs[1].secret),
            (true, false, true)
        );
        let answers: Vec<Answer> = vec![
            (vec![], Some("b.txt".into())),
            (vec!["Accept".into()], None),
        ];
        assert_eq!(
            crate::transcript::answer_pairs(&qs, &answers),
            r#""Which file?"="b.txt", "Run it?"="••••""#
        );
        assert_eq!(
            codex::question_reply(&qs, Some(&answers)),
            json!({"answers": {
                "file": {"answers": ["None of the above", "user_note: b.txt"]},
                "ok": {"answers": ["Accept"]},
            }})
        );
        assert_eq!(
            codex::question_reply(&qs, None),
            json!({"answers": {
                "file": {"answers": [format!("user_note: {ANSWER_IN_CHAT}")]},
                "ok": {"answers": [format!("user_note: {ANSWER_IN_CHAT}")]},
            }})
        );
    }

    #[test]
    fn questions_are_normalized_before_the_ui() {
        let evs = parse(
            r#"{"type":"control_request","request_id":"r","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"q","input":{"questions":[{"question":"Which?","options":[]}]}}}"#,
        );
        let [Ev::Questions(request)] = evs.as_slice() else {
            panic!("{evs:?}")
        };
        assert_eq!(request.questions[0].text, "Which?");
        assert!(request.questions[0].other);
        assert!(!request.record_history);
    }

    #[test]
    fn cli_written_user_lines_are_not_the_users() {
        let user = |line: &str| parse(line);
        // Real lines from a /compact (the summary cut short).
        let summary = r#"{"type":"user","message":{"role":"user","content":"This session is being continued from a previous conversation that ran out of context.\n\nSummary: ..."},"isSynthetic":true,"isReplay":false}"#;
        assert_eq!(
            user(summary),
            vec![Ev::Notice("Conversation compacted".into())]
        );
        let stdout = r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted </local-command-stdout>"},"isReplay":true}"#;
        assert_eq!(user(stdout), vec![Ev::Notice("Compacted".into())]);
        let empty = r#"{"type":"user","message":{"content":"<local-command-stdout></local-command-stdout>"},"isReplay":true}"#;
        assert_eq!(user(empty), vec![]);
        let echo = r#"{"type":"user","message":{"role":"user","content":"<command-message>x</command-message>\n<command-name>/x</command-name>\n<command-args>args here</command-args>"},"isReplay":true}"#;
        assert_eq!(user(echo), vec![Ev::User("/x args here".into())]);
        let bare = r#"{"type":"user","message":{"content":"<command-message>x</command-message>\n<command-name>/x</command-name>\n<command-args></command-args>"}}"#;
        assert_eq!(user(bare), vec![Ev::User("/x".into())]);
        let plain = r#"{"type":"user","message":{"role":"user","content":"hello <b>there</b>"},"isReplay":true}"#;
        assert_eq!(user(plain), vec![Ev::User("hello <b>there</b>".into())]);
        let noted = r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-notification>done</task-notification>"}}"#;
        assert_eq!(user(noted), vec![], "origin lines are still dropped");
        let result = r#"{"type":"result","subtype":"success","local_command":"compact","is_error":false,"result":"","total_cost_usd":1.5,"duration_ms":2000}"#;
        assert!(matches!(
            &user(result)[..],
            [Ev::Result {
                is_error: false,
                ..
            }]
        ));
        let turn = [Ev::Text("Ok?".into()), Ev::Notice("Compacted".into())];
        let done = Ev::Result {
            is_error: false,
            text: String::new(),
            secs: 0,
        };
        let evs: Vec<Ev> = turn.into_iter().chain([done]).collect();
        assert_eq!(
            crate::transcript::waiting(&evs, false),
            Some(crate::transcript::QUESTION),
            "a notice isn't a user turn"
        );
    }
}
