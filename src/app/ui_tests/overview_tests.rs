//! The Overview: a section's chat over its tasks, and the tools it runs them with.
use super::*;
use crate::chat::{Chat, NewsKind};
use crate::state::Agent;
use serde_json::{Value, json};

/// Section `A` holding empty tasks `names`, a repository `api` and an agent `Coder`.
fn section_state(home: &std::path::Path, names: &[&str]) -> State {
    let tasks = (names.iter())
        .map(|name| git::create_empty_task(&tasks_dir().join(name), name, name, None).unwrap())
        .collect();
    State {
        repos: repos(home, &["api"]),
        agents: vec![Agent {
            slug: "coder".into(),
            name: "Coder".into(),
            instructions: "Write code.".into(),
            ..Default::default()
        }],
        sections: vec![Section {
            name: "A".into(),
            icon: "folder".into(),
            overview: None,
            unsent: None,
            collapsed: false,
            tasks: names.iter().map(|n| n.to_string()).collect(),
        }],
        tasks,
        ..Default::default()
    }
}

fn active_chat(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Entity<Chat> {
    cx.update(|cx| shark.read(cx).active_chat().unwrap().clone())
}

fn chat_of(cx: &mut TestAppContext, shark: &Entity<Shark>, task: &str) -> Entity<Chat> {
    cx.update(|cx| {
        shark
            .read(cx)
            .chats
            .get(task)
            .expect("its first chat is loaded")
            .clone()
    })
}

/// A fake agent of this file's own, so one a test before it started late doesn't write over
/// what these tests read.
fn own_claude() -> PathBuf {
    let own = quiet_claude().with_file_name("overview-claude");
    fs::copy(quiet_claude(), &own).unwrap();
    own
}

/// What the fake agent was started with; it writes its arguments as it starts.
fn started_with(quiet: &std::path::Path) -> String {
    let args = PathBuf::from(format!("{}.args", quiet.display()));
    for _ in 0..3000 {
        if let Ok(s) = fs::read_to_string(&args)
            && !s.is_empty()
        {
            let _ = fs::remove_file(&args);
            return s;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the agent never started");
}

/// One call of an Overview's tool, as the MCP server hands it to the app.
fn tool(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    overview: &str,
    name: &str,
    args: Value,
) -> Result<Value, String> {
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    let mut call = Some(crate::mcp::Call {
        overview: overview.into(),
        tool: name.into(),
        args,
        reply,
        expired: Default::default(),
    });
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.overview_call(call.take().unwrap(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    let answer = answer.try_recv().expect("a tool answers at once");
    answer.map(|text| serde_json::from_str(&text).expect("tools answer in JSON"))
}

/// Opens section 0's Overview and, with `message`, sends its first message. Returns its task.
fn open_overview(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    message: Option<&str>,
) -> String {
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_overview(0, w, cx));
    let name = cx.read(|cx| shark.read(cx).active.clone().expect("the Overview is open"));
    if let Some(message) = message {
        let chat = active_chat(cx, shark);
        cx.update_window(window, |_, w, cx| {
            chat.update(cx, |c, cx| assert!(c.ask(message, w, cx)))
        })
        .unwrap();
        cx.run_until_parked();
    }
    name
}

/// Every section has an Overview as its first row. Opening it makes nothing: its task is saved,
/// and gets a directory, when the first message goes out. It has no repositories and is still
/// out of Plan mode, and only its chats get Shark's tools.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_overview_is_its_sections_first_row_and_exists_from_its_first_message(
    cx: &mut TestAppContext,
) {
    let quiet = own_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let _ = crate::mcp::start();
    let home = test_home();
    let (window, shark) = boot(cx, section_state(&home, &["one"]), WindowOptions::default());
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    on(cx, &|s, _, _| {
        assert_eq!(
            s.side_rows(false),
            [SideRow::Section(0), SideRow::Overview(0), SideRow::Task(0)]
        );
        assert_eq!(
            s.state.tasks.len(),
            1,
            "an Overview nobody opened is no task"
        );
    });

    let name = open_overview(cx, window, &shark, None);
    on(cx, &|s, _, cx| {
        let task = s.task(&name).unwrap();
        assert!(task.overview && task.unsent && task.repos.is_empty());
        assert_eq!(task.title, "Overview: A");
        assert_eq!(
            s.side_rows(false),
            [SideRow::Section(0), SideRow::Task(1), SideRow::Task(0)],
            "its task's row, still first"
        );
        assert_eq!(s.shown_overview(), Some(0));
        assert!(!s.right_rail_shown(), "no feature rail on an Overview");
        assert_eq!(
            s.home_rows(cx).len(),
            1,
            "Home lists the section's task, not this"
        );
        s.state.save().unwrap();
    });
    assert!(!tasks_dir().join(&name).exists(), "nothing on disk");
    let saved = State::load();
    assert_eq!((saved.tasks.len(), &saved.sections[0].overview), (1, &None));

    // The page: the section's tasks above the composer, and a chat that may act.
    let chat = active_chat(cx, &shark);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let c = chat.read(cx);
        assert!(
            !c.meta.plan,
            "no repository, and out of Plan mode all the same"
        );
        let listed: Vec<_> = c.section.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(listed, ["one"]);
        assert!(
            w.try_find("overview-readout").is_some(),
            "its header, not a task's"
        );
        assert!(w.try_find("run").is_none(), "no Run button");
        assert!(w.try_find("stop-all").is_none(), "nothing runs");
    })
    .unwrap();

    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| assert!(c.ask("Split the work", w, cx)))
    })
    .unwrap();
    cx.run_until_parked();
    let args = started_with(&quiet);
    let (url, token) = crate::mcp::endpoint(&name).unwrap();
    assert!(
        args.contains("--mcp-config") && args.contains(&url),
        "{args}"
    );
    assert!(
        args.contains("${SHARK_MCP_TOKEN}") && !args.contains(&token),
        "{args}"
    );
    assert!(!args.contains("--permission-mode plan"), "{args}");
    on(cx, &|s, _, _| {
        let task = s.task(&name).unwrap();
        assert!(
            task.overview && !task.unsent,
            "its first message made it a task"
        );
    });
    let docs = fs::read_to_string(tasks_dir().join(&name).join("CLAUDE.md")).unwrap();
    assert!(
        docs.starts_with("# Overview") && docs.contains(".context/overview.md"),
        "{docs}"
    );
    let saved = State::load();
    assert_eq!(saved.sections[0].overview.as_deref(), Some(name.as_str()));
    assert_eq!(
        saved.sections[0].tasks,
        ["one"],
        "it is not one of its section's tasks"
    );
    assert!(saved.tasks.iter().any(|t| t.name == name));

    // An ordinary task's chat gets no tools, so a task can't start tasks.
    on(cx, &|s, w, cx| s.open_task(0, w, cx));
    let one = active_chat(cx, &shark);
    cx.update_window(window, |_, w, cx| {
        one.update(cx, |c, cx| assert!(c.ask("Plan it", w, cx)))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!started_with(&quiet).contains("--mcp-config"));

    // Moved out, it is an ordinary task and its section a plain one again.
    on(cx, &|s, _, cx| {
        s.move_to_section(&name, None, cx);
        assert!(!s.task(&name).unwrap().overview);
        assert_eq!(
            s.side_rows(false),
            [
                SideRow::Section(0),
                SideRow::Overview(0),
                SideRow::Task(0),
                SideRow::Task(1)
            ]
        );
    });
    assert_eq!(State::load().sections[0].overview, None);
    harness::stop_all();
}

