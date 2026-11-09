use super::super::task_draft::BranchSpec;
use super::*;
use crate::state::Agent;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn draft_defaults_save_reset_and_inherit_keep_task_context(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            last_model: Some("sonnet".into()),
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                model: Some("opus".into()),
                effort: Some("high".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.prefill_agent_task("designer", w, cx);
        s.home
            .agent_prompt
            .update(cx, |p, cx| p.set_value("Keep this prompt", w, cx));
        s.home.agent_draft.repos[0].branch = BranchSpec::Existing("feature".into());
        s.home.agent_draft.issues.push(Snapshot {
            id: "issue".into(),
            identifier: "SHA-42".into(),
            title: "Polish".into(),
            ..Default::default()
        });
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("draft-save-default").is_none(),
            "branches and issues are task-only"
        );
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.home.agent_draft.model = Some("sonnet".into());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-save-default", cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents[0].model.as_deref(), Some("sonnet"));
        assert_eq!(s.state.agents[0].repos, ["api"]);
        assert!(s.state.tasks.is_empty());
        assert_eq!(
            s.home.agent_prompt.read(cx).value().as_str(),
            "Keep this prompt"
        );
        assert_eq!(s.home.agent_draft.issues.len(), 1);
        assert!(
            matches!(&s.home.agent_draft.repos[0].branch, BranchSpec::Existing(b) if b=="feature")
        );
        assert!(
            !s.home
                .agent_draft
                .settings_edited(&s.state, Some("designer"))
        );
    });
    on(cx, &|s, _, cx| {
        s.home.agent_draft.model = Some("opus".into());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-reset", cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home.agent_draft.defaults(&s.state, Some("designer")).0,
            "sonnet"
        );
        assert_eq!(s.home.agent_draft.issues.len(), 1);
        assert!(
            matches!(&s.home.agent_draft.repos[0].branch, BranchSpec::Existing(b) if b=="feature")
        );
    });
    on(cx, &|s, w, cx| s.draft_model_menu(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.within("menu-parent").click(0usize, cx);
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-save-default", cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents[0].model, None);
        assert_eq!(s.state.agents[0].effort, None);
        assert_eq!(State::load().agents[0].model, None);
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn draft_context_is_visible_when_empty_and_hidden_in_ask(cx: &mut TestAppContext) {
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
    on_app(window, shark.clone())(cx, &|s, w, cx| s.enter_home(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("draft-empty-repo").is_some());
        assert!(w.try_find("draft-add").is_some());
        assert!(w.try_find("draft-save-default").is_none());
    })
    .unwrap();
    press_in(window)(cx, &["cmd-shift-b"]);
    cx.read(|cx| assert!(shark.read(cx).draft_picker.is_some()));
    press_in(window)(cx, &["escape"]);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.toggle_ask_mode(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("draft-empty-repo").is_none());
        assert!(w.try_find("draft-add").is_none());
    })
    .unwrap();
    press_in(window)(cx, &["cmd-i"]);
    cx.read(|cx| assert!(shark.read(cx).draft_picker.is_none()));
}
