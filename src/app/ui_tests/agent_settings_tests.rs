use super::*;
use crate::state::AgentLink;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_settings_creates_and_edits_a_stable_identity(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            sections: vec![Section {
                name: "Designer".into(),
                icon: "star".into(),
                collapsed: false,
                tasks: vec![],
                agent: None,
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.new_agent_dialog(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.input("Designer", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("agent-settings").is_some());
        w.click(("agent-repo", 0usize), cx);
        w.click("agent-instructions", cx);
        w.input("Always use Mobbin to ground your research", cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        let a = &s.state.agents[0];
        assert_eq!(a.slug, "designer");
        assert_eq!(a.repos, ["api"]);
        assert_eq!(a.instructions, "Always use Mobbin to ground your research");
        assert_eq!(
            s.state.sections.len(),
            1,
            "agent creation leaves task groups alone"
        );
        assert_eq!(s.state.sections[0].agent, None);
    });
    cx.update_window(window, |_, w, cx| w.click("agent-name", cx))
        .unwrap();
    press_in(window)(cx, &["cmd-a"]);
    cx.update_window(window, |_, w, cx| w.input("Design team", cx))
        .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents[0].slug, "designer");
        assert_eq!(s.state.agents[0].name, "Design team");
        assert_eq!(
            s.state.sections[0].name, "Designer",
            "agent edits leave task groups alone"
        );
    });
    press_in(window)(cx, &["escape"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("agent-page").is_some(),
            "the empty agent has its own page"
        );
        assert!(w.try_find("agent-empty-tasks").is_some());
        w.click("Instructions & skills", cx);
        w.render_frame(cx);
        assert!(w.try_find("agent-instructions-page").is_some());
        assert!(w.try_find("agent-skills").is_some());
        w.click("History", cx);
        w.render_frame(cx);
        assert!(w.try_find("agent-page").is_some());
        w.click("agent-page-edit", cx);
        w.render_frame(cx);
        assert!(w.try_find("agent-settings").is_some());
    })
    .unwrap();
    let saved = State::load();
    assert_eq!(
        saved.agents[0].instructions,
        "Always use Mobbin to ground your research"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agents_do_not_create_sections_and_deletion_rechecks_open_tasks(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer");
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer-2");
        assert!(s.state.sections.is_empty());
        s.state.sections.push(Section {
            name: "Legacy".into(),
            icon: "folder".into(),
            collapsed: false,
            tasks: vec![],
            agent: Some("designer".into()),
        });
        s.collapsed_agents.insert("designer".into());
        s.open_agent("designer", w, cx);
        s.edit_agent("designer", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("delete-agent", cx);
    })
    .unwrap();
    on(cx, &|s, _, _| {
        s.state.tasks.push(Task {
            name: "pending".into(),
            agent: Some(AgentLink {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }),
            ..Default::default()
        })
    });
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| assert_eq!(shark.read(cx).state.agents.len(), 2));
    on(cx, &|s, w, cx| {
        assert!(!s.can_delete_agent("designer", w, cx));
        s.state.tasks.clear();
        assert!(s.can_delete_agent("designer", w, cx));
        assert!(!s.can_delete_agent("../other", w, cx));
        let support = crate::state::support_dir();
        let elsewhere = support.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, support.join("agents")).unwrap();
        assert!(!s.can_delete_agent("designer", w, cx));
        fs::remove_file(support.join("agents")).unwrap();
        s.creating.insert("designer-one-seahorse".into());
        assert!(!s.can_delete_agent("designer", w, cx));
        s.creating.clear();
        s.sel = SideRow::Agent(1);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("delete-agent", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents.len(), 1);
        assert_eq!(s.sel, SideRow::Agent(0));
        assert_eq!(s.state.agents[0].slug, "designer-2");
        assert_eq!(
            s.home.destination,
            super::super::home::Destination::AllTasks
        );
        assert!(!s.collapsed_agents.contains("designer"));
        assert_eq!(
            s.state.sections[0].agent, None,
            "the section survives unbound"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_settings_shows_history_and_opens_its_task(cx: &mut TestAppContext) {
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
    let history = [Some(true), Some(false), None]
        .into_iter()
        .enumerate()
        .map(|(i, merged)| crate::state::TaskRecord {
            title: format!("Finished {i}"),
            branch: format!("designer/task-{i}"),
            prs: if i == 0 {
                vec![("api".into(), "https://github.com/example/api/pull/1".into())]
            } else {
                vec![]
            },
            merged,
            at: 1_700_000_000_000,
        })
        .collect();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                icon: "folder".into(),
                history,
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
        assert_eq!(
            s.home.sel.as_deref(),
            Some("current"),
            "agent page selects one of its own tasks"
        );
        s.home.page.set_offset(point(px(0.), px(-200.)));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(!w.has_active_dialog(cx), "agent is a page, not a dialog");
        w.click("Instructions & skills", cx);
        let s = shark.read(cx);
        assert!(s.home.focus.is_focused(w));
        assert_eq!(s.home.page.offset(), point(px(0.), px(0.)));
        w.render_frame(cx);
        let skill = crate::state::agent_skills_dir("designer").join("research");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# Research").unwrap();
        w.click("agent-refresh-skills", cx);
        for folder in [".agents/skills", ".claude/skills"] {
            assert_eq!(
                fs::read_link(tasks_dir().join("current").join(folder).join("research")).unwrap(),
                skill
            );
        }
        w.click("History", cx);
        w.render_frame(cx);
        for (i, status) in ["Merged", "Not merged", "PR state unknown"]
            .into_iter()
            .enumerate()
        {
            let row = w.find(("agent-history", i));
            let label = row.label().unwrap();
            assert!(label.contains(status), "{label}");
            assert!(label.contains("2023"), "history includes its date: {label}");
        }
        assert!(w.try_find("agent-history-pr-0-0").is_some());
        w.click("Tasks", cx);
        w.render_frame(cx);
        w.click(("home-ws", 0usize), cx);
        w.render_frame(cx);
        assert!(!w.has_active_dialog(cx), "task selection opens directly");
    })
    .unwrap();
    cx.read(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("current")));
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    press_in(window)(cx, &["i"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.destination, super::super::home::Destination::Home);
        assert_eq!(s.home.prompt.read(cx).value().as_ref(), "@designer ");
        assert!(s.home.prompt.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_agent_doc_refresh_preserves_instruction_snapshots_until_retry(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let agent = crate::state::Agent {
        slug: "designer".into(),
        name: "Designer".into(),
        instructions: "Old instructions".into(),
        ..Default::default()
    };
    let link = AgentLink {
        slug: agent.slug.clone(),
        name: agent.name.clone(),
        instructions: agent.instructions.clone(),
        ..Default::default()
    };
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
        assert_eq!(s.state.tasks[0].context()["agent:designer"], before);
        assert_eq!(
            s.active_chat().unwrap().read(cx).ws.context()["agent:designer"],
            before
        );
        assert_eq!(
            s.state.tasks[1].agent.as_ref().unwrap().instructions,
            "New instructions"
        );
        s.sync_agent("designer", false, cx).unwrap();
        assert_eq!(
            s.state.tasks[0].context()["agent:designer"],
            before,
            "sibling refresh cannot publish instructions the docs failed to deliver"
        );
        assert_eq!(
            s.active_chat().unwrap().read(cx).ws.context()["agent:designer"],
            before
        );
        fs::remove_dir(&blocked).unwrap();
        s.sync_agent("designer", true, cx).unwrap();
        for task in &s.state.tasks {
            assert_eq!(
                task.agent.as_ref().unwrap().instructions,
                "New instructions"
            );
            assert!(
                fs::read_to_string(task.dir().join("CLAUDE.md"))
                    .unwrap()
                    .contains("New instructions")
            );
        }
        assert_ne!(
            s.active_chat().unwrap().read(cx).ws.context()["agent:designer"].fingerprint,
            before.fingerprint
        );
    });
}
