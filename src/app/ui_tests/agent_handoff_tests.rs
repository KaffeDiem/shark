use super::*;
use crate::state::{Agent, AgentLink};
use gpui_kit::test::TestQueryExt as _;

fn agents() -> Vec<Agent> {
    vec![
        Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            next: Some("planner".into()),
            ..Default::default()
        },
        Agent {
            slug: "planner".into(),
            name: "Planner".into(),
            instructions: "Make an implementation plan".into(),
            model: Some(crate::models::MODELS[1].into()),
            effort: Some("low".into()),
            ..Default::default()
        },
    ]
}

/// A skill `skill` in agent `slug`'s folder; the path a task's link points at.
fn agent_skill(slug: &str, skill: &str) -> PathBuf {
    let dir = crate::state::agent_skills_dir(slug).join(skill);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), format!("# {skill}")).unwrap();
    dir
}

fn linked(task: &Task, skill: &str) -> Option<PathBuf> {
    fs::read_link(task.dir().join(".claude/skills").join(skill)).ok()
}

fn write_transcript(chat: &str, lines: &[&str]) {
    let path = crate::transcript::transcript_path(chat);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, lines.join("\n")).unwrap();
}

const TURN: [&str; 3] = [
    r#"{"type":"user","message":{"content":"Design onboarding"}}"#,
    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Use three steps"}]}}"#,
    r#"{"type":"result","result":"Use three steps","is_error":false}"#,
];

/// A task `name` whose one chat has `agent` (none when `None`).
fn task_for(name: &str, agent: Option<&Agent>) -> Task {
    git::create_empty_task(
        &tasks_dir().join(name),
        name,
        "Design",
        agent.map(AgentLink::from),
    )
    .unwrap()
}

fn shown_chat(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Entity<crate::chat::Chat> {
    cx.read(|cx| shark.read(cx).active_chat().unwrap().clone())
}

/// The open menu's labelled rows: headings as "title. detail", items as their label.
fn menu_rows(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<String> {
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        (w.within("menu-parent").elements().iter())
            .filter_map(|e| e.label().map(str::to_owned))
            .collect()
    })
    .unwrap()
}

fn toasts(cx: &mut TestAppContext, window: AnyWindowHandle) -> usize {
    cx.update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap()
}

/// Before the first message the composer's agent button picks the chat's agent: its model and
/// thinking, its snapshot in the task and its skills linked; No agent takes them back out.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn composer_agent_menu_puts_agent_on_unstarted_chat(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let skill = agent_skill("planner", "planning");
    let (window, shark) = boot(
        cx,
        State {
            agents: agents(),
            tasks: vec![task_for("pick", None)],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("pick", w, cx));
    let chat = shown_chat(cx, &shark);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find_by_label("Agent: none").is_some());
        w.click("chat-agent", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        menu_rows(cx, window),
        ["Designer", "Planner", "No agent", "New agent…"]
    );
    press_in(window)(cx, &["p", "enter"]);
    cx.read(|cx| {
        let meta = &chat.read(cx).meta;
        assert_eq!(meta.agent.as_deref(), Some("planner"));
        assert_eq!(meta.model.as_deref(), Some(crate::models::MODELS[1]));
        assert_eq!(meta.effort.as_deref(), Some("low"));
        let task = shark.read(cx).task("pick").unwrap();
        assert_eq!(task.chats[0].agent.as_deref(), Some("planner"), "saved");
        let slugs: Vec<_> = task.agents.iter().map(|a| a.slug.as_str()).collect();
        assert_eq!(slugs, ["planner"]);
        assert_eq!(linked(task, "planning"), Some(skill.clone()));
    });
    assert_eq!(State::load().tasks[0].agents[0].slug, "planner");
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find_by_label("Agent: Planner").is_some());
        w.click("chat-agent", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        menu_rows(cx, window),
        [
            "Designer",
            "Planner",
            "No agent",
            "Edit Planner…",
            "New agent…"
        ]
    );
    press_in(window)(cx, &["n", "enter"]);
    cx.read(|cx| {
        assert!(chat.read(cx).meta.agent.is_none());
        let task = shark.read(cx).task("pick").unwrap();
        assert!(task.agents.is_empty(), "no chat uses it any more");
        assert_eq!(linked(task, "planning"), None);
    });
}

