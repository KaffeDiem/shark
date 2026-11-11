use super::*;
use crate::state::{Agent, AgentLink};

/// Clicks the open menu's row labelled `label`.
fn click_menu_row(w: &mut Window, label: &str, cx: &mut App) {
    w.render_frame(cx);
    let row = w.within("menu-parent").find_by_label(label);
    let id = row.path().last().unwrap().clone();
    w.within("menu-parent").click(id, cx);
}

/// Whether the agent dialog is on screen.
fn dialog_shown(w: &mut Window, cx: &mut App) -> bool {
    w.render_frame(cx);
    w.try_find("agent-editor").is_some()
}

fn designer() -> Agent {
    Agent {
        slug: "designer".into(),
        name: "Designer".into(),
        ..Default::default()
    }
}

/// A new agent exists once Create (or Enter in its name) makes it, with the instructions typed
/// before; an empty name is refused with the dialog and its text kept; Cancel and Escape make
/// nothing.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_agent_dialog_creates_on_create_or_enter_and_cancels_cleanly(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            sections: vec![Section {
                name: "Designer".into(),
                icon: "star".into(),
                collapsed: false,
                tasks: vec![],
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let (on, press) = (on_app(window, shark.clone()), press_in(window));
    let agents = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            (shark.read(cx).state.agents.iter())
                .map(|a| (a.slug.clone(), a.name.clone(), a.instructions.clone()))
                .collect::<Vec<_>>()
        })
    };
    on(cx, &|s, w, cx| s.new_agent_dialog(w, cx));
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx));
        assert_eq!(w.find("agent-create").disabled(), Some(true));
        for id in ["agent-icon", "agent-skills", "agent-next", "agent-delete"] {
            assert!(w.try_find(id).is_none(), "{id} waits for the agent");
        }
        w.click("agent-instructions", cx);
        w.input("Always use Mobbin", cx);
        w.click("agent-name", cx);
    })
    .unwrap();
    press(cx, &["enter"]);
    assert!(agents(cx).is_empty(), "an empty name makes no agent");
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx), "a refused Enter keeps the dialog");
        let editor = shark
            .read(cx)
            .agent_editor
            .clone()
            .unwrap()
            .upgrade()
            .unwrap();
        let instructions = editor.read(cx).instructions.read(cx).value();
        assert_eq!(instructions, "Always use Mobbin", "and what was typed");
        w.input("Designer", cx);
        w.render_frame(cx);
        assert_ne!(w.find("agent-create").disabled(), Some(true));
        w.click("agent-create", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        agents(cx),
        [(
            "designer".into(),
            "Designer".into(),
            "Always use Mobbin".into()
        )]
    );
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
    on(cx, &|s, w, cx| s.new_agent_dialog(w, cx));
    cx.update_window(window, |_, w, cx| w.input("Planner", cx))
        .unwrap();
    press(cx, &["enter"]);
    assert_eq!(
        agents(cx)[1],
        ("planner".into(), "Planner".into(), "".into())
    );
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
    for close in ["agent-cancel", "escape"] {
        on(cx, &|s, w, cx| s.new_agent_dialog(w, cx));
        cx.update_window(window, |_, w, cx| {
            w.input("Writer", cx);
            if close == "agent-cancel" {
                w.click(close, cx);
            }
        })
        .unwrap();
        if close == "escape" {
            press(cx, &["escape"]);
        }
        cx.update_window(window, |_, w, cx| {
            assert!(!w.has_active_dialog(cx), "{close} closes the dialog")
        })
        .unwrap();
    }
    assert_eq!(agents(cx).len(), 2, "Cancel and Escape make nothing");
    cx.read(|cx| {
        let sections = &shark.read(cx).state.sections;
        assert_eq!(sections.len(), 1, "agents leave task groups alone");
        assert_eq!(sections[0].name, "Designer");
    });
    assert_eq!(State::load().agents[0].instructions, "Always use Mobbin");
}

