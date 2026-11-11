use super::*;
use crate::state::{Agent, AgentLink};

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
        assert!(w.try_find("agent-repos").is_none());
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        let a = &s.state.agents[0];
        assert_eq!(a.slug, "designer");
        assert!(a.repos.is_empty());
        assert_eq!(a.instructions, "Always use Mobbin to ground your research");
        assert_eq!(
            s.state.sections.len(),
            1,
            "agent creation leaves task groups alone"
        );
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
        assert_eq!(
            w.find("agent-page-icon").bounds().size,
            size(px(40.), px(40.))
        );
        assert!(w.try_find("agent-attach").is_none());
        assert!(w.try_find("agent-model").is_none());
        assert!(w.try_find("agent-effort").is_none());
        assert!(w.try_find("agent-defaults-label").is_none());
        assert_eq!(w.find("agent-skills").label().as_deref(), Some("0 skills"));
        assert_eq!(
            w.find("agent-next").label().as_deref(),
            Some("Hands off to nobody")
        );
        assert!(
            w.try_find("agent-settings").is_some(),
            "settings sit in the header"
        );
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let labels = s
            .agent_identity_menu("designer", cx)
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Item { label, .. } => Some(label.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "Rename",
                "Change icon…",
                "Paste Image as Icon",
                "Edit instructions",
                "Delete agent…"
            ]
        );
    });
    let saved = State::load();
    assert_eq!(
        saved.agents[0].instructions,
        "Always use Mobbin to ground your research"
    );
}

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
        w.click("agent-more", cx);
    })
    .unwrap();
    press_in(window)(cx, &["d", "enter"]);
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents.len(), 1);
        assert_eq!(s.state.agents[0].slug, "designer-2");
        assert_eq!(s.home.destination, super::super::home::Destination::Home);
        let task = &s.state.tasks[0];
        assert!(task.agents.is_empty(), "its snapshot goes with it");
        assert_eq!(task.chats[0].agent.as_deref(), Some("designer"));
    });
    on(cx, &|s, w, cx| s.leave_home(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let agent = w.find("chat-agent");
        assert_eq!(
            agent.label().as_deref(),
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

/// An agent's page is its settings: no composer, task list or history below the header.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_page_shows_only_its_settings(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let link = AgentLink {
        slug: "designer".into(),
        name: "Designer".into(),
        ..Default::default()
    };
    let task = git::create_empty_task(&tasks_dir().join("one"), "one", "One", Some(link)).unwrap();
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
    on_app(window, shark)(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("agent-settings").is_some());
        for id in [
            "home-composer",
            "agent-composer",
            "home-list",
            "agent-history",
            "agent-empty-tasks",
        ] {
            assert!(w.try_find(id).is_none(), "{id} on the agent page");
        }
        assert!(w.try_find(("home-ws", 0usize)).is_none());
    })
    .unwrap();
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
            assert!(
                !tasks_dir()
                    .join("other")
                    .join(folder)
                    .join("research")
                    .exists()
            );
        }
        let header = shark.read(cx).home.agent_header.clone().unwrap();
        assert_eq!(header.read(cx).skills, ["research"]);
        w.render_frame(cx);
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

/// Typing in the header's instructions reaches the chats of open tasks once typing pauses.
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
    on_app(window, shark.clone())(cx, &|s, w, cx| s.edit_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| w.input("Use tokens", cx))
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
