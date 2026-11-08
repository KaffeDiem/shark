use super::*;
use crate::state::{Agent, AgentLink};

fn agents() -> Vec<Agent> {
    vec![
        Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            ..Default::default()
        },
        Agent {
            slug: "planner".into(),
            name: "Planner".into(),
            ..Default::default()
        },
    ]
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_next_chip_sets_clears_and_deleted_target_is_cleared(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("next-chip"),
        "next-chip",
        "Next",
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
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task_named("next-chip", w, cx);
        s.open_agent("designer", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-next", cx);
    })
    .unwrap();
    press_in(window)(cx, &["p", "enter"]);
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).state.agents[0].next.as_deref(),
            Some("planner")
        )
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-next", cx);
    })
    .unwrap();
    press_in(window)(cx, &["n", "enter"]);
    cx.read(|cx| assert!(shark.read(cx).state.agents[0].next.is_none()));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-next", cx);
    })
    .unwrap();
    press_in(window)(cx, &["p", "enter"]);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.delete_agent("planner", w, cx));
    press_in(window)(cx, &["enter"]);
    cx.run_until_parked();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents.len(), 1);
        assert!(s.state.agents[0].next.is_none());
        assert!(
            s.chats
                .values()
                .all(|chat| chat.read(cx).next_agent.is_none())
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_menu_prefers_next_and_unsaved_editor_cancel_preserves_task(
    cx: &mut TestAppContext,
) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let mut agents = agents();
    agents[0].next = Some("planner".into());
    let tasks = ["editing", "handoff"].map(|name| {
        git::create_empty_task(
            &tasks_dir().join(name),
            name,
            name,
            Some(AgentLink::from(&agents[0])),
        )
        .unwrap()
    });
    let path = tasks[0].dir().join("notes.md");
    fs::write(&path, "original").unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks: tasks.into(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let ws = s.task("handoff").unwrap();
        let Entry::Sub { entries, .. } = super::super::menus::handoff_submenu(s, ws, cx).unwrap()
        else {
            panic!("submenu expected")
        };
        assert!(
            matches!(&entries[0], Entry::Item { label,hint,.. } if label.as_ref()=="Planner" && hint.as_deref()==Some("next"))
        );
        s.open_task_named("editing", w, cx);
        s.open_path(path.to_string_lossy().into_owned(), w, cx);
    });
    until(cx, window, |_, cx| {
        shark
            .read(cx)
            .editor
            .as_ref()
            .is_some_and(|e| e.read(cx).input.read(cx).value() == "original")
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.editor.as_ref().unwrap().update(cx, |e, cx| {
            e.input.update(cx, |i, cx| i.set_value("unsaved", w, cx));
            e.dirty = true;
        });
        s.hand_off("handoff", "planner", None, w, cx);
    });
    until(cx, window, |w, cx| w.has_active_dialog(cx)).await;
    press_in(window)(cx, &["escape"]);
    cx.run_until_parked();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("editing"));
        assert_eq!(
            s.task("handoff").unwrap().agent.as_ref().unwrap().slug,
            "designer"
        );
        assert!(s.editor.as_ref().unwrap().read(cx).dirty);
    });
    assert_eq!(fs::read_to_string(path).unwrap(), "original");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_save_failure_restores_agent_chats_and_instructions(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("handoff-save"),
        "handoff-save",
        "Save",
        Some(AgentLink::from(&agents[0])),
    )
    .unwrap();
    let docs = task.dir().join("CLAUDE.md");
    let original_docs = fs::read(&docs).unwrap();
    let original_chats = task.chats.len();
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
        s.open_task_named("handoff-save", w, cx);
        s.state.read_only = true;
        s.hand_off("handoff-save", "planner", None, w, cx);
    });
    until(cx, window, |_, cx| {
        shark.read(cx).state.save_error.borrow().is_some()
    })
    .await;
    cx.read(|cx| {
        let task = &shark.read(cx).state.tasks[0];
        assert_eq!(task.agent.as_ref().unwrap().slug, "designer");
        assert_eq!(task.chats.len(), original_chats);
    });
    assert_eq!(fs::read(docs).unwrap(), original_docs);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_target_follows_receiver_name_and_icon_edits(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let mut agents = agents();
    agents[0].next = Some("planner".into());
    let task = git::create_empty_task(
        &tasks_dir().join("target-edits"),
        "target-edits",
        "Target",
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
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task_named("target-edits", w, cx);
        s.rename_agent("planner", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.input("Implementation planner", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        let target = s
            .chats
            .values()
            .next()
            .unwrap()
            .read(cx)
            .next_agent
            .as_ref()
            .unwrap();
        assert_eq!(target.name, "Implementation planner");
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| s.pick_agent_icon("planner", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.input("star", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.agent("planner").unwrap().icon,
            crate::state::AgentIcon::from("star")
        );
        let target = s
            .chats
            .values()
            .next()
            .unwrap()
            .read(cx)
            .next_agent
            .as_ref()
            .unwrap();
        assert_eq!(target.icon, crate::state::AgentIcon::from("star"));
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn handoff_historical_chat_agents_reserve_deleted_slugs(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let task = Task {
        name: "history".into(),
        chats: vec![ChatMeta {
            id: "history".into(),
            agent: Some("designer".into()),
            ..Default::default()
        }],
        closed: vec![ChatMeta {
            id: "history~2".into(),
            agent: Some("designer-2".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer-3");
        assert_eq!(s.state.tasks[0].chats[0].agent.as_deref(), Some("designer"));
        assert_eq!(
            s.state.tasks[0].closed[0].agent.as_deref(),
            Some("designer-2")
        );
    });
}

#[gpui_kit::test]
async fn agent_chat_identity_keeps_its_own_space_beside_status(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let agents = agents();
    let task = git::create_empty_task(
        &tasks_dir().join("identity-status"),
        "identity-status",
        "Identity status",
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
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("identity-status", w, cx));
    let chat = cx.read(|cx| shark.read(cx).chats.values().next().unwrap().clone());
    for scale in [1., 2.] {
        cx.simulate_window_scale_factor_change(window, scale);
        for working in [false, true] {
            on_app(window, shark.clone())(cx, &|s, _, cx| {
                let id = chat.read(cx).meta.id.clone();
                s.unread.insert(id);
                chat.update(cx, |c, cx| {
                    c.busy = working;
                    cx.notify();
                });
                cx.notify();
            });
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let identity = w.find("chat-agent-identity").bounds();
                let status = w.find("chat-agent-status").bounds();
                assert_eq!(identity.size, size(px(16.), px(16.)));
                assert!(identity.right() <= status.left());
            })
            .unwrap();
        }
    }
}
