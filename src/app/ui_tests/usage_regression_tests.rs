//! Footer, panel, and settings regressions for usage: quota readout, panel open and close, focus.
use super::*;
use crate::usage::Allowance;
use gpui_kit::test::TestEventExt as _;

fn weekly(remaining: f64) -> Vec<Allowance> {
    vec![Allowance {
        label: "5 hours".into(),
        remaining: Some(remaining),
        resets_at: Some(crate::usage::now() + 9000.),
        window: Some(18000.),
    }]
}

/// Quotas of 42% and 68% left.
fn fill_usage(s: &mut Shark) {
    for (index, snapshot) in s.usage.iter_mut().enumerate() {
        snapshot.finish(Ok(weekly(if index == 0 { 42. } else { 68. })));
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn footer_opens_usage_in_both_layouts(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            fill_usage(s);
            cx.notify();
        });
        for rail in [false, true] {
            shark.update(cx, |s, cx| {
                s.rail = rail;
                cx.notify();
            });
            w.render_frame(cx);
            let footer = w.find("usage-footer").bounds();
            let button = w.find("show-usage").bounds();
            assert!(button.left() >= footer.left() && button.right() <= footer.right());
            assert!(w.try_find("usage-trigger-claude").is_none());
            assert!(w.try_find("usage-trigger-codex").is_none());
            assert!(w.try_find("usage-panel").is_none());
            w.click("show-usage", cx);
            w.render_frame(cx);
            assert!(w.find("usage-claude-0").visible());
            assert!(w.find("usage-codex-0").visible());
            w.press("escape", cx);
            assert!(w.try_find("usage-panel").is_none());
        }
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn footer_quota_readout_shares_one_line_and_names_each_quota(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for mode in [crate::state::Mode::Dark, crate::state::Mode::Light] {
        for rail in [false, true] {
            let mut state = State::default();
            state.appearance.mode = mode;
            let (window, shark) = boot(cx, state, WindowOptions::default());
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.rail = rail;
                    fill_usage(s);
                    cx.notify();
                });
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                let footer = w.find("usage-footer").bounds();
                let claude = w.find("usage-footer-quota-claude").bounds();
                let codex = w.find("usage-footer-quota-codex").bounds();
                assert_eq!(claude.top(), codex.top(), "both providers share one line");
                assert!(codex.left() > claude.right());
                assert!(footer.size.height <= px(if rail { 64. } else { 48. }));
                for id in ["usage-footer-quota-claude", "usage-footer-quota-codex"] {
                    let element = w.find(id);
                    assert!(element.visible());
                    let bounds = element.bounds();
                    assert!(
                        bounds.left() >= footer.left() && bounds.right() <= footer.right(),
                        "{id}: {bounds:?} outside {footer:?}"
                    );
                }
                assert!(w.find("usage-footer-quotas").visible());
                assert_eq!(w.try_find("usage-footer-pace-claude").is_some(), !rail);
                if !rail {
                    // The row fills the button, so the readout leads instead of centring (button padding is p_1).
                    let button = w.find("show-usage").bounds();
                    let dot = w.find("usage-footer-pace-claude").bounds();
                    assert!(
                        (dot.left() - button.left()).abs() <= px(4. + 3.),
                        "first quota must lead the button: {dot:?} in {button:?}"
                    );
                    assert!(
                        claude.left() - button.left() <= px(4. + 6. + 3. + 3.),
                        "quota text must start at the leading edge: {claude:?} in {button:?}"
                    );
                }
                let label = w.find("show-usage").label().unwrap_or_default().to_owned();
                assert!(
                    label.ends_with(". Open usage"),
                    "accessibility label must end with the action: {label}"
                );
                assert!(label.contains("Claude · 5 hours · 42% left"), "{label}");
                assert!(label.contains("Codex · 5 hours · 68% left"), "{label}");
                w.remove_window();
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn refresh_keeps_footer_and_panel_anchor_stable(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for rail in [false, true] {
        for (width, height) in [(640., 480.), (1120., 840.)] {
            let (window, shark) = boot(
                cx,
                State::default(),
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(width), px(height)),
                    ))),
                    ..Default::default()
                },
            );
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.rail = rail;
                    for snapshot in &mut s.usage {
                        snapshot.finish(Ok(weekly(42.)));
                    }
                    cx.notify();
                });
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                let footer = w.find("usage-footer").bounds();
                for remaining in [100., 42.] {
                    shark.update(cx, |s, cx| {
                        for snapshot in &mut s.usage {
                            snapshot.allowances[0].remaining = Some(remaining);
                        }
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let claude = w.find("usage-footer-quota-claude").bounds();
                    let codex = w.find("usage-footer-quota-codex").bounds();
                    assert_eq!(claude.top(), codex.top());
                    assert!(codex.left() > claude.right());
                    assert!(
                        codex.right() <= footer.right(),
                        "quotas must fit the navigation"
                    );
                    assert!(footer.size.height <= px(if rail { 64. } else { 48. }));
                }
                w.click("show-usage", cx);
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                assert_eq!(
                    w.find("usage-footer").bounds(),
                    footer,
                    "opening and refreshing must not resize the footer; rail={rail}"
                );
                let panel = w.find("usage-panel").bounds();
                let viewport = w.viewport_size();
                let assert_panel = |current: Bounds<Pixels>| {
                    assert_eq!(current.left(), panel.left());
                    assert_eq!(current.bottom(), panel.bottom());
                    assert_eq!(current.size.width, panel.size.width);
                    assert!(
                        current.top() >= px(8.)
                            && current.left() >= px(8.)
                            && current.right() <= viewport.width - px(8.)
                            && current.bottom() <= viewport.height - px(8.),
                        "dynamic Usage content must fit the viewport: {current:?}"
                    );
                };
                for provider in 0..2 {
                    shark.update(cx, |s, cx| {
                        s.usage[provider].finish(Ok(weekly(42.)));
                        cx.notify();
                    });
                    w.render_frame(cx);
                    w.refresh();
                    w.render_frame(cx);
                    assert_eq!(w.find("usage-footer").bounds(), footer);
                    assert_panel(w.find("usage-panel").bounds());
                }
                assert!(w.find("refresh-usage").visible());
                w.click("refresh-usage", cx);
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                assert_eq!(w.find("usage-footer").bounds(), footer);
                assert_panel(w.find("usage-panel").bounds());
                for result in [
                    Err(crate::usage::Failure::Temporary),
                    Err(crate::usage::Failure::SignIn),
                    Ok(weekly(42.)),
                ] {
                    shark.update(cx, |s, cx| {
                        for snapshot in &mut s.usage {
                            snapshot.finish(result.clone());
                        }
                        cx.notify();
                    });
                    w.render_frame(cx);
                    w.refresh();
                    w.render_frame(cx);
                    assert_eq!(
                        w.find("usage-footer").bounds(),
                        footer,
                        "error and recovery states must preserve footer geometry"
                    );
                    assert_panel(w.find("usage-panel").bounds());
                }
                w.remove_window();
            })
            .unwrap();
        }
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn footer_and_panel_fit_small_windows(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for mode in [crate::state::Mode::Dark, crate::state::Mode::Light] {
        for rail in [false, true] {
            for (width, height) in [(640., 480.), (800., 600.)] {
                let mut state = State::default();
                state.appearance.mode = mode;
                let (window, shark) = boot(
                    cx,
                    state,
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(width), px(height)),
                        ))),
                        ..Default::default()
                    },
                );
                cx.update_window(window, |_, w, cx| {
                    shark.update(cx, |s, cx| {
                        s.rail = rail;
                        fill_usage(s);
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let button = w.find("show-usage").bounds();
                    assert!(button.left() >= px(0.) && button.bottom() <= w.viewport_size().height);
                    w.click("show-usage", cx);
                    w.render_frame(cx);
                    let panel = w.find("usage-panel").bounds();
                    assert!(
                        panel.left() >= px(0.)
                            && panel.top() >= px(0.)
                            && panel.right() <= w.viewport_size().width
                            && panel.bottom() <= w.viewport_size().height,
                        "popover outside viewport {panel:?} vs {:?}",
                        w.viewport_size()
                    );
                    w.remove_window();
                })
                .unwrap();
            }
        }
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn keyboard_reaches_each_usage_control_and_restores_previous_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.sidebar_focus.focus(w, cx);
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        w.press("tab", cx);
        w.render_frame(cx);
        assert_eq!(
            w.find("refresh-usage").focused(),
            Some(true),
            "Tab must reach refresh-usage"
        );
        w.press("escape", cx);
        assert!(shark.read(cx).usage_panel.is_none());
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn window_owned_panel_coalesces_open_and_releases_on_close(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.sidebar_focus.focus(w, cx);
            s.close_usage(w, cx);
            assert!(s.sidebar_focus.is_focused(w));
            s.show_usage(w, cx);
        });
        let original = w.focused(cx);
        shark.update(cx, |s, cx| s.show_usage(w, cx));
        assert_eq!(
            w.focused(cx),
            original,
            "second open keeps the live focus scope/subscription"
        );
        w.render_frame(cx);
        w.pointer_down(
            point(w.viewport_size().width - px(8.), px(200.)),
            MouseButton::Left,
            cx,
        );
        w.pointer_up(
            point(w.viewport_size().width - px(8.), px(200.)),
            MouseButton::Left,
            cx,
        );
        assert!(shark.read(cx).usage_panel.is_none());
        shark.update(cx, |s, cx| {
            s.close_usage(w, cx);
            s.show_usage(w, cx);
        });
        assert_ne!(
            w.focused(cx),
            original,
            "reopen owns a fresh scope after teardown"
        );
        w.press("escape", cx);
        assert!(shark.read(cx).usage_panel.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn about_dialog_shows_the_license(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        crate::settings::about(w, cx);
        w.render_frame(cx);
        assert!(w.find("license-shark").visible());
    })
    .unwrap();
}
