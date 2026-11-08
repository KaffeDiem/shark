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
        w.click("agent-instructions", cx);
        w.input("Always use Mobbin to ground your research", cx);
        // Defaults › Repos › api makes it a default repository.
        w.click("agent-repos", cx);
    })
    .unwrap();
    press_in(window)(cx, &["a", "enter"]);
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
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-name", cx)
    })
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
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(!w.has_active_dialog(cx), "creation opens no second dialog");
        assert!(
            w.try_find("agent-page").is_some(),
            "settings are edited on the agent's page"
        );
        assert!(w.try_find("agent-skills").is_some());
        assert!(w.try_find("agent-attach").is_none());
        assert_eq!(
            w.find("agent-model").label().as_deref(),
            Some("Model Inherit")
        );
        assert_eq!(
            w.find("agent-effort").label().as_deref(),
            Some("Thinking Inherit")
        );
        assert_eq!(w.find("agent-next").label().as_deref(), Some("Then None"));
        assert!(
            w.try_find("agent-repos").is_some(),
            "the default shows as a chip"
        );
        assert!(w.try_find("agent-empty-tasks").is_some());
        assert!(w.try_find("agent-composer").is_some());
        assert!(
            w.try_find("agent-settings").is_some(),
            "settings sit in the header"
        );
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
        w.click("agent-more", cx);
    })
    .unwrap();
    press_in(window)(cx, &["d", "enter"]);
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
        s.creating.insert("spacing".into(), Some("designer".into()));
        assert!(!s.can_delete_agent("designer", w, cx));
        assert!(s.can_delete_agent("designer-2", w, cx));
        s.creating.insert("designer-standalone".into(), None);
        s.creating.remove("spacing");
        assert!(s.can_delete_agent("designer", w, cx));
        s.creating.clear();
        s.sel = SideRow::Agent(1);
    });
    // The sidebar row's right-click offers the same actions.
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.within("sidebar").right_click(("agent", 0usize), cx);
    })
    .unwrap();
    // The cursor on the other agent follows its identity through the deletion.
    on(cx, &|s, _, _| s.sel = SideRow::Agent(1));
    press_in(window)(cx, &["d", "enter"]);
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents.len(), 1);
        assert_eq!(s.sel, SideRow::Agent(0));
        assert_eq!(s.state.agents[0].slug, "designer-2");
        assert_eq!(s.home.destination, super::super::home::Destination::Home);
        assert!(!s.collapsed_agents.contains("designer"));
        assert_eq!(
            s.state.sections[0].agent, None,
            "the section survives unbound"
        );
    });
    on(cx, &|s, w, cx| {
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
        assert!(s.state.tasks.is_empty());
        assert!(
            s.creating.is_empty(),
            "stale dialogs cannot start a deleted agent's task"
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
        let skill = crate::state::agent_skills_dir("designer").join("research");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "# Research").unwrap();
        w.click("agent-skills", cx);
    })
    .unwrap();
    // The skills chip's menu: Refresh skills links the new skill into the agent's tasks.
    press_in(window)(cx, &["r", "enter"]);
    cx.update_window(window, |_, w, cx| {
        let skill = crate::state::agent_skills_dir("designer").join("research");
        for folder in [".agents/skills", ".claude/skills"] {
            assert_eq!(
                fs::read_link(tasks_dir().join("current").join(folder).join("research")).unwrap(),
                skill
            );
        }
        let header = shark.read(cx).home.agent_header.clone().unwrap();
        assert_eq!(header.read(cx).skills, ["research"]);
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
        assert_eq!(
            s.home.destination,
            super::super::home::Destination::Agent("designer".into()),
            "i types a task without leaving the agent"
        );
        assert!(s.home.agent_prompt.read(cx).focus_handle(cx).is_focused(w));
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

/// Typing in the header's instructions reaches open tasks' CLAUDE.md once typing pauses.
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
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            tasks: vec![task],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.edit_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| w.input("Use tokens", cx))
        .unwrap();
    let synced = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            shark.read(cx).state.tasks[0]
                .agent
                .as_ref()
                .unwrap()
                .instructions
                .clone()
        })
    };
    assert_eq!(synced(cx), "", "no sync while typing");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(synced(cx), "Use tokens");
    let md = cx.read(|cx| shark.read(cx).state.tasks[0].dir().join("CLAUDE.md"));
    assert!(fs::read_to_string(md).unwrap().contains("Use tokens"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn task_composers_keep_shift_enter_as_a_newline(cx: &mut TestAppContext) {
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
            s.home.agent_prompt.read(cx).value(),
            "Agent first\nAgent second"
        );
        assert!(s.state.tasks.is_empty());
        assert!(s.creating.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_drafts_keep_context_and_only_unedited_repos_follow_defaults(
    cx: &mut TestAppContext,
) {
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
        s.create_named_agent("Reviewer", cx).unwrap();
        s.update_agent("designer", |a| a.repos = vec!["api".into()], cx);
        s.prefill_agent_task("designer", w, cx);
        assert_eq!(s.home.agent_draft.repos.len(), 1);
        s.update_agent("designer", |a| a.repos.push("web".into()), cx);
        assert_eq!(
            s.home.agent_draft.repos.len(),
            2,
            "untouched pills follow defaults"
        );
        s.home.agent_draft.repos.clear();
        s.home.agent_draft.repos_edited = true;
        s.home.agent_draft.issues.push(crate::linear::Snapshot {
            identifier: "ENG-412".into(),
            title: "Keep this issue".into(),
            ..Default::default()
        });
        s.home.agent_draft.model = Some("sonnet".into());
        s.home.agent_draft.effort = Some("medium".into());
        s.home.agent_draft.plan = true;
        s.home
            .agent_prompt
            .update(cx, |p, cx| p.set_value("Keep these notes", w, cx));
        s.update_agent("designer", |a| a.repos = vec!["api".into()], cx);
        assert!(
            s.home.agent_draft.repos.is_empty(),
            "edited tray remains independent"
        );
        assert_eq!(s.state.agents[0].repos, ["api"]);
        s.prefill_agent_task("reviewer", w, cx);
        assert!(s.home.agent_draft.issues.is_empty());
        assert!(s.home.agent_draft.model.is_none());
        s.prefill_agent_task("designer", w, cx);
        let draft = &s.home.agent_draft;
        assert_eq!(s.home.agent_prompt.read(cx).value(), "Keep these notes");
        assert!(draft.repos.is_empty());
        assert!(draft.repos_edited);
        assert_eq!(draft.issues[0].identifier, "ENG-412");
        assert_eq!(draft.model.as_deref(), Some("sonnet"));
        assert_eq!(draft.effort.as_deref(), Some("medium"));
        assert!(draft.plan);
    });
}
