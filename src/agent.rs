//! Drives `claude -p` over its stream-json protocol, one long-lived process per workspace.
use crate::state::{Workspace, support_dir};
use serde_json::{Value, json};
use std::{
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
    Init(String),
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
    /// Count of backgrounded tasks (subagents, shells) still running after the turn's result.
    Background(usize),
    Exited(String),
}

pub struct Agent {
    pid: u32,
    /// Set once the reader thread has reaped the child, so we never signal a reused pid.
    exited: Arc<AtomicBool>,
    stdin: ChildStdin,
    transcript: PathBuf,
}

pub fn transcript_path(ws: &str) -> PathBuf {
    support_dir()
        .join("transcripts")
        .join(format!("{ws}.jsonl"))
}

fn append(path: &PathBuf, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

pub fn spawn(ws: &Workspace) -> std::io::Result<(Agent, smol::channel::Receiver<Ev>)> {
    let logs = support_dir().join("logs");
    fs::create_dir_all(&logs)?;
    fs::create_dir_all(support_dir().join("transcripts"))?;
    let log = logs.join(format!("{}.stderr.log", ws.name));
    let mut cmd = Command::new("claude");
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
        "--permission-mode",
        PERMISSION_MODE,
    ]);
    if let Some(m) = &ws.model {
        cmd.args(["--model", m]);
    }
    if let Some(id) = &ws.session_id {
        cmd.args(["--resume", id]);
    }
    let mut child = cmd
        .current_dir(ws.dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&log)?) // a piped but unread stderr could block the child
        .spawn()?;
    let (tx, rx) = smol::channel::unbounded();
    let stdout = child.stdout.take().unwrap();
    let transcript = transcript_path(&ws.name);
    let exited = Arc::new(AtomicBool::new(false));
    let reaped = exited.clone();
    let agent = Agent {
        pid: child.id(),
        exited,
        stdin: child.stdin.take().unwrap(),
        transcript: transcript.clone(),
    };
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let evs = parse(&line);
            if evs.iter().any(|e| !matches!(e, Ev::TextDelta(_))) {
                append(&transcript, &line); // deltas are redundant with the full assistant message
            }
            for ev in evs {
                let _ = tx.send_blocking(ev);
            }
        }
        let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
        reaped.store(true, Ordering::SeqCst);
        let err = fs::read_to_string(&log).unwrap_or_default();
        let tail: Vec<&str> = err.lines().rev().take(5).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
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
        let line = json!({"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": null})
            .to_string();
        append(&self.transcript, &line); // stdout doesn't echo user turns, so record them for replay
        self.stdin.write_all(format!("{line}\n").as_bytes())?;
        self.stdin.flush()
    }

    /// SIGTERM ends the turn; the next send respawns with `--resume`.
    pub fn stop(&self) {
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        let _ = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status();
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
        "system" if v["subtype"] == "init" => vec![Ev::Init(s(&v["session_id"]))],
        "system" if v["subtype"] == "background_tasks_changed" => {
            vec![Ev::Background(v["tasks"].as_array().map_or(0, Vec::len))]
        }
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
        "user" if v["message"]["content"].is_string() => {
            vec![Ev::User(s(&v["message"]["content"]))]
        }
        "user" => blocks(&v)
            .filter(|b| b["type"] == "tool_result")
            .map(|b| Ev::ToolResult {
                id: s(&b["tool_use_id"]),
                text: text_of(&b["content"]),
                is_error: b["is_error"].as_bool().unwrap_or(false),
            })
            .collect(),
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
    v["message"]["content"].as_array().into_iter().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from claude 2.1.309, trimmed.
    const SESSION: &str = r#"{"type": "system", "subtype": "hook_started", "session_id": "s1"}
{"type": "system", "subtype": "init", "session_id": "s1"}
{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a1","task_type":"local_agent"}]}
{"type":"system","subtype":"background_tasks_changed","tasks":[]}
{"type": "user", "message": {"role": "user", "content": "read a.txt"}, "parent_tool_use_id": null}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/tmp/a.txt"}}]}, "parent_tool_use_id": null}
{"type": "user", "message": {"role": "user", "content": [{"tool_use_id": "toolu_1", "type": "tool_result", "content": "1\thello\n2\t"}]}, "parent_tool_use_id": null}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "nested"}]}, "parent_tool_use_id": "toolu_9"}
{"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "h"}}, "parent_tool_use_id": null}
{"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{"}}, "parent_tool_use_id": null}
{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "thinking", "thinking": "x"}, {"type": "text", "text": "hello"}]}, "parent_tool_use_id": null}
{"type":"result","subtype":"success","is_error":false,"result":"hello","total_cost_usd":0.5,"duration_ms":48210,"session_id":"s1"}
{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["Invalid API key"],"total_cost_usd":0}
not json"#;

    #[test]
    fn parses_stream_json() {
        let evs: Vec<Ev> = SESSION.lines().flat_map(parse).collect();
        assert_eq!(
            evs,
            vec![
                Ev::Init("s1".into()),
                Ev::Background(1),
                Ev::Background(0),
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
                Ev::TextDelta("h".into()),
                Ev::Text("hello".into()),
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
    let (mut agent, rx) = spawn(&ws).unwrap();
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
    let _ = fs::remove_file(transcript_path(&ws.name));
    assert!(
        text.contains("Python") && text.contains("TypeScript"),
        "imports not loaded: {text}"
    );
}
