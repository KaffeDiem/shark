use super::*;
use crate::state::Agent;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_task_captures_selected_agent_and_repository_defaults(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let skill = crate::state::agent_skills_dir("designer").join("spacing");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "# Spacing").unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            last_repos: vec!["api".into()],
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                instructions: "Keep spacing consistent".into(),
                repos: vec!["web".into()],
                model: Some("opus".into()),
                effort: Some("low".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("@designer Initial notes", w, cx));
        s.draft_composer_event(&crate::task_composer::Event::Change, w, cx);
    });
    cx.run_until_parked();
    on(cx, &|s, w, cx| {
        s.new_task_default(w, cx);
        // Selecting context must use the text still visible when Start is finally pressed.
        s.home.prompt.update(cx, |p, cx| {
            p.set_value("@designer Fix onboarding spacing", w, cx)
        });
    });
    press_in(window)(cx, &["enter"]);
    cx.run_until_parked();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            s.draft_picker.is_none(),
            "Enter should close branch picker before Start"
        );
        assert!(
            s.state.tasks.is_empty(),
            "picker does not create a task or send hidden notes"
        );
        assert_eq!(
            s.home
                .draft
                .repos
                .iter()
                .map(|r| r.repo.name.as_str())
                .collect::<Vec<_>>(),
            ["web"]
        );
        assert_eq!(
            s.home.prompt.read(cx).value(),
            "@designer Fix onboarding spacing"
        );
    });
    cx.update_window(window, |_, w, cx| w.click("draft-start", cx))
        .unwrap();
    until(cx, window, |_, cx| {
        let s = shark.read(cx);
        s.state.tasks.len() == 1 && !s.home.draft.starting
    })
    .await;
    cx.read(|cx| {
        let s = shark.read(cx);
        let task = &s.state.tasks[0];
        assert_eq!(task.chats[0].agent.as_deref(), Some("designer"));
        assert_eq!(task.agents[0].slug, "designer");
        assert_eq!(task.agents[0].instructions, "Keep spacing consistent");
        assert_eq!(
            task.repos
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            ["web"]
        );
        assert_eq!(task.title, "Fix onboarding spacing");
        assert_eq!(task.chats[0].model.as_deref(), Some("opus"));
        assert_eq!(task.chats[0].effort.as_deref(), Some("low"));
        for folder in [".agents/skills", ".claude/skills"] {
            assert_eq!(
                fs::canonicalize(task.dir().join(folder).join("spacing")).unwrap(),
                fs::canonicalize(&skill).unwrap()
            );
        }
        assert_eq!(
            s.chats[&task.chats[0].id].read(cx).sent_texts(),
            ["Fix onboarding spacing"]
        );
        assert_eq!(s.home.prompt.read(cx).value(), "");
        assert!(s.home_shown() && s.active.is_none());
    });
}
