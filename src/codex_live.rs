//! QA against the real `codex app-server`: `cargo test codex_live -- --ignored --test-threads=1`.
//! Each test uses a ChatGPT subscription turn or two; prompts are kept tiny.
use crate::agent::{self, Agent, Ev};
use crate::git;
use crate::state::{Repo, Workspace};
use gpui_kit::base::async_util::Receiver;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

const MODEL: &str = "codex:gpt-6.1-sol";
const TURN: u64 = 600; // secs a turn may take

/// Two temp repos in a workspace linked into `workspaces_dir()`; removed on drop.
struct Env {
    root: PathBuf,
    ws: Workspace,
}

impl Env {
    fn new(tag: &str, model: &str) -> Env {
        let root = std::env::temp_dir().join(format!("shark-cxlive-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut repos = vec![];
        for name in ["api", "web"] {
            let p = root.join("src").join(name);
            fs::create_dir_all(&p).unwrap();
            git::git(&p, &["init", "-q", "-b", "main"]).unwrap();
            fs::write(p.join("README.md"), format!("{name}\n")).unwrap();
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
        ws.name = format!("cxlive-{tag}-{}", std::process::id());
        // A unique transcript name: the default (`live`) would be a real chat's file.
        ws.chats[0].id = ws.name.clone();
        ws.chats[0].model = Some(model.into());
        fs::create_dir_all(crate::state::workspaces_dir()).unwrap();
        let link = crate::state::workspaces_dir().join(&ws.name);
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(root.join("ws/live"), &link).unwrap();
        let _ = fs::remove_file(self::transcript(&ws));
        Env { root, ws }
    }

    fn spawn(&self, session_id: Option<String>) -> Run {
        let mut meta = self.ws.chats[0].clone();
        meta.session_id = session_id;
        let (agent, rx) = agent::spawn(&self.ws, &meta).unwrap();
        Run {
            agent,
            rx,
            evs: vec![],
            t0: Instant::now(),
        }
    }

    fn repo(&self, name: &str) -> PathBuf {
        self.root.join("ws/live").join(name)
    }
}

fn transcript(ws: &Workspace) -> PathBuf {
    agent::transcript_path(&ws.chats[0].id)
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_file(crate::state::workspaces_dir().join(&self.ws.name));
        let _ = fs::remove_file(transcript(&self.ws));
        let log = format!("{}.stderr.log", self.ws.chats[0].id);
        let _ = fs::remove_file(crate::state::support_dir().join("logs").join(log));
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Run {
    agent: Agent,
    rx: Receiver<Ev>,
    evs: Vec<Ev>,
    t0: Instant,
}

impl Run {
    /// Collects events until `pred` matches one (true) or `secs` pass or the channel closes.
    fn until(&mut self, secs: u64, pred: impl Fn(&Ev) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            match self.rx.try_recv() {
                Ok(ev) => {
                    eprintln!("  {:>6.2}s {}", self.t0.elapsed().as_secs_f64(), short(&ev));
                    let hit = pred(&ev);
                    self.evs.push(ev);
                    if hit {
                        return true;
                    }
                }
                Err(e) if e.is_closed() => return false,
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        false
    }

    /// Stops the process and waits for its reader's last transcript line.
    fn close(&mut self) {
        self.agent.stop();
        self.until(10, is_exit);
        self.evs.retain(|e| !is_exit(e));
    }

    /// Everything that arrives in `secs`.
    fn drain(&mut self, secs: u64) {
        self.until(secs, |_| false);
    }

    fn mark(&self) -> usize {
        self.evs.len()
    }

    fn results(&self) -> usize {
        self.evs
            .iter()
            .filter(|e| matches!(e, Ev::Result { .. }))
            .count()
    }

    fn text(&self) -> String {
        self.evs
            .iter()
            .filter_map(|e| match e {
                Ev::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn short(ev: &Ev) -> String {
    let clip = |s: &str| s.chars().take(90).collect::<String>().replace('\n', "⏎");
    match ev {
        Ev::Init { session_id, .. } => format!("Init({session_id})"),
        Ev::User(t) => format!("User({})", clip(t)),
        Ev::TextDelta(_) => "Δ".into(),
        Ev::Text(t) => format!("Text({})", clip(t)),
        Ev::ToolUse { id, name, input } => {
            format!("ToolUse({id} {name} {})", clip(&input.to_string()))
        }
        Ev::ToolResult { id, text, is_error } => {
            format!("ToolResult({id} err={is_error} {})", clip(text))
        }
        Ev::Result {
            is_error,
            text,
            secs,
            ..
        } => format!("Result(err={is_error} {secs}s {})", clip(text)),
        Ev::Interrupted => "Interrupted".into(),
        Ev::Exited(e) => format!("Exited({})", clip(e)),
        other => format!("{other:?}"),
    }
}

fn is_result(e: &Ev) -> bool {
    matches!(e, Ev::Result { .. })
}

fn is_exit(e: &Ev) -> bool {
    matches!(e, Ev::Exited(_))
}

/// Live events as the transcript should replay them.
fn stored(evs: &[Ev]) -> Vec<&Ev> {
    evs.iter()
        .filter(|e| !matches!(e, Ev::TextDelta(_) | Ev::Permission { .. } | Ev::Exited(_)))
        .collect()
}

/// Scenario 6: the transcript replays what the live turns showed; prints the diff and the
/// file's make-up.
fn check_replay(env: &Env, live: &[Ev]) {
    let path = transcript(&env.ws);
    let body = fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let mut methods = std::collections::BTreeMap::<String, usize>::new();
    for l in &lines {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_default();
        let key = match (v["method"].as_str(), v["params"]["item"]["type"].as_str()) {
            (Some(m), Some(t)) => format!("{m}:{t}"),
            (Some(m), None) => m.into(),
            (None, _) => format!("reply:{}", v["id"]),
        };
        *methods.entry(key).or_default() += 1;
    }
    let longest = lines.iter().map(|l| l.len()).max().unwrap_or(0);
    eprintln!(
        "transcript {}: {} bytes, {} lines, longest {longest}; by kind {methods:?}",
        path.display(),
        body.len(),
        lines.len()
    );
    let noise: Vec<_> = methods
        .keys()
        .filter(|k| k.starts_with("hook/") || k.contains("tokenUsage") || k.contains("reasoning"))
        .collect();
    eprintln!("noise stored: {noise:?}");
    let replay = agent::read_history(&path, u64::MAX);
    let want = stored(live);
    let got: Vec<&Ev> = replay.iter().collect();
    if got != want {
        eprintln!(
            "REPLAY MISMATCH: live {} vs replay {}",
            want.len(),
            got.len()
        );
        for i in 0..want.len().max(got.len()) {
            let (w, g) = (want.get(i), got.get(i));
            if w != g {
                eprintln!(
                    "  #{i}\n    live:   {}\n    replay: {}",
                    w.map_or("-".into(), |e| short(e)),
                    g.map_or("-".into(), |e| short(e))
                );
            }
        }
    }
    assert_eq!(got, want, "transcript replay differs from live events");
}

fn git_out(dir: &PathBuf, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// 1 + 6: an edit and a commit in one repo of the workspace, then the transcript's replay.
#[test]
#[ignore]
fn codex_live_edit_commit() {
    let env = Env::new("edit", MODEL);
    let mut r = env.spawn(None);
    r.agent
        .send("Create api/hello.txt containing the single line hi, then commit it in the api repo (cd api) with message 'add hello'. Reply with one word when done.")
        .unwrap();
    assert!(r.until(TURN, |e| is_result(e) || is_exit(e)), "no Result");
    let tools: Vec<&str> = r
        .evs
        .iter()
        .filter_map(|e| match e {
            Ev::ToolUse { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let results = r
        .evs
        .iter()
        .filter(|e| matches!(e, Ev::ToolResult { .. }))
        .count();
    eprintln!("tools {tools:?}, {results} results");
    assert!(
        tools.iter().any(|n| *n == "Bash" || *n == "Edit"),
        "{tools:?}"
    );
    assert!(results > 0);
    assert!(
        matches!(
            r.evs.last(),
            Some(Ev::Result {
                is_error: false,
                ..
            })
        ),
        "{:?}",
        r.evs.last()
    );
    let log = git_out(&env.repo("api"), &["log", "--format=%s", "-n", "3"]);
    let tree = git_out(&env.repo("api"), &["ls-tree", "--name-only", "HEAD"]);
    eprintln!("api log: {log:?} tree: {tree:?}");
    assert!(
        tree.lines().any(|l| l == "hello.txt"),
        "not committed: {log} {tree}"
    );
    // Every id a ToolResult names was announced by a ToolUse.
    for e in &r.evs {
        if let Ev::ToolResult { id, .. } = e {
            assert!(
                r.evs
                    .iter()
                    .any(|u| matches!(u, Ev::ToolUse { id: i, .. } if i == id)),
                "orphan result {id}"
            );
        }
    }
    r.close();
    check_replay(&env, &r.evs);
}

/// 2: a message sent while `sleep 8` runs steers the turn.
#[test]
#[ignore]
fn codex_live_steer() {
    let env = Env::new("steer", MODEL);
    let mut r = env.spawn(None);
    r.agent
        .send("Run the shell command `sleep 8`, then reply with the word FIRST.")
        .unwrap();
    assert!(
        r.until(TURN, |e| matches!(e, Ev::ToolUse { .. }) || is_result(e)),
        "no tool use"
    );
    assert!(
        !is_result(r.evs.last().unwrap()),
        "turn ended before the steer"
    );
    let at = r.mark();
    let sent = Instant::now();
    r.agent
        .send("Also include the word ZEBRA in your reply.")
        .unwrap();
    assert!(r.until(TURN, is_result), "no Result");
    r.drain(15); // a second Result would show up here
    let echo = r.evs[at..]
        .iter()
        .position(|e| matches!(e, Ev::User(t) if t.contains("ZEBRA")));
    eprintln!(
        "steer echo at {echo:?} ({:?} after send), results {}, text {:?}",
        sent.elapsed(),
        r.results(),
        r.text()
    );
    let users = r.evs.iter().filter(|e| matches!(e, Ev::User(_))).count();
    assert!(echo.is_some(), "steered message never echoed as Ev::User");
    assert!(r.text().contains("ZEBRA"), "steer not acted on");
    assert_eq!(r.results(), 1, "expected exactly one Result");
    assert_eq!(users, 2);
    r.close();
    check_replay(&env, &r.evs);
}

/// 3: interrupt before the turn has an id errs; mid-command it ends the turn promptly.
#[test]
#[ignore]
fn codex_live_interrupt() {
    let env = Env::new("intr", MODEL);
    let mut r = env.spawn(None);
    r.agent
        .send("Run the shell command `sleep 37`, then reply OK.")
        .unwrap();
    // Before turn/started it errs (the server would refuse it); the reader may already have
    // seen the turn start, so either is right here.
    eprintln!("interrupt right after send: {:?}", r.agent.interrupt());
    assert!(
        r.until(TURN, |e| matches!(e, Ev::ToolUse { .. }) || is_result(e)),
        "no tool use"
    );
    std::thread::sleep(Duration::from_secs(1));
    let at = r.mark();
    let t = Instant::now();
    r.agent.interrupt().unwrap();
    assert!(r.until(10, is_result), "no Result within 10s of interrupt");
    let took = t.elapsed();
    let tail: Vec<String> = r.evs[at..].iter().map(short).collect();
    eprintln!("after interrupt ({took:?}): {tail:?}");
    assert!(r.evs[at..].contains(&Ev::Interrupted), "no Interrupted");
    assert!(took < Duration::from_secs(5), "slow: {took:?}");
    let sleeping = Command::new("pgrep")
        .args(["-f", "sleep 37"])
        .output()
        .unwrap();
    eprintln!(
        "sleep 37 still running: {:?}",
        String::from_utf8_lossy(&sleeping.stdout).trim()
    );
    std::thread::sleep(Duration::from_secs(3));
    let sleeping = Command::new("pgrep")
        .args(["-f", "sleep 37"])
        .output()
        .unwrap();
    eprintln!(
        "sleep 37 3s later: {:?}",
        String::from_utf8_lossy(&sleeping.stdout).trim()
    );
    // The process lives on: a second interrupt now has no turn.
    eprintln!("interrupt when idle: {:?}", r.agent.interrupt());
    r.drain(3);
    assert_eq!(r.results(), 1);
    assert!(!r.evs.iter().any(is_exit));
}

/// 4 + 6: a new process resumes the thread by `session_id`.
#[test]
#[ignore]
fn codex_live_resume() {
    let env = Env::new("resume", MODEL);
    let mut a = env.spawn(None);
    a.agent
        .send("Remember the word PELICAN. Reply only OK.")
        .unwrap();
    assert!(a.until(TURN, |e| is_result(e) || is_exit(e)));
    let Some(Ev::Init { session_id, .. }) = a.evs.iter().find(|e| matches!(e, Ev::Init { .. }))
    else {
        panic!("no Init: {:?}", a.evs)
    };
    let sid = session_id.clone();
    a.close();
    let mut b = env.spawn(Some(sid.clone()));
    b.agent
        .send("What word did I ask you to remember? Reply with just the word.")
        .unwrap();
    assert!(b.until(TURN, |e| is_result(e) || is_exit(e)));
    let Some(Ev::Init {
        session_id: sid2, ..
    }) = b.evs.iter().find(|e| matches!(e, Ev::Init { .. }))
    else {
        panic!("no Init: {:?}", b.evs)
    };
    eprintln!("session {sid} -> {sid2}; answer {:?}", b.text());
    assert_eq!(&sid, sid2);
    assert!(
        b.text().to_uppercase().contains("PELICAN"),
        "{:?}",
        b.text()
    );
    b.close();
    let mut all = a.evs;
    all.extend(b.evs);
    check_replay(&env, &all);
}

/// 5: an unknown model ends in an error, not a hang.
#[test]
#[ignore]
fn codex_live_bad_model() {
    let env = Env::new("badmodel", "codex:nope-model");
    let mut r = env.spawn(None);
    r.agent.send("Reply OK.").unwrap();
    assert!(
        r.until(90, |e| is_result(e) || is_exit(e)),
        "hang: no Result"
    );
    // The process stays up (the thread itself is fine); a second message fails the same way.
    r.agent.send("Reply OK.").unwrap();
    assert!(
        r.until(90, |e| is_result(e) || is_exit(e)),
        "hang on second send"
    );
    r.drain(3);
    eprintln!("bad model: {:?}", r.evs);
    assert_eq!(
        r.evs
            .iter()
            .filter(|e| matches!(e, Ev::Result { is_error: true, .. }))
            .count(),
        2,
        "each turn ends in an error Result"
    );
    assert!(!r.evs.iter().any(is_exit), "process exited");
}

/// 7: two messages sent before the thread exists both reach the agent.
#[test]
#[ignore]
fn codex_live_queued() {
    let env = Env::new("queued", MODEL);
    let mut r = env.spawn(None);
    r.agent.send("Reply with the word ALPHA.").unwrap();
    r.agent.send("Also include the word BRAVO.").unwrap();
    r.until(TURN, is_result);
    r.drain(20);
    let users: Vec<&Ev> = r.evs.iter().filter(|e| matches!(e, Ev::User(_))).collect();
    eprintln!(
        "queued: users {users:?}, results {}, text {:?}",
        r.results(),
        r.text()
    );
    let t = r.text();
    assert!(t.contains("ALPHA") && t.contains("BRAVO"), "{t:?}");
    assert_eq!(users.len(), 2, "both messages echoed");
    r.close();
    check_replay(&env, &r.evs);
}

/// 8: a message sent as the turn's final text lands, just before `turn/completed`: the steer
/// can reach a turn that is already ending.
#[test]
#[ignore]
fn codex_live_steer_at_end() {
    let env = Env::new("steerend", MODEL);
    let mut r = env.spawn(None);
    r.agent
        .send("Reply with only the word ONE. Use no tools.")
        .unwrap();
    assert!(r.until(TURN, |e| matches!(e, Ev::Text(_)) || is_result(e)));
    r.agent
        .send("Now reply with only the word TWO. Use no tools.")
        .unwrap();
    r.until(TURN, |e| matches!(e, Ev::Text(t) if t.contains("TWO")));
    r.drain(5);
    let users = r.evs.iter().filter(|e| matches!(e, Ev::User(_))).count();
    eprintln!(
        "steer at end: users {users}, results {}, text {:?}",
        r.results(),
        r.text()
    );
    assert!(r.text().contains("TWO"), "second message lost");
    assert_eq!(users, 2);
}

/// Plan mode: a plan turn proposes a plan and edits nothing (read-only sandbox); with plan
/// mode off, the next message builds it. Fast off rides along. Then the transcript's replay.
#[test]
#[ignore]
fn codex_live_plan() {
    let mut env = Env::new("plan", MODEL);
    env.ws.chats[0].plan = true;
    env.ws.chats[0].fast = Some(false);
    let mut r = env.spawn(None);
    r.agent
        .send("Plan adding api/hello.txt containing the single line hi. Two steps at most; don't ask me anything.")
        .unwrap();
    assert!(r.until(TURN, |e| is_result(e) || is_exit(e)), "no Result");
    let plan = r.evs.iter().find_map(|e| match e {
        Ev::Plan { text, .. } => Some(text.clone()),
        _ => None,
    });
    eprintln!("plan: {plan:?}");
    assert!(plan.is_some_and(|p| p.contains("hello")), "no plan");
    assert!(
        !env.repo("api").join("hello.txt").exists(),
        "plan mode edited"
    );
    r.agent.set_plan_mode(false).unwrap();
    r.agent
        .send("I approve the plan. Implement it now.")
        .unwrap();
    let at = r.results();
    assert!(r.until(TURN, |e| is_result(e) || is_exit(e)) && r.results() > at);
    assert!(env.repo("api").join("hello.txt").exists(), "not built");
    r.close();
    check_replay(&env, &r.evs);
}

/// A sub-agent's turn runs on its own thread: the chat's turn, its session and its one
/// Result are the main thread's. Needs codex's `multi_agent` feature. Then the replay.
#[test]
#[ignore]
fn codex_live_sub_agent() {
    let env = Env::new("subagent", MODEL);
    let mut r = env.spawn(None);
    r.agent
        .send("Spawn exactly one sub-agent that runs the shell command `sleep 10` and replies DONE. Wait for it, then run `sleep 5` yourself and reply with only the word FINISHED.")
        .unwrap();
    assert!(r.until(TURN, |e| is_result(e) || is_exit(e)), "no Result");
    r.drain(5);
    let sessions: std::collections::BTreeSet<&str> = (r.evs.iter())
        .filter_map(|e| match e {
            Ev::Init { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .collect();
    eprintln!(
        "sessions {sessions:?}, results {}, text {:?}",
        r.results(),
        r.text()
    );
    assert_eq!(sessions.len(), 1, "a sub-agent's thread became the session");
    assert_eq!(r.results(), 1, "a sub-agent's turn ended the chat's");
    assert!(
        r.text().contains("FINISHED"),
        "Result came before the main turn's reply"
    );
    // Its row: started, and ended before the main turn did.
    let Some(Ev::ToolUse { id, .. }) =
        (r.evs.iter()).find(|e| matches!(e, Ev::ToolUse { name, .. } if name == "Agent"))
    else {
        panic!("no Agent row")
    };
    let end = (r.evs.iter()).position(|e| matches!(e, Ev::ToolResult { id: i, .. } if i == id));
    assert!(
        end.is_some_and(|e| e < r.evs.len() - 1),
        "Agent row never ended"
    );
    r.close();
    check_replay(&env, &r.evs);
}