/// After the first message the button hands off instead: the other agents, the next one first,
/// and the chat keeps its agent.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn started_chat_agent_menu_only_hands_off_to_other_agents(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let mut agents = agents();
    agents.insert(
        1,
        Agent {
            slug: "reviewer".into(),
            name: "Reviewer".into(),
            ..Default::default()
        },
    );
    let task = task_for("started", Some(&agents[0]));
    write_transcript(&task.chats[0].id, &TURN);
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("started", w, cx));
    let chat = shown_chat(cx, &shark);
    until(cx, window, |_, cx| chat.read(cx).turns() == 1).await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("chat-agent", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        menu_rows(cx, window),
        [
            "Hand off to. Opens a new chat with this transcript",
            "Planner",
            "Reviewer",
            "Edit Designer…",
            "New agent…",
        ]
    );
    cx.update_window(window, |_, w, _| {
        let mut hint = |ix: usize| {
            w.within("menu-parent")
                .try_find(("menu-hint", ix))
                .is_some()
        };
        assert!(hint(1) && !hint(2), "only the next agent is hinted");
    })
    .unwrap();
    press_in(window)(cx, &["escape"]);
    let weak = chat.downgrade();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.set_chat_agent(&weak, Some("planner".into()), w, cx)
    });
    cx.read(|cx| {
        assert_eq!(chat.read(cx).meta.agent.as_deref(), Some("designer"));
        let task = shark.read(cx).task("started").unwrap();
        assert_eq!(task.chats.len(), 1, "a refused pick isn't a handoff");
        assert_eq!(task.chats[0].agent.as_deref(), Some("designer"));
    });
}

/// A handoff opens a new tab for the receiver with the source's transcript (and approved plan)
/// attached in its composer. The source chat, the task and its docs stay as they were; the
/// receiver joins the task's agents and its skills are linked beside the source's.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_opens_receiver_chat_with_transcript_attached(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let design = agent_skill("designer", "design");
    let planning = agent_skill("planner", "planning");
    let mut task = task_for("handoff", Some(&agents[0]));
    task.chats[0].title = "Design onboarding".into();
    let source = task.chats[0].id.clone();
    write_transcript(&source, &TURN);
    let plan = crate::plan::store_path(&source);
    fs::create_dir_all(plan.parent().unwrap()).unwrap();
    fs::write(plan, r#"{"versions":[{"id":"first","text":"Approved design"},{"id":"second","text":"Unapproved revision"}],"approved":0}"#).unwrap();
    let docs = fs::read(task.dir().join("CLAUDE.md")).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("handoff", w, cx));
    let from = shown_chat(cx, &shark);
    until(cx, window, |_, cx| from.read(cx).turns() == 1).await;
    let before = cx.read(|cx| shark.read(cx).task("handoff").unwrap().clone());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.hand_off("handoff", "planner", Some(source.clone()), w, cx)
    });
    until(cx, window, |_, cx| {
        shark
            .read(cx)
            .active_chat()
            .is_some_and(|c| c.read(cx).meta.id != source)
    })
    .await;
    let mention = "@\".context/attachments/Design onboarding transcript.md\"";
    cx.read(|cx| {
        let s = shark.read(cx);
        let task = s.task("handoff").unwrap();
        assert_eq!(task.chats.len(), 2);
        assert_eq!(task.active_chat, 1, "the new tab is shown");
        let new = s.active_chat().unwrap().read(cx);
        assert_eq!(new.meta.agent.as_deref(), Some("planner"));
        assert_eq!(new.meta.model.as_deref(), Some(crate::models::MODELS[1]));
        assert_eq!(new.meta.effort.as_deref(), Some("low"));
        assert!(new.agent_open(), "nothing is sent for the user");
        let text = new.composer.read(cx).value().to_string();
        assert!(text.contains(mention), "{text}");
        assert_eq!(
            task.chats[0], before.chats[0],
            "the source chat is untouched"
        );
        assert_eq!(from.read(cx).meta.agent.as_deref(), Some("designer"));
        assert_eq!(
            (
                &task.title,
                &task.branch,
                task.repos.len(),
                task.closed.len()
            ),
            (
                &before.title,
                &before.branch,
                before.repos.len(),
                before.closed.len()
            )
        );
        let slugs: Vec<_> = task.agents.iter().map(|a| a.slug.as_str()).collect();
        assert_eq!(slugs, ["designer", "planner"]);
        assert_eq!(linked(task, "design"), Some(design.clone()));
        assert_eq!(linked(task, "planning"), Some(planning.clone()));
    });
    let dir = tasks_dir().join("handoff");
    assert_eq!(fs::read(dir.join("CLAUDE.md")).unwrap(), docs);
    let md = fs::read_to_string(dir.join(".context/attachments/Design onboarding transcript.md"))
        .unwrap();
    assert!(md.starts_with("# Transcript: Design onboarding"), "{md}");
    assert!(
        md.contains("Design onboarding") && md.contains("Use three steps"),
        "{md}"
    );
    assert!(
        md.contains("Approved design") && !md.contains("Unapproved revision"),
        "{md}"
    );
    assert_eq!(
        State::load().tasks[0].chats[1].agent.as_deref(),
        Some("planner")
    );
}