/// The edit dialog saves the name and instructions as they change. An emptied name keeps the
/// saved one, the slug never changes, Enter in the name moves to the instructions, and only Done
/// closes.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn edit_agent_dialog_saves_at_once_and_done_closes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                instructions: "Old instructions".into(),
                ..designer()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let press = press_in(window);
    let saved = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            let a = &shark.read(cx).state.agents[0];
            (a.slug.clone(), a.name.clone(), a.instructions.clone())
        })
    };
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx));
        assert_eq!(w.find("agent-icon").bounds().size, size(px(40.), px(40.)));
        for id in [
            "agent-create",
            "agent-model",
            "agent-effort",
            "agent-attach",
        ] {
            assert!(w.try_find(id).is_none(), "{id} in the edit dialog");
        }
        assert_eq!(w.find("agent-skills").label(), Some("0 skills"));
        assert_eq!(w.find("agent-next").label(), Some("Hands off to nobody"));
        w.click("agent-name", cx);
    })
    .unwrap();
    press(cx, &["cmd-a", "backspace"]);
    assert_eq!(saved(cx).1, "Designer", "an empty name keeps the saved one");
    cx.update_window(window, |_, w, cx| w.input("Design team", cx))
        .unwrap();
    assert_eq!(
        saved(cx),
        (
            "designer".into(),
            "Design team".into(),
            "Old instructions".into()
        )
    );
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx), "Enter in the name doesn't close");
        let editor = shark
            .read(cx)
            .agent_editor
            .clone()
            .unwrap()
            .upgrade()
            .unwrap();
        let instructions = editor.read(cx).instructions.clone();
        assert!(instructions.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();
    press(cx, &["cmd-a"]);
    cx.update_window(window, |_, w, cx| w.input("New instructions", cx))
        .unwrap();
    assert_eq!(saved(cx).2, "New instructions");
    cx.update_window(window, |_, w, cx| {
        w.click("agent-done", cx);
        assert!(!w.has_active_dialog(cx));
    })
    .unwrap();
    let agent = &State::load().agents[0];
    assert_eq!(
        (
            agent.slug.as_str(),
            agent.name.as_str(),
            agent.instructions.as_str()
        ),
        ("designer", "Design team", "New instructions")
    );
}

/// The skills and handoff menus open above the agent dialog: clicking their rows, or dismissing
/// them, leaves the dialog open.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_dialog_menus_keep_the_dialog_open(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let planner = Agent {
        slug: "planner".into(),
        name: "Planner".into(),
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![designer(), planner],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let press = press_in(window);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-next", cx);
        click_menu_row(w, "Planner", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert_eq!(
            shark.read(cx).state.agents[0].next.as_deref(),
            Some("planner")
        );
        assert!(dialog_shown(w, cx), "a menu row click keeps the dialog");
        assert!(w.try_find("menu-parent").is_none());
        assert_eq!(w.find("agent-next").label(), Some("Hands off to Planner"));
        let skill = crate::state::agent_skills_dir("designer").join("research");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# Research").unwrap();
        w.click("agent-skills", cx);
        click_menu_row(w, "Refresh skills", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx));
        assert_eq!(w.find("agent-skills").label(), Some("1 skill"));
        w.click("agent-skills", cx);
        w.render_frame(cx);
        assert!(w.try_find("menu-parent").is_some());
    })
    .unwrap();
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(w.try_find("menu-parent").is_none());
        assert!(dialog_shown(w, cx), "Escape closes only the menu");
    })
    .unwrap();
}

/// "New agent…" in an unstarted chat's agent menu makes the agent and puts it on that chat.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_agent_from_a_chat_menu_goes_on_that_chat(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let task = git::create_empty_task(&tasks_dir().join("pick"), "pick", "Pick", None).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_task_named("pick", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("chat-agent", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| click_menu_row(w, "New agent…", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx));
        w.input("Designer", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        let s = shark.read(cx);
        assert_eq!(s.state.agents[0].slug, "designer");
        let chat = s.active_chat().unwrap().read(cx);
        assert_eq!(chat.meta.agent.as_deref(), Some("designer"));
        assert_eq!(s.task("pick").unwrap().agents[0].slug, "designer");
        w.render_frame(cx);
        assert_eq!(w.find("chat-agent").label(), Some("Agent: Designer"));
    })
    .unwrap();
}

/// "New agent…" in Home's agent picker makes the agent and picks it for Home's task.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_agent_from_home_picker_is_picked_on_home(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![designer()],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("fix spacing", w, cx));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("home-agent-picker", cx);
        w.input("New agent", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx));
        w.input("Planner", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        let s = shark.read(cx);
        assert_eq!(s.state.agents[1].slug, "planner");
        assert_eq!(s.home.prompt.read(cx).value(), "@planner fix spacing");
    })
    .unwrap();
}