/// The four tools act on the Overview's own section and nothing else; the Overview hears what
/// happened there with its next message; Plan mode and leaving the section end its reach.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_overviews_tools_run_its_section(cx: &mut TestAppContext) {
    let quiet = own_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let mut state = section_state(&home, &["one"]);
    let outside = git::create_empty_task(&tasks_dir().join("out"), "out", "out", None).unwrap();
    state.tasks.push(outside);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    let over = open_overview(cx, window, &shark, Some("Take over"));
    let call = |cx: &mut TestAppContext, name: &str, args: Value| {
        tool(cx, window, &shark, &over, name, args)
    };

    // read_task: the section's tasks, and what spawn_task takes.
    let all = call(cx, "read_task", json!({})).unwrap();
    assert_eq!(all["section"], "A");
    assert_eq!(all["repositories"], json!(["api"]));
    assert_eq!(all["agents"], json!(["Coder"]));
    let names: Vec<_> = (all["tasks"].as_array().unwrap().iter())
        .map(|t| t["task"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["one"], "not itself, not another section's task");
    assert_eq!(
        call(cx, "read_task", json!({"task": "one"})).unwrap()["status"],
        "idle"
    );
    for other in ["out", over.as_str(), "nope"] {
        for name in ["read_task", "message_task", "stop_task"] {
            let args = json!({"task": other, "message": "hi"});
            let e = call(cx, name, args).unwrap_err();
            assert!(e.contains("in this section"), "{name} {other}: {e}");
        }
    }

    // message_task: into the task's first chat, marked as the Overview's, and no news of it.
    let sent = call(
        cx,
        "message_task",
        json!({"task": "one", "message": "Carry on."}),
    );
    assert_eq!(sent.unwrap(), json!({"task": "one", "status": "sent"}));
    let one = chat_of(cx, &shark, "one");
    let overview = chat_of(cx, &shark, &over);
    cx.read(|cx| {
        assert_eq!(one.read(cx).overview_messages(), ["Carry on."]);
        assert!(overview.read(cx).pending_news().is_empty());
    });

    // What the user writes there, and how its turn ends, the Overview hears with its next message.
    cx.update(|cx| {
        one.update(cx, |c, cx| {
            let done = |is_error| harness::Ev::Result {
                is_error,
                text: String::new(),
                secs: 90,
            };
            c.play(done(false), cx);
        })
    });
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        one.update(cx, |c, cx| assert!(c.ask("Use the theme token.", w, cx)))
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            one.read(cx).overview_messages(),
            ["Carry on."],
            "the user's has no label"
        );
        assert_eq!(
            overview.read(cx).pending_news(),
            [
                (NewsKind::Finished, "one".to_string()),
                (NewsKind::Messaged, "one".to_string())
            ]
        );
        assert!(
            shark.read(cx).section_readout(0, cx).2 >= 90,
            "its agent time counts"
        );
    });
    cx.update(|cx| {
        overview.update(cx, |c, cx| {
            let done = harness::Ev::Result {
                is_error: false,
                text: String::new(),
                secs: 1,
            };
            c.play(done, cx)
        })
    });
    cx.update_window(window, |_, w, cx| {
        overview.update(cx, |c, cx| assert!(c.ask("What changed?", w, cx)))
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let c = overview.read(cx);
        let notes = c.since_notes();
        let (head, rows) = notes.last().expect("the message carried a note");
        assert_eq!(head, "Since last turn · 1 finished · 1 you messaged");
        assert!(
            rows[0].starts_with("- one [task one]: finished: its turn ended"),
            "{rows:?}"
        );
        assert_eq!(
            rows[1],
            "- one [task one]: you messaged it: Use the theme token."
        );
        assert!(c.pending_news().is_empty(), "told once");
    });

    // stop_task stops; it doesn't delete.
    assert_eq!(
        call(cx, "stop_task", json!({"task": "one"})).unwrap()["task"],
        "one"
    );
    on(cx, &|s, _, _| assert!(s.task("one").is_some()));

    // spawn_task: a task in this section, started at once on the brief; `base` builds on another's branch.
    let bad = call(
        cx,
        "spawn_task",
        json!({"brief": "x", "repos": ["web"], "agent": "Coder"}),
    );
    assert!(bad.unwrap_err().contains("Repositories: api"));
    let bad = call(
        cx,
        "spawn_task",
        json!({"brief": "x", "repos": ["api"], "agent": "Ghost"}),
    );
    assert!(bad.unwrap_err().contains("Agents: Coder"));
    let brief = "Restack after squash merge\n\nKeep the stack intact.";
    let started = call(
        cx,
        "spawn_task",
        json!({"brief": brief, "repos": ["api"], "agent": "coder"}),
    );
    let started = started.unwrap();
    assert_eq!(started["status"], "starting");
    let first = started["task"].as_str().unwrap().to_string();
    until(cx, window, |_, cx| {
        let s = shark.read(cx);
        s.creating.is_empty() && s.starting.is_empty() && s.task(&first).is_some()
    })
    .await;
    cx.run_until_parked();
    on(cx, &|s, _, cx| {
        assert_eq!(
            s.state.sections[0].tasks,
            ["one", first.as_str()],
            "in the Overview's section"
        );
        assert_eq!(s.active.as_deref(), Some(over.as_str()), "and not opened");
        let chat = s.chats.get(&first).expect("its agent is started");
        assert_eq!(chat.read(cx).sent_texts(), [brief]);
        assert_eq!(
            chat.read(cx).overview_messages(),
            [brief],
            "the brief is the Overview's"
        );
        let news = s.chats[&over].read(cx).pending_news();
        assert!(
            !news.iter().any(|(_, task)| *task == first),
            "no news of the user's: {news:?}"
        );
    });
    let child = call(
        cx,
        "spawn_task",
        json!({"brief": "Retarget on merge", "repos": ["api"], "agent": "Coder", "base": first}),
    );
    let child = child.unwrap()["task"].as_str().unwrap().to_string();
    until(cx, window, |_, cx| {
        let s = shark.read(cx);
        s.starting.is_empty() && s.task(&child).is_some()
    })
    .await;
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        let (parent, child) = (s.task(&first).unwrap(), s.task(&child).unwrap());
        let branch = parent.branch_of(&parent.repos[0]);
        assert_eq!(
            child.repos[0].base, branch,
            "cut from the parent's unpushed branch"
        );
        assert_eq!(
            crate::stack::parent_of(&child.repos[0].base, "main"),
            Some(branch)
        );
    });
    let e = call(
        cx,
        "spawn_task",
        json!({"brief": "x", "repos": ["api"], "agent": "Coder", "base": "out"}),
    );
    assert!(e.unwrap_err().contains("in this section"));

    // In Plan mode it may read, and nothing else.
    cx.update(|cx| overview.update(cx, |c, cx| c.set_plan_mode(true, cx)));
    cx.run_until_parked();
    assert!(call(cx, "read_task", json!({})).is_ok());
    for (name, args) in [
        (
            "spawn_task",
            json!({"brief": "x", "repos": ["api"], "agent": "Coder"}),
        ),
        ("message_task", json!({"task": "one", "message": "hi"})),
        ("stop_task", json!({"task": "one"})),
    ] {
        assert!(
            call(cx, name, args).unwrap_err().contains("Plan mode"),
            "{name}"
        );
    }
    cx.update(|cx| overview.update(cx, |c, cx| c.set_plan_mode(false, cx)));

    // Stop all reaches every chat of the section, the Overview's too.
    on(cx, &|s, _, cx| {
        let running = s.section_readout(0, cx).0;
        assert!(running >= 1, "the Overview's own turn runs");
        s.stop_section(0, cx);
    });

    // Its token outlives the role: out of the section, the tools refuse.
    on(cx, &|s, _, cx| s.move_to_section(&over, None, cx));
    for name in ["read_task", "stop_task"] {
        let e = call(cx, name, json!({"task": "one"})).unwrap_err();
        assert!(e.contains("not a section's Overview"), "{e}");
    }
    harness::stop_all();
}

