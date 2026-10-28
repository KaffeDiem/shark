//! Review probes for quick-open lifecycle and scaling with a large workspace catalog.
use super::super::workspace_picker::WorkspacePicker;
use super::*;
use gpui_kit::test::TestQueryExt;
use std::cell::Cell;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_review_cache_tracks_status_and_repository_changes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            workspaces: vec![Workspace {
                name: "eng-130-rate-limits".into(),
                repos: vec![crate::state::WsRepo {
                    name: "api".into(),
                    src: PathBuf::new(),
                    base: "main".into(),
                    branch: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let picker = cx
        .update_window(window, |_, w, cx| {
            let picker = cx.new(|cx| {
                WorkspacePicker::new(
                    WorkspacePicker::snapshot(shark.read(cx), cx),
                    None,
                    shark.downgrade(),
                    w,
                    cx,
                )
            });
            let child = picker.clone();
            w.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(child.clone()));
            picker
                .read(cx)
                .state
                .clone()
                .update(cx, |s, cx| s.focus(w, cx));
            w.render_frame(cx);
            picker
        })
        .unwrap();
    cx.run_until_parked();
    let notifications = Rc::new(Cell::new(0));
    let observed = notifications.clone();
    let minute = super::super::interaction::now_ms() / 60_000;
    let _subscription =
        cx.update(|cx| cx.observe(&picker, move |_, _| observed.set(observed.get() + 1)));
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.workspaces[0].viewed.insert("api/file.rs".into(), 1);
        cx.notify();
    });
    if minute == super::super::interaction::now_ms() / 60_000 {
        assert_eq!(
            notifications.get(),
            0,
            "unrelated workspace metadata does not rebuild the picker"
        );
    }
    notifications.set(0);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let ws = &s.state.workspaces[0];
        let mut repo = crate::sync::Repo::fresh(&ws.repos[0], ws);
        repo.pr = Some(Err("offline".into()));
        s.attn.set_checked(&ws.name, vec![repo]);
        cx.notify();
    });
    assert!(
        notifications.get() > 0,
        "background PR status changes update the cached rows"
    );
    let before = cx
        .update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(
                w.elements().iter().any(|element| element
                    .label()
                    .is_some_and(|label| label.contains("api: Status unavailable"))),
                "the visible accessible label refreshes with PR status"
            );
            w.find("workspace-result-eng-130-rate-limits")
                .bounds()
                .size
                .height
        })
        .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        let mut repo = s.state.workspaces[0].repos[0].clone();
        repo.name = "web".into();
        s.state.workspaces[0].repos.push(repo);
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("workspace-result-eng-130-rate-limits")
                .bounds()
                .size
                .height
                > before,
            "repository additions remeasure variable-height rows"
        );
        w.input("rate limits", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(picker.read(cx).hits, ["eng-130-rate-limits"]));
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).active.as_deref(),
            Some("eng-130-rate-limits")
        )
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_review_large_catalog_and_release(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    for count in [100, 500, 2000] {
        on_app(window, shark.clone())(cx, &|s, _, _cx| {
            s.state.workspaces = (0..count)
                .map(|i| Workspace {
                    name: format!("workspace-{i:04}"),
                    branch: format!("workspace-{i:04}"),
                    last_interacted_at: Some(i as u64),
                    repos: vec![crate::state::WsRepo {
                        name: "api".into(),
                        src: PathBuf::new(),
                        base: "main".into(),
                        branch: None,
                    }],
                    ..Default::default()
                })
                .collect();
        });
        let started = Instant::now();
        for _ in 0..5 {
            on_app(window, shark.clone())(cx, &|_, _, cx| cx.notify());
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
        }
        let baseline = started.elapsed();
        let started = Instant::now();
        let picker = cx
            .update_window(window, |_, w, cx| {
                let picker = cx.new(|cx| {
                    WorkspacePicker::new(
                        WorkspacePicker::snapshot(shark.read(cx), cx),
                        None,
                        shark.downgrade(),
                        w,
                        cx,
                    )
                });
                let child = picker.clone();
                w.open_dialog(cx, move |d, _, cx| menu_dialog(d, cx).child(child.clone()));
                picker
                    .read(cx)
                    .state
                    .clone()
                    .update(cx, |s, cx| s.focus(w, cx));
                w.render_frame(cx);
                picker
            })
            .unwrap();
        let opened = started.elapsed();
        cx.update(|cx| {
            assert_eq!(picker.read(cx).hits.len(), count);
            assert_eq!(
                picker.read(cx).hits[0],
                format!("workspace-{:04}", count - 1)
            );
        });
        let started = Instant::now();
        for _ in 0..5 {
            on_app(window, shark.clone())(cx, &|_, _, cx| cx.notify());
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
        }
        println!(
            "workspace review: {count} workspaces; open={opened:?}; five closed refreshes={baseline:?}; five open refreshes={:?}",
            started.elapsed()
        );
        cx.update_window(window, |_, w, cx| w.input("workspace-0000", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(picker.read(cx).hits[0], "workspace-0000"));
        let weak = picker.downgrade();
        press_in(window)(cx, &["escape"]);
        drop(picker);
        cx.run_until_parked();
        cx.update(|_| {
            assert!(
                weak.upgrade().is_none(),
                "closing releases picker and subscriptions"
            )
        });
    }
}
