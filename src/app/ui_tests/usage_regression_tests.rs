//! Footer, panel, and settings regressions for usage: quota readout, panel open and close, token sources.
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

/// Quotas of 42% and 68% left, plus a token ledger for every provider.
fn fill_usage(s: &mut Shark) {
    let now = crate::usage::now();
    for index in 0..s.usage_providers.len() {
        s.usage_reports[index] = crate::usage::logs::Report::from_entries(
            vec![crate::usage::logs::Entry {
                id: format!("usage-first-{index}"),
                date: now,
                model: "fixture".into(),
                input: 100,
                cache_read: 200,
                explicit_cost: Some(1.25),
                cost_is_estimate: Some(true),
                ..Default::default()
            }],
            now,
        );
        s.usage_activity[index] = s.usage_reports[index].daily.clone();
    }
    for (index, snapshot) in s.usage.iter_mut().enumerate() {
        snapshot.finish(Ok(weekly(if index == 0 { 42. } else { 68. })));
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn footer_opens_usage_in_both_layouts_and_details_toggle(cx: &mut TestAppContext) {
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
            assert!(w.try_find("usage-trend").is_none());
            assert!(w.try_find("usage-accounting-claude").is_none());
            for provider in ["claude", "codex"] {
                assert!(w.try_find(format!("usage-heatmap-{provider}")).is_none());
            }
            w.click("usage-toggle-details", cx);
            w.render_frame(cx);
            assert!(w.find("usage-trend").visible());
            assert!(w.try_find("usage-accounting-claude").is_some());
            w.click("usage-toggle-details", cx);
            w.render_frame(cx);
            assert!(w.try_find("usage-trend").is_none());
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
async fn usage_accounting_scrolls_without_horizontal_clipping(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State::default(),
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(800.), px(600.)),
            ))),
            ..Default::default()
        },
    );
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            for provider in crate::usage::Provider::ALL {
                s.usage_reports[provider.index()] = crate::usage::logs::Report::from_entries(
                    vec![crate::usage::logs::Entry {
                        id: "fixture".into(),
                        date: crate::usage::now(),
                        model: "fixture".into(),
                        input: 100,
                        cache_read: 200,
                        explicit_cost: Some(1.25),
                        cost_is_estimate: Some(true),
                        ..Default::default()
                    }],
                    crate::usage::now(),
                );
            }
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        w.click("usage-toggle-details", cx);
        w.render_frame(cx);
        let panel = w.find("usage-panel").bounds();
        for provider in crate::usage::Provider::ALL {
            let accounting = w
                .find(format!("usage-accounting-{}", provider.source().id()))
                .bounds();
            assert!(
                accounting.left() >= panel.left() && accounting.right() <= panel.right(),
                "inclusive accounting must fit panel"
            );
        }
        w.scroll(
            "usage-panel",
            ScrollDelta::Pixels(point(px(0.), px(-2000.))),
            cx,
        );
        w.render_frame(cx);
        assert!(
            w.find("usage-accounting-codex").visible(),
            "second provider's cache/cost breakdown must be reachable"
        );
    })
    .unwrap();
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
        for id in [
            "usage-open-recap",
            "refresh-usage",
            "usage-provider-all",
            "usage-provider-tab-claude",
            "usage-provider-tab-codex",
            "usage-toggle-details",
        ] {
            w.press("tab", cx);
            w.render_frame(cx);
            assert_eq!(w.find(id).focused(), Some(true), "Tab must reach {id}");
        }
        w.press("enter", cx);
        w.render_frame(cx);
        assert_eq!(w.find("usage-toggle-details").checked(), Some(true));
        w.press("space", cx);
        w.render_frame(cx);
        assert_eq!(w.find("usage-toggle-details").checked(), Some(false));
        w.press("shift-tab", cx);
        w.render_frame(cx);
        assert_eq!(
            w.find("usage-provider-tab-codex").focused(),
            Some(true),
            "Shift-Tab must walk back through the same order"
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
async fn about_dialog_holds_licenses(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        crate::settings::about(w, cx);
        w.render_frame(cx);
        assert!(w.find("license-shark").visible());
        let initial = w.find("license-shark").bounds().top();
        w.scroll("about", ScrollDelta::Pixels(point(px(0.), px(-1600.))), cx);
        w.render_frame(cx);
        assert!(w.find("license-shark").bounds().top() < initial);
        assert!(w.try_find("license-poketokenbar").is_some());
    })
    .unwrap();
}

#[test]
fn folder_entries_the_scan_drops_are_listed_as_ignored() {
    let home = std::path::Path::new("/Users/me");
    assert_eq!(
        crate::settings::ignored_roots("/a, rel,\n~/x, ./y, ,/, ~", home),
        ["rel", "./y", "/", "~"]
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn source_folder_drafts_commit_only_the_selected_provider(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state
        .usage_roots
        .insert("codex".into(), "~/codex-existing".into());
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        state.save().unwrap();
        let settings =
            cx.new(|cx| crate::settings::Settings::new(shark.downgrade(), &state, w, cx));
        let claude = settings.read(cx).roots_input("claude");
        let codex = settings.read(cx).roots_input("codex");
        claude.update(cx, |input, cx| {
            input.set_value("~/claude-a, ~/claude-b", w, cx)
        });
        codex.update(cx, |input, cx| input.set_value("~/codex-draft", w, cx));
        assert!(!State::load().usage_roots.contains_key("claude"));
        assert_eq!(State::load().usage_roots["codex"], "~/codex-existing");
        settings.update(cx, |s, cx| s.save_roots(0, cx));
        assert_eq!(
            State::load().usage_roots["claude"],
            "~/claude-a, ~/claude-b"
        );
        assert_eq!(State::load().usage_roots["codex"], "~/codex-existing");
        settings.update(cx, |s, cx| s.save_roots(1, cx));
        assert_eq!(State::load().usage_roots["codex"], "~/codex-draft");
        claude.update(cx, |input, cx| input.set_value("  ", w, cx));
        settings.update(cx, |s, cx| s.save_roots(0, cx));
        assert!(!State::load().usage_roots.contains_key("claude"));
        assert_eq!(State::load().usage_roots["codex"], "~/codex-draft");
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        w.click("Token sources", cx);
        w.render_frame(cx);
        assert!(w.try_find("token-roots-save-claude").is_none());
        assert!(w.try_find("token-roots-save-codex").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn source_folders_are_saved_when_the_dialog_closes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        let settings =
            cx.new(|cx| crate::settings::Settings::new(shark.downgrade(), &state, w, cx));
        let claude = settings.read(cx).roots_input("claude");
        let codex = settings.read(cx).roots_input("codex");
        codex.update(cx, |input, cx| input.set_value("/tmp/close-b", w, cx));
        assert!(!State::load().usage_roots.contains_key("codex"));
        // Blur saves too (see Settings::new); closing must not lose a field that never blurred.
        drop((settings, claude, codex));
    })
    .unwrap();
    // Release callbacks run when the update that dropped the entity ends.
    assert_eq!(State::load().usage_roots["codex"], "/tmp/close-b");
}