/// An Overview nobody has messaged is a row, not a task: commands on the open task pass it by,
/// and deleting its section drops it and leaves the cursor on the task it was on.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_unsent_overview_is_no_task_to_act_on(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, section_state(&home, &["one"]), WindowOptions::default());
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    let name = open_overview(cx, window, &shark, None);
    on(cx, &|s, w, cx| {
        assert!(
            s.target(w).is_none(),
            "no task for Rename, Move or Attach to hit"
        );
        s.move_to_section(&name, None, cx);
        assert!(
            s.task(&name).is_some_and(|t| t.unsent),
            "nothing to move yet"
        );
        assert_eq!(s.state.overview_of(0), Some(name.as_str()));
        // A task made after it sits behind it in the list; the cursor is on that one.
        let later = git::create_empty_task(&tasks_dir().join("later"), "later", "later", None);
        s.state.tasks.push(later.unwrap());
        let at = s.state.tasks.len() - 1;
        s.sel = SideRow::Task(at);
        s.delete_section(0, w, cx);
        assert!(s.task(&name).is_none(), "the draft goes with its section");
        assert!(!s.chats.contains_key(&name), "and its chat with it");
        assert!(s.home_shown(), "its page is gone");
        let SideRow::Task(ix) = s.sel else {
            panic!("the cursor stays on a task: {:?}", s.sel);
        };
        assert_eq!(
            s.state.tasks[ix].name, "later",
            "the same task, at its new index"
        );
    });
}