/// cmd-t keeps the agent of the chat it opens from, on that agent's defaults.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_chat_keeps_the_shown_chats_agent(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![task_for("cmd-t", Some(&agents[1]))],
            agents,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("cmd-t", w, cx));
    let first = shown_chat(cx, &shark);
    press_in(window)(cx, &["cmd-t"]);
    let new = shown_chat(cx, &shark);
    cx.read(|cx| {
        assert_ne!(new.read(cx).meta.id, first.read(cx).meta.id);
        let meta = &new.read(cx).meta;
        assert_eq!(meta.agent.as_deref(), Some("planner"));
        assert_eq!(meta.model.as_deref(), Some(crate::models::MODELS[1]));
        assert_eq!(meta.effort.as_deref(), Some("low"));
        assert_eq!(shark.read(cx).task("cmd-t").unwrap().chats.len(), 2);
    });
}

/// After a finished turn the bar offers the agent's next one; dismissed, it waits for the next
/// turn. Its button and cmd-alt-enter each hand off to that agent in a new tab.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_bar_and_shortcut_hand_off_to_next_agent(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let task = task_for("bar", Some(&agents[0]));
    write_transcript(&task.chats[0].id, &TURN);
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![task],
            agents,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("bar", w, cx));
    let source = shown_chat(cx, &shark);
    until(cx, window, |_, cx| source.read(cx).turns() == 1).await;
    let turn = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            source.update(cx, |c, cx| {
                c.play(crate::harness::Ev::User("Design".into()), cx);
                c.play(crate::harness::Ev::Text("Use three steps".into()), cx);
                c.play(
                    crate::harness::Ev::Result {
                        is_error: false,
                        text: String::new(),
                        secs: 1,
                    },
                    cx,
                );
            })
        })
    };
    let chats = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            let task = shark.read(cx).task("bar").unwrap();
            (task.chats.iter())
                .map(|c| c.agent.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        })
    };
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("handoff-bar").is_some(), "after a finished turn");
        w.click("handoff-dismiss", cx);
        w.render_frame(cx);
        assert!(w.try_find("handoff-bar").is_none());
    })
    .unwrap();
    turn(cx);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("handoff-bar").is_some());
        w.click("handoff-go", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        shark.read(cx).task("bar").unwrap().chats.len() == 2
    })
    .await;
    assert_eq!(chats(cx), ["designer", "planner"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_ne!(
            s.active_chat(),
            Some(&source),
            "the receiver's tab is shown"
        );
        let next = source.read(cx).next_agent.as_ref().map(|a| a.slug.clone());
        assert_eq!(
            next.as_deref(),
            Some("planner"),
            "the source still offers it"
        );
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.select_chat(0, w, cx);
        source.update(cx, |c, cx| c.focus_composer(w, cx));
    });
    press_in(window)(cx, &["cmd-alt-enter"]);
    until(cx, window, |_, cx| {
        shark.read(cx).task("bar").unwrap().chats.len() == 3
    })
    .await;
    assert_eq!(chats(cx), ["designer", "planner", "planner"]);
}

