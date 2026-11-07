use super::*;
use crate::state::{Agent, AgentLink, RepoSettings, TaskRepo};

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_pages_do_not_mutate_hidden_chats_or_run_hidden_tasks(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut task = Task {
        name: "work".into(),
        branch: "main".into(),
        agent: Some(AgentLink {
            slug: "designer".into(),
            name: "Designer".into(),
            instructions: String::new(),
            siblings: vec![],
        }),
        repos: vec![TaskRepo {
            name: "api".into(),
            src: home.join("api"),
            base: "main".into(),
            branch: None,
        }],
        chats: vec![ChatMeta {
            id: "work-chat".into(),
            model: Some(MODELS[0].into()),
            ..Default::default()
        }],
        closed: vec![ChatMeta {
            id: "closed-chat".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    task.normalize_tabs();
    init_repo(&task.dir().join("api"));
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![task],
            agents: vec![
                Agent {
                    slug: "designer".into(),
                    name: "Designer".into(),
                    ..Default::default()
                },
                Agent {
                    slug: "empty".into(),
                    name: "Empty".into(),
                    ..Default::default()
                },
            ],
            repo_settings: [(
                "api".into(),
                RepoSettings {
                    run: vec!["sleep 30".into()],
                    ..Default::default()
                },
            )]
            .into(),
            last_open: Some(("work".into(), "chat".into())),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let chat = s.active_chat().unwrap().clone();
        chat.update(cx, |c, _| c.meta.session_id = Some("keep-session".into()));
        let before = chat.read(cx).meta.clone();
        let reset = palette::COMMANDS
            .iter()
            .find(|row| row.0 == "Start a new conversation")
            .unwrap();
        let blocked_chat_commands = |s: &mut Shark, w: &mut Window, cx: &mut Context<Shark>| {
            s.with_sync(w, cx, |_, _, _| {
                panic!("overview must not run hidden Git actions")
            });
            reset.run(s, w, cx);
            s.set_model(1, w, cx);
            s.set_effort("high", w, cx);
            s.new_chat(w, cx);
            s.reopen_chat(None, w, cx);
            s.with_plan(w, cx, |c, _, _| c.meta.plan = !c.meta.plan);
            assert_eq!(chat.read(cx).meta.session_id, before.session_id);
            assert_eq!(chat.read(cx).meta.model, before.model);
            assert_eq!(chat.read(cx).meta.effort, before.effort);
            assert_eq!(chat.read(cx).meta.plan, before.plan);
            assert_eq!(s.task("work").unwrap().chats.len(), 1);
            assert_eq!(s.task("work").unwrap().closed.len(), 1);
        };
        for tab in [
            agent_settings::AgentTab::Instructions,
            agent_settings::AgentTab::History,
        ] {
            s.open_agent("designer", w, cx);
            s.home.agent_tab = tab;
            blocked_chat_commands(s, w, cx);
            s.toggle_run(w, cx);
            assert!(s.runs.is_empty());
        }
        s.open_agent("empty", w, cx);
        // Even stale selection cannot make an empty agent target another agent's task.
        s.home.sel = Some("work".into());
        blocked_chat_commands(s, w, cx);
        s.toggle_run(w, cx);
        assert!(s.runs.is_empty());
        s.enter_all_tasks(w, cx);
        blocked_chat_commands(s, w, cx);
        // A task selected in All tasks can run scripts explicitly.
        s.home.sel = Some("work".into());
        s.toggle_run(w, cx);
        assert!(s.runs.contains_key("work"));
        s.toggle_run(w, cx);
        assert!(s.runs.is_empty());
        s.enter_home(w, cx);
        let mut synced = false;
        s.with_sync(w, cx, |_, _, _| synced = true);
        assert!(synced, "fixture has a live GitSync to protect");
        s.set_model(1, w, cx);
        assert_eq!(
            chat.read(cx).meta.model.as_deref(),
            Some(MODELS[1]),
            "legacy Home keeps its chat shortcuts"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_tasks_do_not_inherit_an_invisible_home_draft(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    for (destination, branch) in [
        (home::Destination::Agent("designer".into()), "from-agent"),
        (home::Destination::AllTasks, "from-all-tasks"),
    ] {
        on(cx, &|s, w, cx| {
            s.enter_home(w, cx);
            s.home
                .prompt
                .update(cx, |p, cx| p.set_value("Keep this draft", w, cx));
            match &destination {
                home::Destination::Agent(slug) => s.open_agent(slug, w, cx),
                _ => s.enter_all_tasks(w, cx),
            }
            s.new_task_default(w, cx);
        });
        cx.update_window(window, |_, w, cx| w.input(branch, cx))
            .unwrap();
        press_in(window)(cx, &["enter"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(branch)
        })
        .await;
        cx.run_until_parked();
        cx.read(|cx| {
            let s = shark.read(cx);
            assert_eq!(s.home.prompt.read(cx).value().as_ref(), "Keep this draft");
            assert!(s.active_chat().unwrap().read(cx).sent_texts().is_empty());
            assert!(s.task(branch).unwrap().agent.is_none());
        });
    }
}