/// Delete agent… in the dialog asks, then removes the agent and closes every dialog. A chat
/// using it doesn't block deletion and goes on without it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agents_do_not_create_sections_and_deleting_one_a_chat_uses_succeeds(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer");
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer-2");
        assert!(s.state.sections.is_empty());
        let link = AgentLink::from(&s.state.agents[0]);
        let task = git::create_empty_task(
            &tasks_dir().join("pending"),
            "pending",
            "Pending",
            Some(link),
        )
        .unwrap();
        s.state.tasks.push(task);
        s.open_task_now(0, w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let agent = w.find("chat-agent");
        assert_eq!(agent.label().as_deref(), Some("Agent: Designer"));
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        assert!(
            s.can_delete_agent("designer", w, cx),
            "a chat using it doesn't block deletion"
        );
        s.open_agent("designer", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-delete", cx);
        assert!(w.has_active_dialog(cx));
    })
    .unwrap();
    cx.read(|cx| assert_eq!(shark.read(cx).state.agents.len(), 2, "asks first"));
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx), "no dialog outlives the agent");
        let s = shark.read(cx);
        assert_eq!(s.state.agents.len(), 1);
        assert_eq!(s.state.agents[0].slug, "designer-2");
        let task = &s.state.tasks[0];
        assert!(task.agents.is_empty(), "its snapshot goes with it");
        assert_eq!(task.chats[0].agent.as_deref(), Some("designer"));
        w.render_frame(cx);
        assert_eq!(
            w.find("chat-agent").label(),
            Some("Agent: none"),
            "the chat goes on without it"
        );
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        assert!(!s.can_delete_agent("../other", w, cx));
        let support = crate::state::support_dir();
        let elsewhere = support.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, support.join("agents")).unwrap();
        assert!(!s.can_delete_agent("designer-2", w, cx));
        fs::remove_file(support.join("agents")).unwrap();
        s.creating
            .insert("spacing".into(), Some("designer-2".into()));
        assert!(!s.can_delete_agent("designer-2", w, cx));
        s.creating.insert("standalone".into(), None);
        s.creating.remove("spacing");
        assert!(s.can_delete_agent("designer-2", w, cx));
        s.creating.clear();
    });
    on(cx, &|s, w, cx| {
        assert_eq!(
            s.create_named_agent("Designer", cx).unwrap(),
            "designer-3",
            "a chat still names the deleted agent's slug"
        );
        s.state.agents.pop();
        let deleted = AgentLink {
            slug: "designer".into(),
            ..Default::default()
        };
        s.new_task(None, None, Some(deleted.clone()), w, cx);
        s.create_task_on(
            "spacing".into(),
            vec![],
            vec![],
            vec![],
            None,
            None,
            "Spacing".into(),
            Some(deleted),
            false,
            w,
            cx,
        );
        assert_eq!(s.state.tasks.len(), 1);
        assert!(
            s.creating.is_empty(),
            "stale dialogs cannot start a deleted agent's task"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn refreshing_skills_links_them_into_tasks_whose_chats_use_the_agent(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let link = AgentLink {
        slug: "designer".into(),
        name: "Designer".into(),
        ..Default::default()
    };
    let task = git::create_empty_task(
        &tasks_dir().join("current"),
        "current",
        "Current task",
        Some(link),
    )
    .unwrap();
    let unrelated =
        git::create_empty_task(&tasks_dir().join("other"), "other", "Other task", None).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                icon: "folder".into(),
                ..Default::default()
            }],
            tasks: vec![task, unrelated],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task_now(1, w, cx);
        s.open_agent("designer", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        assert!(dialog_shown(w, cx));
        let skill = crate::state::agent_skills_dir("designer").join("research");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# Research").unwrap();
        // The skills chip's menu: Refresh skills links the new skill into the agent's tasks.
        w.click("agent-skills", cx);
        click_menu_row(w, "Refresh skills", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        let skill = crate::state::agent_skills_dir("designer").join("research");
        for folder in [".agents/skills", ".claude/skills"] {
            assert_eq!(
                fs::read_link(tasks_dir().join("current").join(folder).join("research")).unwrap(),
                skill
            );
            assert!(
                !tasks_dir()
                    .join("other")
                    .join(folder)
                    .join("research")
                    .exists()
            );
        }
        let editor = shark
            .read(cx)
            .agent_editor
            .clone()
            .unwrap()
            .upgrade()
            .unwrap();
        assert_eq!(editor.read(cx).skills, ["research"]);
        assert!(dialog_shown(w, cx), "a menu pick keeps the dialog");
        assert_eq!(w.find("agent-skills").label().as_deref(), Some("1 skill"));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_agent_doc_refresh_still_updates_instruction_snapshots(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let agent = Agent {
        slug: "designer".into(),
        name: "Designer".into(),
        instructions: "Old instructions".into(),
        ..Default::default()
    };
    let link = AgentLink::from(&agent);
    let tasks = ["blocked", "healthy"]
        .into_iter()
        .map(|name| {
            git::create_empty_task(&tasks_dir().join(name), name, name, Some(link.clone())).unwrap()
        })
        .collect();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![agent],
            tasks,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task_now(0, w, cx);
        let before = s.state.tasks[0].context()["agent:designer"].clone();
        let blocked = s.state.tasks[0].dir().join("CLAUDE.md");
        fs::remove_file(&blocked).unwrap();
        fs::create_dir(&blocked).unwrap();
        s.state.agents[0].instructions = "New instructions".into();
        assert!(
            s.sync_agent("designer", true, cx)
                .unwrap_err()
                .contains("blocked")
        );
        // Instructions reach chats through the system prompt, not the root docs that failed.
        for task in &s.state.tasks {
            assert_eq!(task.agents[0].instructions, "New instructions");
            let prompt = git::agent_prompt(task, &task.chats[0]).unwrap();
            assert!(prompt.contains("New instructions"));
        }
        assert_ne!(
            s.active_chat().unwrap().read(cx).ws.context()["agent:designer"].fingerprint,
            before.fingerprint
        );
        fs::remove_dir(&blocked).unwrap();
        s.sync_agent("designer", true, cx).unwrap();
        assert!(blocked.is_file(), "a retry rewrites the root docs");
    });
}

/// Typing in the dialog's instructions reaches the chats of open tasks once typing pauses.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn inline_instruction_edits_reach_open_tasks_after_a_pause(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let link = AgentLink {
        slug: "designer".into(),
        name: "Designer".into(),
        ..Default::default()
    };
    let task = git::create_empty_task(&tasks_dir().join("one"), "one", "one", Some(link)).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-instructions", cx);
        w.input("Use tokens", cx);
    })
    .unwrap();
    let synced = |cx: &mut TestAppContext| {
        cx.read(|cx| shark.read(cx).state.tasks[0].agents[0].instructions.clone())
    };
    assert_eq!(synced(cx), "", "no sync while typing");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(synced(cx), "Use tokens");
    cx.read(|cx| {
        let task = &shark.read(cx).state.tasks[0];
        let prompt = git::agent_prompt(task, &task.chats[0]).unwrap();
        assert!(prompt.contains("Use tokens"));
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn task_composer_keeps_shift_enter_as_a_newline(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.home.prompt.update(cx, |p, cx| p.focus(w, cx));
    });
    cx.update_window(window, |_, w, cx| w.input("First line", cx))
        .unwrap();
    press_in(window)(cx, &["shift-enter"]);
    cx.update_window(window, |_, w, cx| w.input("Second line", cx))
        .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.prompt.read(cx).value(), "First line\nSecond line");
        assert!(s.state.tasks.is_empty());
        assert!(s.creating.is_empty());
    });
    on(cx, &|s, w, cx| {
        s.create_named_agent("Designer", cx).unwrap();
        s.home.prompt.update(cx, |p, cx| p.set_value("", w, cx));
        s.prefill_agent_task("designer", w, cx);
    });
    cx.update_window(window, |_, w, cx| w.input("Agent first", cx))
        .unwrap();
    press_in(window)(cx, &["shift-enter"]);
    cx.update_window(window, |_, w, cx| w.input("Agent second", cx))
        .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home.prompt.read(cx).value(),
            "@designer Agent first\nAgent second"
        );
        assert!(s.state.tasks.is_empty());
        assert!(s.creating.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn only_unedited_draft_repos_follow_the_picked_agents_defaults(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.create_named_agent("Designer", cx).unwrap();
        s.update_agent("designer", |a| a.repos = vec!["api".into()], cx);
        s.prefill_agent_task("designer", w, cx);
        assert_eq!(s.home.draft.repos.len(), 1);
        s.update_agent("designer", |a| a.repos.push("web".into()), cx);
        assert_eq!(
            s.home.draft.repos.len(),
            2,
            "untouched pills follow defaults"
        );
        s.home.draft.repos.clear();
        s.home.draft.repos_edited = true;
        s.update_agent("designer", |a| a.repos = vec!["api".into()], cx);
        assert!(
            s.home.draft.repos.is_empty(),
            "edited tray remains independent"
        );
        assert_eq!(s.state.agents[0].repos, ["api"]);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn ui_review_regression_skill_refresh_reaches_the_draft(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_named_agent("Designer", cx).unwrap();
        s.open_agent("designer", w, cx);
        s.enter_home(w, cx);
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("@designer ", w, cx));
        s.draft_composer_event(&crate::task_composer::Event::Change, w, cx);
        let skill = crate::state::agent_skills_dir("designer").join("regression-research");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# Research").unwrap();
        s.sync_agent_skills("designer", cx).unwrap();
    });
    cx.run_until_parked();
    on(cx, &|s, w, cx| {
        s.home.prompt.update(cx, |p, cx| {
            p.set_value("@designer ", w, cx);
            let end = p.value().len();
            p.set_selected_range(end..end, cx);
            p.focus(w, cx);
            p.insert("/regression-res", w, cx);
        });
    });
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        assert!(
            (shark.read(cx).home.prompt.read(cx).value()).ends_with("/regression-research "),
            "refreshed skill must complete in the cached draft"
        );
    });
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn reference_attachment_changes_only_the_draft_and_paste_undo_cancels_pending_reads(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.attach_draft_reference(
            crate::complete::Reference::Pr {
                repo: "api".into(),
                label: "#93 Review".into(),
                pr: git::PullRequest {
                    number: 93,
                    url: "https://github.com/org/api/pull/93".into(),
                    base: "main".into(),
                },
            },
            w,
            cx,
        );
        assert_eq!(s.home.draft.repos.len(), 1);
        assert_eq!(
            s.home.draft.repos[0].pr_label.as_deref(),
            Some("#93 Review")
        );
        assert!(
            matches!(&s.home.draft.repos[0].branch, super::super::task_draft::BranchSpec::Pr(pr) if pr.number == 93)
        );
        assert!(s.state.tasks.is_empty(), "attaching never starts a task");
        let url = "https://linear.app/acme/issue/SHA-42/test".to_string();
        s.paste_draft_reference(1, url, w, cx);
        assert_eq!(
            s.home.draft.staging, 1,
            "a pending full snapshot blocks submit"
        );
        s.home.draft.issues.push(Snapshot {
            id: "unrelated".into(),
            ..Default::default()
        });
        s.undo_draft_reference(1, w, cx);
        assert_eq!(s.home.draft.staging, 0);
        assert_eq!(
            s.home.draft.issues[0].id, "unrelated",
            "undo keeps newer attachments"
        );
        assert!(
            matches!(&s.home.draft.repos[0].branch, super::super::task_draft::BranchSpec::Pr(pr) if pr.number == 93)
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            s.home.draft.issues.len() == 1,
            "cancelled reads cannot reattach after undo"
        );
        assert_eq!(s.home.draft.staging, 0);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_paste_restores_the_draft_after_navigation(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "second".into(),
                name: "Second".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.home.prompt.update(cx, |p, cx| p.focus(w, cx));
    });
    let url = "https://linear.app/acme/issue/SHA-42/test";
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(url.into())));
    cx.update_window(window, |_, w, cx| w.press("cmd-v", cx))
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.open_agent("second", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home.prompt.read(cx).value().as_ref(),
            url,
            "failed paste returns to Home's draft"
        );
        assert_eq!(s.home.draft.staging, 0);
        assert!(s.home.draft.issues.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn reference_suggestions_exclude_other_linear_workspaces(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            linear: Some(crate::linear::Connection {
                org_id: "current".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark)(cx, &|s, _, _| {
        s.home.draft.issues.push(Snapshot {
            org: "prior".into(),
            id: "prior-id".into(),
            identifier: "SHA-42".into(),
            ..Default::default()
        });
        assert_eq!(s.home.draft.issues.len(), 1);
        assert!(
            s.reference_hits().is_empty(),
            "a matching identifier from a prior workspace is not a current suggestion"
        );
    });
}
