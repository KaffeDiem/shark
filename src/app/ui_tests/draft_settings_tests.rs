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
            .prompt
            .update(cx, |p, cx| p.set_value("Keep this prompt", w, cx));
        s.home.draft.repos[0].branch = BranchSpec::Existing("feature".into());
        s.home.draft.issues.push(Snapshot {
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
            w.try_find("draft-edited").is_none(),
            "branches and issues are task-only"
        );
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.home.draft.model = Some("sonnet".into());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-edited", cx);
        w.render_frame(cx);
        w.within("menu-parent").click(2usize, cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.agents[0].model.as_deref(), Some("sonnet"));
        assert_eq!(s.state.agents[0].repos, ["api"]);
        assert!(s.state.tasks.is_empty());
        assert_eq!(s.home.prompt.read(cx).value().as_str(), "Keep this prompt");
        assert_eq!(s.home.draft.issues.len(), 1);
        assert!(matches!(&s.home.draft.repos[0].branch, BranchSpec::Existing(b) if b=="feature"));
        assert!(!s.home.draft.settings_edited(&s.state, Some("designer")));
    });
    on(cx, &|s, _, cx| {
        s.home.draft.model = Some("opus".into());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-edited", cx);
        w.render_frame(cx);
        w.within("menu-parent").click(1usize, cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home.draft.defaults(&s.state, Some("designer")).0,
            "sonnet"
        );
        assert_eq!(s.home.draft.issues.len(), 1);
        assert!(matches!(&s.home.draft.repos[0].branch, BranchSpec::Existing(b) if b=="feature"));
    });
    on(cx, &|s, w, cx| s.draft_model_menu(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.within("menu-parent").click(0usize, cx);
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("draft-edited", cx);
        w.render_frame(cx);
        w.within("menu-parent").click(2usize, cx);
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
        assert!(w.try_find("draft-empty-repo").is_none());
        assert_eq!(w.find("draft-plan").label().as_deref(), Some("Plan only"));
        assert!(w.try_find("draft-add").is_some());
        assert!(w.try_find("draft-save-default").is_none());
    })
    .unwrap();
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).draft_chip_stops().first(),
            Some(&super::super::draft_view::DraftStop::Agent),
            "Home's agent is the leading chip"
        )
    });
    press_in(window)(cx, &["cmd-shift-b"]);
    cx.read(|cx| assert!(shark.read(cx).draft_picker.is_some()));
    press_in(window)(cx, &["escape"]);
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.home
            .draft
            .repos
            .push(super::super::task_draft::DraftRepo {
                repo: s.state.repos[0].clone(),
                branch: BranchSpec::New {
                    base: String::new(),
                },
                pr_label: None,
            });
        s.toggle_ask_mode(w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("draft-empty-repo").is_none());
        assert!(w.try_find("draft-add").is_none());
        assert!(w.try_find("draft-model").is_none());
        assert!(w.try_find("draft-plan").is_none());
        assert!(w.try_find("draft-start").is_some());
    })
    .unwrap();
    press_in(window)(cx, &["cmd-i"]);
    cx.read(|cx| assert!(shark.read(cx).draft_picker.is_none()));
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.home.tray_shown = usize::MAX;
        s.home.tray_stop = Some(super::super::draft_view::DraftStop::Repo("api".into()));
        s.home.tray_focus.focus(w, cx);
    });
    press_in(window)(cx, &["backspace"]);
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.draft.repos.len(),
            1,
            "Ask row cannot remove hidden draft context"
        )
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn removing_default_repository_explains_edited_settings_in_one_row(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into(), "web".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.prefill_agent_task("designer", w, cx));
    press_in(window)(cx, &["up"]);
    // The tray starts on the agent chip; web is two stops on.
    press_in(window)(cx, &["right", "right", "backspace"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("draft-edited").is_some(),
            "removing a default leaves an Edited control"
        );
        assert!(w.try_find("draft-reset").is_none());
        assert!(w.try_find("draft-save-default").is_none());
        let row_y = w.find("draft-add").bounds().center().y;
        for bounds in [
            w.find(("draft-repo", 0usize)).bounds(),
            w.find("draft-model").bounds(),
            w.find("draft-start").bounds(),
        ] {
            assert!(
                (bounds.center().y - row_y).abs() < px(1.),
                "context and actions share one row"
            );
        }
        w.click("draft-edited", cx);
        w.render_frame(cx);
        let summary = w
            .within("menu-parent")
            .find(0usize)
            .label()
            .unwrap_or_default()
            .to_string();
        assert!(summary.contains("Removed web"), "{summary}");
        w.within("menu-parent").click(1usize, cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home
                .draft
                .repos
                .iter()
                .map(|repo| repo.repo.name.as_str())
                .collect::<Vec<_>>(),
            ["api", "web"]
        );
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn draft_hints_only_show_for_a_focused_empty_prompt(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| s.focus_draft(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("draft-hints").is_some());
        w.input("Some work", cx);
        w.render_frame(cx);
        assert!(w.try_find("draft-hints").is_none());
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.home
            .prompt
            .update(cx, |prompt, cx| prompt.set_value("", w, cx));
        s.home.focus.focus(w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("draft-hints").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn narrow_draft_keeps_start_inside_with_edited_model_and_disconnected_issue(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(800.), px(600.)),
            ))),
            ..Default::default()
        },
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.prefill_agent_task("designer", w, cx);
        s.home.draft.model = Some("long-custom-model-name".into());
        s.home.draft.issues.push(Snapshot {
            org: "disconnected".into(),
            id: "old-issue".into(),
            identifier: "OLD-42".into(),
            ..Default::default()
        });
        cx.notify();
    });
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let card = w.find("home-composer").bounds();
        let start = w.find("draft-start").bounds();
        assert!(
            start.right() <= card.right() - px(12.),
            "Start remains inside the card: {start:?}, card {card:?}"
        );
        for id in [
            "draft-model",
            "draft-edited",
            "draft-add",
            "draft-disconnected",
        ] {
            let bounds = w.find(id).bounds();
            assert!(
                (bounds.center().y - start.center().y).abs() < px(1.),
                "{id} stays in the same compact row"
            );
        }
    })
    .unwrap();
    for starting in [false, true] {
        on_app(window, shark.clone())(cx, &|s, _, cx| {
            s.home.draft.repos.clear();
            s.home.draft.issues.clear();
            s.home.draft.files.clear();
            s.home.draft.starting = starting;
            cx.notify();
        });
        for _ in 0..4 {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let attach = w.find("draft-add").bounds();
            let edited = w.find("draft-edited").bounds();
            let start = w.find("draft-start").bounds();
            let card = w.find("home-composer").bounds();
            assert!(
                attach.right() <= edited.left(),
                "empty Attach must not overlap Edited, starting={starting}: {attach:?}, {edited:?}"
            );
            assert!(
                start.right() <= card.right() - px(12.),
                "empty row keeps Start inside the card, starting={starting}: {start:?}, {card:?}"
            );
        })
        .unwrap();
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn narrow_draft_keeps_the_model_label_whole_while_plan_is_visible(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["web".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(800.), px(600.)),
            ))),
            ..Default::default()
        },
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.prefill_agent_task("designer", w, cx);
        s.home.draft.issues.push(Snapshot {
            org: "acme".into(),
            id: "issue-412".into(),
            identifier: "ENG-412".into(),
            title: "Skills can't be added".into(),
            ..Default::default()
        });
        s.home.draft.files = vec![
            PathBuf::from("/tmp/design-review-notes.md"),
            PathBuf::from("/tmp/long-composer-layout-reference.png"),
        ];
        s.home.prompt.update(cx, |prompt, cx| {
            prompt.set_value("Keep every attachment reachable in a narrow window", w, cx)
        });
        cx.notify();
    });
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(window,|_,w,cx| {
        w.render_frame(cx);
        let density=shark.read(cx).home.footer_density;
        assert!(density<2,"fixture retains Plan before compacting the model further");
        assert!(w.try_find("draft-plan").is_some());
        let measured=w.find(if density==0 {"draft-probe-full"}else{"draft-probe-short"}).bounds().size.width;
        let actual=w.find("draft-model").bounds().size.width;
        assert!(actual+px(1.)>=measured,"model must keep its measured label while Plan remains visible: actual {actual:?}, measured {measured:?}, density {density}");
    }).unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn draft_model_button_backspace_cannot_delete_a_stale_repository_stop(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.prefill_agent_task("designer", w, cx);
        s.home.tray_stop = Some(super::super::draft_view::DraftStop::Repo("api".into()));
    });
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    cx.update_window(window, |_, w, cx| {
        for _ in 0..128 {
            w.render_frame(cx);
            if w.find("draft-model").focused() == Some(true) {
                break;
            }
            w.focus_next(cx);
        }
        w.render_frame(cx);
        assert_eq!(
            w.find("draft-model").focused(),
            Some(true),
            "native keyboard focus reaches the model button"
        );
        assert!(!shark.read(cx).home.tray_focus.is_focused(w));
        w.click("draft-model", cx);
        w.render_frame(cx);
    })
    .unwrap();
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(
            w.find("draft-model").focused(),
            Some(true),
            "closing model menu returns native focus to its button"
        );
        assert!(!shark.read(cx).home.tray_focus.is_focused(w));
    })
    .unwrap();
    press_in(window)(cx, &["backspace"]);
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.draft.repos.len(),
            1,
            "model-button keys cannot act on stale context-row selection"
        )
    });
}