/// On the real renderer: the Overview row answers the pointer like the rows around it, before
/// it has a task and after.
/// `SHARK_RENDER_OVERVIEW_ONLY=1 cargo test --features test-support --test rendering`
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    section_state(&home, &["one"]).save().unwrap();
    let (window, shark) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1280.), px(800.)),
                    ))),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |w, cx| cx.new(|cx| Shark::new(w, cx)),
            )
        })
        .unwrap();
    cx.run_until_parked();
    // The row's background, sampled right of its text; `hover` puts the pointer on `id` first.
    let row_color = |cx: &mut gpui_kit::HeadlessAppContext, hover: Option<&'static str>| {
        let at = cx
            .update_window(window, |_, w, cx| {
                match hover {
                    Some("overview") => w.hover(("overview", 0usize), cx),
                    Some(other) => w.hover(other, cx),
                    None => {}
                }
                w.render_frame(cx);
                let b = w.find(("overview", 0usize)).bounds();
                (f32::from(b.right()) - 12., f32::from(b.center().y))
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        let shot = cx.capture_screenshot(window).expect("real renderer");
        let scale = shot.width() as f32 / 1280.;
        *shot.get_pixel((at.0 * scale) as u32, (at.1 * scale) as u32)
    };
    for sent in [false, true] {
        if sent {
            // Once it has a task, and another row is the open one.
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.open_overview(0, w, cx);
                    let name = s.active.clone().unwrap();
                    s.state.overview_sent(&name);
                    s.open_task(0, w, cx);
                })
            })
            .unwrap();
            cx.run_until_parked();
        }
        let away = row_color(cx, Some("ws-header"));
        let over = row_color(cx, Some("overview"));
        assert_ne!(
            away, over,
            "hovering highlights the Overview row (sent: {sent})"
        );
        assert_eq!(
            row_color(cx, Some("ws-header")),
            away,
            "and leaving ends it"
        );
    }
    cx.update_window(window, |_, w, _| w.remove_window())
        .unwrap();
    cx.run_until_parked();
}

