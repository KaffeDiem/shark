use super::super::task_draft::BranchSpec;
use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn add_picker_space_marks_multiple_repositories_and_enter_only_attaches(
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
    on_app(window, shark.clone())(cx, &|s, w, cx| s.enter_home(w, cx));
    press_in(window)(cx, &["cmd-i"]);
    cx.read(|cx| assert!(shark.read(cx).draft_picker.is_some()));
    press_in(window)(cx, &["space", "down", "space", "enter"]);
    until(cx, window, |_, cx| shark.read(cx).draft_picker.is_none()).await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home
                .draft
                .repos
                .iter()
                .map(|r| r.repo.name.as_str())
                .collect::<Vec<_>>(),
            vec!["api", "web"]
        );
        assert!(s.home.draft.repos_edited);
        assert!(
            s.state.tasks.is_empty(),
            "context selection never starts a task"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn anchored_branch_from_accepts_a_local_only_base(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let registered = repos(&home, &["api"]);
    git::git(&registered[0].path, &["branch", "local-base"]).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: registered,
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.prefill_agent_task("designer", w, cx));
    press_in(window)(cx, &["up"]);
    press_in(window)(cx, &["right", "enter"]); // past the agent chip
    press_in(window)(cx, &["down", "enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 1usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("local-base", cx))
        .unwrap();
    press_in(window)(cx, &["alt-enter"]);
    cx.run_until_parked();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(s.draft_picker.is_none(), "local bases do not require a pushed stacking parent");
        assert!(matches!(&s.home.draft.repos[0].branch, BranchSpec::New { base } if base == "local-base"));
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn grouped_branch_picker_enter_checks_out_and_alt_enter_uses_base(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let registered = repos(&home, &["api"]);
    git::git(&registered[0].path, &["branch", "feature"]).unwrap();
    git::git(&registered[0].path, &["branch", "local-base"]).unwrap();
    git::git(
        &registered[0].path,
        &["update-ref", "refs/remotes/origin/feature", "main"],
    )
    .unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: registered,
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.prefill_agent_task("designer", w, cx));
    // The repository is one stop; its first menu action opens the branch picker.
    press_in(window)(cx, &["up"]);
    press_in(window)(cx, &["right", "enter"]); // past the agent chip
    press_in(window)(cx, &["down", "enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 1usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("feature", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    until(cx, window, |_, cx| shark.read(cx).draft_picker.is_none()).await;
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert!(
            matches!(&s.home.draft.repos[0].branch,BranchSpec::Existing(name) if name=="feature")
        );
        assert!(
            s.home.tray_focus.is_focused(w),
            "branch selection restores its opening control"
        );
        assert!(s.state.tasks.is_empty());
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    press_in(window)(cx, &["down", "enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 1usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("feature", cx))
        .unwrap();
    press_in(window)(cx, &["alt-enter"]);
    until(cx, window, |_, cx| shark.read(cx).draft_picker.is_none()).await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(matches!(&s.home.draft.repos[0].branch,BranchSpec::New{base} if base=="feature"));
        assert!(
            s.state.tasks.is_empty(),
            "branch-from changes only draft context"
        );
    });
    press_in(window)(cx, &["enter"]);
    press_in(window)(cx, &["down", "enter"]);
    until(cx, window, |w, cx| {
        w.render_frame(cx);
        w.try_find(("source-branch", 2usize)).is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.click("draft-source-base", cx);
        w.render_frame(cx);
        w.input("local-base", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            matches!(&s.home.draft.repos[0].branch,BranchSpec::New{base} if base=="local-base"),
            "the base submenu accepts a local-only branch without requiring it to be pushed"
        );
        assert!(s.draft_picker.is_none());
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn narrow_draft_keyboard_visits_overflow_and_removes_only_a_chosen_hidden_chip(
    cx: &mut TestAppContext,
) {
    use super::super::draft_view::DraftStop;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let names = [
        "api-component-with-an-extraordinarily-long-package-name",
        "web-component-with-an-extraordinarily-long-package-name",
        "src-component-with-an-extraordinarily-long-package-name",
        "lib-component-with-an-extraordinarily-long-package-name",
    ];
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &names),
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: names.iter().map(|name| name.to_string()).collect(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1100.), px(600.)),
            ))),
            ..Default::default()
        },
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.prefill_agent_task("designer", w, cx));
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    let more = cx.read(|cx| {
        let s = shark.read(cx);
        let visible = s.draft_stops();
        assert!(
            visible
                .iter()
                .filter(|stop| matches!(stop, DraftStop::Repo(_)))
                .count()
                < s.draft_chip_stops().len(),
            "narrow row hides some chips"
        );
        visible
            .iter()
            .position(|stop| *stop == DraftStop::More)
            .expect("hidden chips have an overflow stop")
    });
    press_in(window)(cx, &["up"]);
    for _ in 0..more {
        press_in(window)(cx, &["right"]);
    }
    cx.read(|cx| assert_eq!(shark.read(cx).home.tray_stop, Some(DraftStop::More)));
    press_in(window)(cx, &["backspace"]);
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.draft.repos.len(),
            4,
            "Backspace on overflow removes nothing"
        )
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let focused = s
            .draft_stops()
            .into_iter()
            .find(|stop| matches!(stop, DraftStop::Repo(_)))
            .expect("the narrow fixture keeps one repository visible");
        let DraftStop::Repo(name) = &focused else {
            unreachable!()
        };
        let index = s
            .home
            .draft
            .repos
            .iter()
            .position(|repo| &repo.repo.name == name)
            .unwrap();
        let last = s.home.draft.repos.len() - 1;
        s.home.tray_stop = Some(focused);
        s.home.tray_focus.focus(w, cx);
        s.home.draft.repos.swap(index, last);
        cx.notify();
    });
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.tray_stop,
            Some(DraftStop::More),
            "same-count reorder moves hidden focused identity to overflow"
        )
    });
    press_in(window)(cx, &["backspace"]);
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.draft.repos.len(),
            4,
            "reordering cannot make Backspace delete a different visible repository"
        )
    });
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.within("menu-parent")
                .find(0usize)
                .label()
                .unwrap_or_default()
                .contains("component")
        );
    })
    .unwrap();
    press_in(window)(cx, &["down", "enter"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(
            w.within("menu-parent").find(0usize).label().as_deref(),
            Some("Branch or pull request…")
        );
    })
    .unwrap();
    press_in(window)(cx, &["r", "enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.draft.repos.len(), 3);
        assert!(s.state.tasks.is_empty());
    });
}
