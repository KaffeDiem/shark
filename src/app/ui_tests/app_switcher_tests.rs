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
