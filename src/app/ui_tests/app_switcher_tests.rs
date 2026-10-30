use super::*;
use crate::apps::OPENED;
use gpui_kit::test::TestEventExt;

fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.state.workspaces = ["remaining", "target"]
            .map(|name| Workspace {
                name: name.into(),
                branch: name.into(),
                ..Default::default()
            })
            .to_vec();
        s.active = Some("remaining".into());
        s.sel = SideRow::Ws(1);
        s.sidebar_focus.focus(w, cx);
    });
    (window, shark)
}

fn modifiers(cx: &mut TestAppContext, window: AnyWindowHandle, modifiers: Modifiers) {
    cx.update_window(window, |_, w, cx| w.change_modifiers(modifiers, cx))
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn app_switcher_recents(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let apps = crate::apps::installed();
    let press = press_in(window);
    let history =
        |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).state.recent_apps.clone());

    // Keyboard confirmation records once; a cancelled selection records nothing.
    press(cx, &["cmd-o", "right", "enter"]);
    assert_eq!(history(cx), [apps[1].path.clone()]);
    press(cx, &["cmd-o", "right", "escape"]);
    assert_eq!(history(cx), [apps[1].path.clone()]);

    // Both direct repo entries and workspace submenus use the same launch path.
    for folder in [false, true] {
        let action = shark.update(cx, |_, cx| {
            let dir = ("Workspace folder".into(), PathBuf::from("/workspace"));
            let entries = crate::app::menus::open_in_entries(
                folder.then_some(&dir),
                &[("repo".into(), PathBuf::from("/repo"))],
                cx,
            );
            let entry = entries
                .into_iter()
                .find(|entry| match entry {
                    Entry::Item { label, .. } | Entry::Sub { label, .. } => {
                        label == "Open in Terminal"
                    }
                    _ => false,
                })
                .unwrap();
            let entry = match entry {
                Entry::Sub { mut entries, .. } => entries.remove(0),
                entry => entry,
            };
            let Entry::Item { on_click, .. } = entry else {
                panic!("expected launch action")
            };
            on_click
        });
        cx.update_window(window, |_, w, cx| action(w, cx)).unwrap();
    }
    assert_eq!(history(cx), [apps[0].path.clone(), apps[1].path.clone()]);

    // Another workspace sees the same order; moving an app to the front deduplicates it.
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.sel = SideRow::Ws(0);
        s.sidebar_focus.focus(w, cx);
    });
    press(cx, &["cmd-o", "right", "enter"]);
    let expected = vec![apps[1].path.clone(), apps[0].path.clone()];
    assert_eq!(history(cx), expected);
    assert_eq!(State::load().recent_apps, expected);

    // Restart with unavailable history entries: filter first, keeping unused apps in order.
    let mut saved = State::load();
    saved
        .recent_apps
        .insert(0, "/Applications/Removed.app".into());
    saved.recent_apps.insert(0, apps[3].path.clone());
    let (reopened, restored) = boot(cx, saved, WindowOptions::default());
    on_app(reopened, restored.clone())(cx, &|s, w, cx| {
        s.sel = SideRow::Ws(0);
        s.sidebar_focus.focus(w, cx);
    });
    press_in(reopened)(cx, &["cmd-o"]);
    cx.update(|cx| {
        let switcher = restored.read(cx).app_switcher.as_ref().unwrap();
        assert_eq!(switcher.selected, 0);
        assert_eq!(
            switcher
                .apps
                .iter()
                .map(|app| app.name.as_str())
                .collect::<Vec<_>>(),
            ["Code", "Terminal", "Fork"]
        );
    });
    press_in(reopened)(cx, &["escape"]);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn app_switcher_menu_modifiers(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let before = OPENED.lock().unwrap().len();
    // The File menu dispatches the action without holding Command.
    cx.update_window(window, |_, w, cx| {
        w.dispatch_action(Box::new(CycleOpenApp), cx);
        w.render_frame(cx);
    })
    .unwrap();
    modifiers(
        cx,
        window,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(
        OPENED.lock().unwrap().len(),
        before,
        "Shift alone must not launch"
    );
    modifiers(cx, window, Modifiers::default());
    cx.update(|cx| assert!(shark.read(cx).app_switcher.is_some()));
    modifiers(
        cx,
        window,
        Modifiers {
            alt: true,
            ..Default::default()
        },
    );
    modifiers(cx, window, Modifiers::default());
    assert_eq!(
        OPENED.lock().unwrap().len(),
        before,
        "Option alone must not launch"
    );
    press_in(window)(cx, &["enter"]);
    assert_eq!(OPENED.lock().unwrap().last().unwrap().0, "Terminal");
    assert_eq!(OPENED.lock().unwrap().len(), before + 1);
    // With no intervening character, GPUI reports a bare Command tap as a keystroke too.
    cx.update_window(window, |_, w, cx| {
        w.dispatch_action(Box::new(CycleOpenApp), cx);
        w.render_frame(cx);
    })
    .unwrap();
    modifiers(
        cx,
        window,
        Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    modifiers(cx, window, Modifiers::default());
    assert_eq!(
        OPENED.lock().unwrap().len(),
        before + 2,
        "bare Command release confirms the menu-opened switcher"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn app_switcher_active_window_cancels(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, _| assert!(w.is_window_active()))
        .unwrap();
    let before = OPENED.lock().unwrap().len();
    modifiers(
        cx,
        window,
        Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    press_in(window)(cx, &["cmd-o", "cmd-o"]);
    cx.update(|cx| assert_eq!(shark.read(cx).app_switcher.as_ref().unwrap().selected, 1));
    modifiers(
        cx,
        window,
        Modifiers {
            platform: true,
            shift: true,
            ..Default::default()
        },
    );
    modifiers(
        cx,
        window,
        Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    assert_eq!(
        OPENED.lock().unwrap().len(),
        before,
        "releasing Shift while holding Command does not launch"
    );
    modifiers(cx, window, Modifiers::default());
    assert_eq!(OPENED.lock().unwrap().len(), before + 1);
    assert_eq!(OPENED.lock().unwrap().last().unwrap().0, "Code");
    modifiers(
        cx,
        window,
        Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    press_in(window)(cx, &["cmd-o"]);
    let (other, _) = open(cx, WindowOptions::default());
    cx.update_window(other, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, _| assert!(!w.is_window_active()))
        .unwrap();
    cx.update(|cx| {
        assert!(
            shark.read(cx).app_switcher.is_none(),
            "deactivation cancels"
        )
    });
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    modifiers(cx, window, Modifiers::default());
    assert_eq!(OPENED.lock().unwrap().len(), before + 1);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn app_switcher_archived_target_does_not_retarget(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let before = OPENED.lock().unwrap().len();
    modifiers(
        cx,
        window,
        Modifiers {
            platform: true,
            ..Default::default()
        },
    );
    // Archiving finishes asynchronously, after the switcher has captured the selected row.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.remove_workspace("target", true, w, cx);
        });
        w.press("cmd-o", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.wait_for(window, Duration::from_secs(5), |_, cx| {
        shark.read(cx).ws("target").is_none()
    })
    .await;
    cx.update(|cx| {
        assert!(
            shark.read(cx).app_switcher.is_some(),
            "background archive keeps overlay focus"
        )
    });
    modifiers(cx, window, Modifiers::default());
    cx.update(|cx| {
        assert!(
            shark.read(cx).app_switcher.is_none(),
            "release confirms or dismisses the switcher"
        )
    });
    assert_eq!(
        OPENED.lock().unwrap().len(),
        before,
        "a vanished target must never become another workspace"
    );
}

/// Esc cancels whether or not Command is still held; releasing it afterwards opens nothing.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn app_switcher_escape_cancels(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let before = OPENED.lock().unwrap().len();
    let held = Modifiers {
        platform: true,
        ..Default::default()
    };
    for esc in ["cmd-escape", "escape"] {
        modifiers(cx, window, held);
        press_in(window)(cx, &["cmd-o", "cmd-o", esc]);
        cx.update(|cx| assert!(shark.read(cx).app_switcher.is_none(), "{esc} cancels"));
        modifiers(cx, window, Modifiers::default());
        assert_eq!(
            OPENED.lock().unwrap().len(),
            before,
            "{esc} then release opens nothing"
        );
    }
}