/// A missing receiver or task is refused with a toast and changes nothing; a chat with no agent
/// can still hand off.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_refuses_missing_receiver_and_works_from_chat_without_agent(
    cx: &mut TestAppContext,
) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: agents(),
            tasks: vec![task_for("handoff", None)],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task_named("handoff", w, cx);
        s.hand_off("handoff", "missing", None, w, cx);
        s.hand_off("gone", "planner", None, w, cx);
    });
    assert_eq!(toasts(cx, window), 2);
    cx.read(|cx| {
        let task = shark.read(cx).task("handoff").unwrap();
        assert_eq!(task.chats.len(), 1);
        assert!(task.agents.is_empty());
    });
    assert!(
        !tasks_dir().join("handoff/.context").exists(),
        "no transcript written"
    );
    let source = cx.read(|cx| shark.read(cx).task("handoff").unwrap().chats[0].id.clone());
    write_transcript(&source, &TURN);
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.hand_off("handoff", "planner", None, w, cx)
    });
    until(cx, window, |_, cx| {
        shark.read(cx).task("handoff").unwrap().chats.len() == 2
    })
    .await;
    cx.read(|cx| {
        let task = shark.read(cx).task("handoff").unwrap();
        assert!(task.chats[0].agent.is_none());
        assert_eq!(task.chats[1].agent.as_deref(), Some("planner"));
    });
}

/// The transcript is read from disk, so a plan that failed to save is retried first; while it
/// still can't, the handoff is refused.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_retries_unsaved_source_plan_and_refuses_until_it_can_save(
    cx: &mut TestAppContext,
) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![task_for("handoff", Some(&agents[0]))],
            agents,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.open_task_named("handoff", w, cx));
    let (id, plan) = cx.read(|cx| {
        let c = shark.read(cx).active_chat().unwrap().read(cx);
        (c.meta.id.clone(), c.plan.clone())
    });
    write_transcript(&id, &TURN);
    let path = crate::plan::store_path(&id);
    fs::create_dir_all(&path).unwrap();
    cx.update(|cx| {
        plan.update(cx, |p, cx| {
            p.add("approved".into(), "Approved design in memory".into(), cx);
            p.approve(cx);
            assert!(p.storage_error().is_some());
        })
    });
    on(cx, &|s, w, cx| {
        s.hand_off("handoff", "planner", Some(id.clone()), w, cx)
    });
    cx.read(|cx| {
        let task = shark.read(cx).task("handoff").unwrap();
        assert_eq!(task.chats.len(), 1);
        assert!(plan.read(cx).storage_error().is_some());
    });
    assert_eq!(toasts(cx, window), 1);
    fs::remove_dir(&path).unwrap();
    on(cx, &|s, w, cx| {
        s.hand_off("handoff", "planner", Some(id.clone()), w, cx)
    });
    until(cx, window, |_, cx| {
        shark.read(cx).task("handoff").unwrap().chats.len() == 2
    })
    .await;
    cx.read(|cx| assert!(plan.read(cx).storage_error().is_none()));
    let md =
        fs::read_to_string(tasks_dir().join("handoff/.context/attachments/Design transcript.md"))
            .unwrap();
    assert!(md.contains("## Plan\n\nApproved design in memory"), "{md}");
}
