use super::*;
use crate::state::{Agent, AgentLink, Attachment};

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
            ..Default::default()
        },
    ]
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_preserves_task_and_carries_unopened_chat_reply(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("handoff"),
        "handoff",
        "Design",
        Some(AgentLink::from(&agents[0])),
    )
    .unwrap();
    let source = task.chats[0].id.clone();
    let transcript = crate::transcript::transcript_path(&source);
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, concat!(
        "{\"type\":\"user\",\"message\":{\"content\":\"Design onboarding\"}}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Use three steps\"}]}}\n"
    )).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.hand_off("handoff", "planner", None, w, cx)
    });
    until(cx, window, |_, cx| {
        shark.read(cx).state.tasks[0].chats.len() == 2
    })
    .await;
    cx.read(|cx| {
        let s = shark.read(cx);
        let task = &s.state.tasks[0];
        assert_eq!(task.agent.as_ref().unwrap().slug, "planner");
        assert_eq!(task.chats[0].agent.as_deref(), Some("designer"));
        let chat = &task.chats[1];
        assert_eq!(chat.agent.as_deref(), Some("planner"));
        assert_eq!(chat.model.as_deref(), Some(crate::models::MODELS[1]));
        assert_eq!(chat.handoff.as_ref().unwrap().text, "Use three steps");
        assert!(
            fs::read_to_string(task.dir().join("CLAUDE.md"))
                .unwrap()
                .contains("## Agent: Planner")
        );
        assert_eq!(task.chats[0].id, source);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_refuses_missing_current_and_attaching_targets(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("handoff"),
        "handoff",
        "Design",
        Some(AgentLink::from(&agents[0])),
    )
    .unwrap();
    let before = fs::read(task.dir().join("CLAUDE.md")).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.hand_off("handoff", "missing", None, w, cx);
        s.hand_off("handoff", "designer", None, w, cx);
        s.task_mut("handoff")
            .unwrap()
            .attaching
            .push(Attachment::fresh(&repos[0], "main", ""));
        s.hand_off("handoff", "planner", None, w, cx);
        let task = s.task("handoff").unwrap();
        assert_eq!(task.agent.as_ref().unwrap().slug, "designer");
        assert_eq!(task.chats.len(), 1);
        assert_eq!(fs::read(task.dir().join("CLAUDE.md")).unwrap(), before);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_approved_plan_switches_skills_and_attaches_default_repo(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let mut agents = agents();
    agents[1].repos = vec!["api".into()];
    for (slug, skill) in [("designer", "design"), ("planner", "planning")] {
        let dir = crate::state::agent_skills_dir(slug).join(skill);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("# {skill}")).unwrap();
    }
    let task = git::create_empty_task(
        &tasks_dir().join("handoff"),
        "handoff",
        "Design",
        Some(AgentLink::from(&agents[0])),
    )
    .unwrap();
    let plan = crate::plan::store_path(&task.chats[0].id);
    fs::create_dir_all(plan.parent().unwrap()).unwrap();
    fs::write(plan, r#"{"versions":[{"id":"first","text":"Approved design"},{"id":"second","text":"Unapproved revision"}],"approved":0}"#).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            repos,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.hand_off("handoff", "planner", None, w, cx)
    });
    until(cx, window, |_, cx| {
        shark.read(cx).state.tasks[0].chats.len() == 2
    })
    .await;
    cx.read(|cx| {
        let s = shark.read(cx);
        let task = &s.state.tasks[0];
        assert_eq!(
            task.chats[1].handoff.as_ref().unwrap().text,
            "Approved design"
        );
        assert!(!task.attaching.is_empty() || task.repos.iter().any(|r| r.name == "api"));
        assert!(task.dir().join(".claude/skills/planning").exists());
        assert!(!task.dir().join(".claude/skills/design").exists());
    });
    until(cx, window, |_, cx| {
        let task = &shark.read(cx).state.tasks[0];
        task.attaching.is_empty() && task.repos.iter().any(|r| r.name == "api")
    })
    .await;
    assert!(tasks_dir().join("handoff/api/.git").exists());
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_bar_dismisses_until_next_turn_and_button_moves_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("handoff"),
        "handoff",
        "Design",
        Some(AgentLink::from(&agents[0])),
    )
    .unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_now(0, w, cx));
    cx.run_until_parked();
    let source = cx.read(|cx| shark.read(cx).active_chat().unwrap().clone());
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
    turn(cx);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("handoff-bar").is_some());
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
        shark.read(cx).state.tasks[0].chats.len() == 2
    })
    .await;
    cx.read(|cx| assert!(source.read(cx).next_agent.is_none()));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_composer_shortcut_moves_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("handoff"),
        "handoff",
        "Design",
        Some(AgentLink::from(&agents[0])),
    )
    .unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_now(0, w, cx));
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.active_chat().unwrap().clone().update(cx, |c, cx| {
            c.play(crate::harness::Ev::User("Design".into()), cx);
            c.play(
                crate::harness::Ev::Result {
                    is_error: false,
                    text: String::new(),
                    secs: 1,
                },
                cx,
            );
            c.focus_composer(w, cx);
        })
    });
    press_in(window)(cx, &["cmd-alt-enter"]);
    until(cx, window, |_, cx| {
        shark.read(cx).state.tasks[0].chats.len() == 2
    })
    .await;
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).state.tasks[0].agent.as_ref().unwrap().slug,
            "planner"
        )
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn closing_unsent_handoff_preserves_quote_for_reopen_and_restart(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let agents = agents();
    let mut task = git::create_empty_task(
        &tasks_dir().join("handoff"),
        "handoff",
        "Design",
        Some(AgentLink::from(&agents[1])),
    )
    .unwrap();
    let quote = crate::state::Handoff {
        from: "Designer".into(),
        title: "Onboarding ideas".into(),
        text: "Use three steps".into(),
    };
    task.chats[0].handoff = Some(quote.clone());
    let id = task.chats[0].id.clone();
    assert!(!crate::transcript::transcript_path(&id).exists());
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.open_task_now(0, w, cx);
        s.close_chat(w, cx);
    });
    let saved = State::load();
    assert_eq!(saved.tasks[0].closed.len(), 1);
    assert_eq!(saved.tasks[0].closed[0].id, id);
    assert_eq!(saved.tasks[0].closed[0].handoff.as_ref(), Some(&quote));
    assert_ne!(
        saved.tasks[0].chats[0].id, id,
        "replacement must have a fresh identity"
    );
    on(cx, &|s, w, cx| s.reopen_chat(None, w, cx));
    cx.read(|cx| {
        let c = shark.read(cx).active_chat().unwrap().read(cx);
        assert_eq!(c.meta.id, id);
        assert_eq!(c.meta.handoff.as_ref(), Some(&quote));
        assert_eq!(c.meta.agent.as_deref(), Some("planner"));
    });
    let saved = State::load();
    let task = &saved.tasks[0];
    assert!(task.closed.is_empty());
    assert_eq!(task.chats[task.active_chat].handoff.as_ref(), Some(&quote));
}
