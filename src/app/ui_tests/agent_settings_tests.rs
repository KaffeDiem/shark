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
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        crate::settings::open_agents(&s.state, w, cx)
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("new-agent", cx);
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
        assert_eq!(s.state.sections.len(), 1, "reuse the unbound section");
        assert_eq!(s.state.sections[0].agent.as_deref(), Some("designer"));
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
        assert_eq!(s.state.sections[0].name, "Design team");
    });
    let saved = State::load();
    assert_eq!(
        saved.agents[0].instructions,
        "Always use Mobbin to ground your research"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_sections_are_unique_and_deletion_rechecks_open_tasks(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer");
        assert_eq!(s.create_named_agent("Designer", cx).unwrap(), "designer-2");
        assert_eq!(s.state.sections[0].name, "Designer");
        assert_eq!(s.state.sections[1].name, "Designer 2");
        s.open_agent("designer", w, cx);
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
        assert_eq!(
            s.state.sections[0].agent, None,
            "the section survives unbound"
        );
    });
}