/// The whole thing against a real Claude, on the real renderer: an Overview starts a task with
/// `spawn_task`, hears how its turn ended, messages it with `message_task`, and is refused in
/// Plan mode. Uses a Claude subscription for a handful of small turns:
/// `SHARK_LIVE_OVERVIEW=1 SHARK_RENDER_DIR=/tmp/overview-live cargo test --features test-support --test rendering`
pub(super) fn live(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    section_state(&home, &[]).save().unwrap();
    let (window, shark) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1280.), px(800.)),
                    ))),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |w, cx| cx.new(|cx| Shark::new(w, cx)),
            )
        })
        .unwrap();
    // As `main` does: the server, and its calls run where the tasks are.
    let calls = crate::mcp::start().expect("the server starts once");
    let tools = shark.downgrade();
    cx.update(|cx| {
        cx.spawn(async move |cx| {
            while let Ok(call) = calls.recv().await {
                cx.update(|cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        tools.update(cx, |s, cx| s.overview_call(call, window, cx))
                    });
                });
            }
        })
        .detach()
    });
    // Real agents answer on their own threads, in their own time.
    cx.allow_parking();
    cx.run_until_parked();

    let shot = |cx: &mut gpui_kit::HeadlessAppContext, name: &str| {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        let screenshot = cx.capture_screenshot(window).expect("real renderer");
        if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir).unwrap();
            screenshot
                .save(dir.join(format!("live-{name}.png")))
                .unwrap();
        }
    };
    // Lets the app run for up to `secs`, until `done`.
    fn pump(
        cx: &mut gpui_kit::HeadlessAppContext,
        secs: u64,
        what: &str,
        mut done: impl FnMut(&mut gpui_kit::HeadlessAppContext) -> bool,
    ) {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
            cx.advance_clock(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }
    let on = |cx: &mut gpui_kit::HeadlessAppContext, f: &AppUpdate<'_>| {
        cx.update_window(window, |_, w, cx| shark.update(cx, |s, cx| f(s, w, cx)))
            .unwrap();
        cx.run_until_parked();
    };
    let idle = |cx: &mut gpui_kit::HeadlessAppContext, task: &str| {
        cx.update(|cx| {
            let s = shark.read(cx);
            (s.chats.get(task)).is_some_and(|c| !c.read(cx).busy && c.read(cx).turns() > 0)
                && s.running_chats(task, cx) == 0
        })
    };
    let ask = |cx: &mut gpui_kit::HeadlessAppContext, task: &str, text: &str| {
        let chat = cx.update(|cx| shark.read(cx).chats.get(task).unwrap().clone());
        cx.update_window(window, |_, w, cx| {
            chat.update(cx, |c, cx| assert!(c.ask(text, w, cx), "sent: {text}"))
        })
        .unwrap();
        cx.run_until_parked();
    };

    on(cx, &|s, w, cx| s.open_overview(0, w, cx));
    let over = cx.update(|cx| shark.read(cx).active.clone().expect("the Overview is open"));
    shot(cx, "1-unsent");
    assert!(
        !tasks_dir().join(&over).exists(),
        "nothing on disk before a message"
    );

    // 1. It reads its section and starts a task, which does its work.
    ask(
        cx,
        &over,
        "Use the shark tools. First call read_task with no arguments. Then call spawn_task once \
         with brief \"In the api repository, create the file hello.txt containing the single line \
         hi and commit it. Then end your turn. Do not ask questions.\", repos [\"api\"] and agent \
         \"Coder\". Then reply with the new task's name and stop.",
    );
    let child = std::cell::RefCell::new(String::new());
    pump(cx, 900, "the spawned task to finish its first turn", |cx| {
        let name = cx.update(|cx| shark.read(cx).state.sections[0].tasks.first().cloned());
        let Some(name) = name else { return false };
        *child.borrow_mut() = name.clone();
        idle(cx, &name) && idle(cx, &over)
    });
    let child = child.into_inner();
    shot(cx, "2-started");
    let hello = tasks_dir().join(&child).join("api/hello.txt");
    assert_eq!(
        fs::read_to_string(&hello).unwrap().trim(),
        "hi",
        "the task did its brief"
    );
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(tasks_dir().join(&over).join("CLAUDE.md").exists());
        assert_eq!(s.state.sections[0].overview.as_deref(), Some(over.as_str()));
        let task = s.task(&child).unwrap();
        assert_eq!(task.repos[0].name, "api");
        let briefs = s.chats[&child].read(cx).overview_messages();
        assert_eq!(
            briefs.len(),
            1,
            "its brief is the Overview's message: {briefs:?}"
        );
        assert_eq!(
            s.active.as_deref(),
            Some(over.as_str()),
            "nothing was opened"
        );
        assert!(
            s.unread.contains(&child),
            "its finished turn is the user's to read first"
        );
    });

    // 2. With its next message it hears how that turn ended, and messages the task.
    ask(
        cx,
        &over,
        "Call message_task for that task with the message: Append the line there to hello.txt \
         and commit. Then end your turn. After the call, reply SENT and stop.",
    );
    pump(cx, 900, "the task to act on the Overview's message", |cx| {
        let done = fs::read_to_string(&hello).is_ok_and(|t| t.contains("there"));
        done && idle(cx, &child) && idle(cx, &over)
    });
    shot(cx, "3-messaged");
    cx.update(|cx| {
        let s = shark.read(cx);
        let notes = s.chats[&over].read(cx).since_notes();
        let (head, rows) = notes.first().expect("the second message carried a note");
        assert!(head.contains("1 finished"), "{head}");
        assert!(
            rows[0].contains(&format!("[task {child}]: finished: ")),
            "{rows:?}"
        );
        assert_eq!(rows.len(), 1, "its own brief is no news: {rows:?}");
        let told = s.chats[&child].read(cx).overview_messages();
        assert_eq!(
            told.len(),
            2,
            "the brief and the message, both labelled: {told:?}"
        );
    });

    // 3. In Plan mode nothing is started.
    let overview = cx.update(|cx| shark.read(cx).chats[&over].clone());
    cx.update(|cx| overview.update(cx, |c, cx| c.set_plan_mode(true, cx)));
    ask(
        cx,
        &over,
        "Call spawn_task with brief \"x\", repos [\"api\"] and agent \"Coder\". Call nothing \
         else and write no plan. Then say in one word whether the call RAN or was REFUSED.",
    );
    pump(cx, 600, "the Overview's plan-mode turn", |cx| {
        idle(cx, &over)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.sections[0].tasks, [child.clone()], "no second task");
        assert!(s.starting.is_empty() && s.creating.is_empty());
    });
    shot(cx, "4-plan-mode");

    // The task's own page: the Overview's message carries its label.
    on(cx, &|s, w, cx| s.open_task_named(&child, w, cx));
    shot(cx, "5-task");
    harness::stop_all();
    cx.update_window(window, |_, w, _| w.remove_window())
        .unwrap();
    cx.run_until_parked();
}
