use super::*;
use crate::usage::{Allowance, Failure, Status};
use gpui_kit::test::TestEventExt;

fn sample() -> Vec<Allowance> {
    vec![
        Allowance {
            label: "5 hours".into(),
            remaining: Some(20.),
            resets_at: Some(1793184600.),
        },
        Allowance {
            label: "Weekly".into(),
            remaining: Some(0.),
            resets_at: Some(1793311200.),
        },
        Allowance {
            label: "Fable".into(),
            remaining: None,
            resets_at: None,
        },
    ]
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn usage_independent_states_refresh_and_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.sidebar_focus.focus(w, cx));
    press_in(window)(cx, &["cmd-k"]);
    cx.update_window(window, |_, w, cx| {
        w.input("Show usage", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("usage-panel").visible());
        assert!(w.find("usage-status-Claude").visible());
        assert!(w.find("usage-status-Codex").visible());
        assert!(
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status == Status::Loading)
        );
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.usage[0].finish(Ok(sample()));
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("usage-Claude-0").visible());
        assert!(w.try_find("usage-status-Claude").is_none());
        assert!(w.find("usage-status-Codex").visible());
        w.click("refresh-usage", cx);
        assert!(
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status == Status::Loading)
        );
        assert_eq!(shark.read(cx).usage_queries, [2, 1]);
        let checked = shark.read(cx).usage[0].checked_at;
        w.click("refresh-usage", cx);
        assert_eq!(shark.read(cx).usage[0].checked_at, checked);
        assert_eq!(shark.read(cx).usage_queries, [2, 1]);
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.usage[0].finish(Err(Failure::Temporary));
        s.usage[1].finish(Err(Failure::SignIn));
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("usage-Claude-0").visible(),
            "temporary errors keep previous rows"
        );
        assert!(w.find("usage-status-Claude").visible());
        assert!(w.try_find("usage-Codex-0").is_none());
        w.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).usage_panel.is_none());
        assert!(
            shark.read(cx).sidebar_focus.is_focused(w),
            "palette dismissal restores original focus"
        );
        shark.update(cx, |s, cx| s.show_usage(w, cx));
        w.render_frame(cx);
        w.press("tab", cx);
        w.press("escape", cx);
        assert!(
            shark.read(cx).usage_panel.is_none(),
            "Escape also works from Refresh"
        );
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
        shark.update(cx, |s, cx| s.show_usage(w, cx));
        w.render_frame(cx);
        w.pointer_down(point(px(10.), px(300.)), MouseButton::Left, cx);
        w.pointer_up(point(px(10.), px(300.)), MouseButton::Left, cx);
        assert!(shark.read(cx).usage_panel.is_none());
        assert!(
            shark.read(cx).sidebar_focus.is_focused(w),
            "outside click restores focus"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn usage_polling_only_open_and_active(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.show_usage(w, cx);
        assert!(s.usage_panel.as_ref().unwrap().poll.is_some());
        for snapshot in &mut s.usage {
            snapshot.finish(Ok(sample()));
        }
    });
    cx.executor().advance_clock(Duration::from_secs(59));
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status == Status::Ready)
        )
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status == Status::Loading)
        )
    });
    let (other, _) = open(cx, WindowOptions::default());
    cx.update_window(other, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert!(s.usage_panel.as_ref().unwrap().poll.is_none());
        for snapshot in &mut s.usage {
            snapshot.finish(Ok(sample()));
        }
    });
    cx.executor().advance_clock(Duration::from_secs(120));
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status == Status::Ready)
        )
    });
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    on(cx, &|s, w, cx| {
        assert!(s.usage_panel.as_ref().unwrap().poll.is_some());
        s.close_usage(w, cx);
    });
    cx.executor().advance_clock(Duration::from_secs(120));
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status == Status::Ready)
        )
    });
    on(cx, &|s, w, cx| {
        s.show_usage(w, cx);
        assert!(
            s.usage.iter().all(|s| s.status == Status::Loading),
            "reopen fetches both"
        );
    });
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let file = home.join("usage-preview.rs");
    fs::create_dir_all(&home).unwrap();
    fs::write(&file, "fn main() {}\n").unwrap();
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for width in [800., 1440.] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(720.)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| cx.new(|cx| Shark::new(w, cx)),
                    )
                })
                .unwrap();
            for view in ["home", "chat", "diff", "file"] {
                cx.update_window(window, |_, w, cx| {
                    shark.update(cx, |s, cx| {
                        s.close_usage(w, cx);
                        s.home.on = view == "home";
                        s.active = (view != "home").then(|| "usage-preview".into());
                        s.state.workspaces = vec![Workspace {
                            name: "usage-preview".into(),
                            branch: "usage-preview".into(),
                            chats: vec![ChatMeta {
                                id: "usage-preview".into(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }];
                        s.state.workspaces[0].normalize_tabs();
                        s.tab = match view {
                            "diff" => Tab::Feature(Feature::Diff),
                            "file" => Tab::File,
                            _ => Tab::Chat,
                        };
                        if view == "file" {
                            s.editor = Some(cx.new(|cx| {
                                FileEditor::new(
                                    "usage-preview.rs".into(),
                                    file.clone(),
                                    false,
                                    w,
                                    cx,
                                )
                            }));
                        }
                        s.show_usage(w, cx);
                        s.usage[0].finish(Ok(sample()));
                        s.usage[1].finish(Ok(vec![Allowance {
                            label: "Weekly".into(),
                            remaining: Some(68.),
                            resets_at: Some(1793774588.),
                        }]));
                        cx.notify();
                    });
                    w.render_frame(cx);
                    // Anchor geometry is captured in prepaint.
                    w.refresh();
                    w.render_frame(cx);
                    let button = w.find("show-usage").bounds();
                    let panel = w.find("usage-panel").bounds();
                    let bar = w.find("workspace-titlebar").bounds();
                    assert!(button.left() >= bar.left() && button.right() <= bar.right());
                    assert!(button.top() >= bar.top() && button.bottom() <= bar.bottom());
                    assert!(
                        (panel.right() - button.right()).abs() <= px(1.),
                        "{view} {label} {width}: panel={panel:?}, button={button:?}, anchor={:?}",
                        shark.read(cx).usage_at.get()
                    );
                    assert!(panel.left() >= px(8.) && panel.bottom() <= px(712.));
                    assert!(w.find("refresh-usage").visible());
                    assert!(w.find("usage-Codex-0").visible());
                })
                .unwrap();
                let screenshot = cx.capture_screenshot(window).expect("real usage renderer");
                if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                    let dir = PathBuf::from(dir);
                    fs::create_dir_all(&dir).unwrap();
                    screenshot
                        .save(dir.join(format!("usage-{view}-{label}-{}.png", width as usize)))
                        .unwrap();
                }
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
}

/// Opt-in integration: production panel and background queries, real authenticated CLIs.
/// Only the polling clock is accelerated; query responses and process cleanup are real.
#[gpui_kit::test]
#[ignore = "read-only installed CLIs and live account allowances, no inference"]
#[allow(clippy::await_holding_lock)]
async fn live_usage_panel(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.usage_live = true;
        s.sidebar_focus.focus(w, cx);
        w.activate_window();
    });
    press_in(window)(cx, &["cmd-k"]);
    cx.update_window(window, |_, w, cx| {
        w.input("Show usage", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    for round in 1..=4 {
        cx.wait_for(window, Duration::from_secs(30), |_, cx| {
            shark
                .read(cx)
                .usage
                .iter()
                .all(|s| s.status != Status::Loading)
        })
        .await;
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let s = shark.read(cx);
            assert_eq!(s.usage_queries, [round; 2]);
            for (provider, snapshot) in crate::usage::Provider::ALL.into_iter().zip(&s.usage) {
                assert_eq!(
                    snapshot.status,
                    Status::Ready,
                    "{}: {:?}",
                    provider.name(),
                    snapshot
                );
                assert!(
                    snapshot
                        .allowances
                        .iter()
                        .any(|row| row.remaining.is_some() && row.resets_at.is_some())
                );
                assert!(w.find(format!("usage-{}-0", provider.name())).visible());
            }
            eprintln!("live panel round {round}: both providers rendered real allowances");
        })
        .unwrap();
        match round {
            1 => {
                cx.update_window(window, |_, w, cx| {
                    w.click("refresh-usage", cx);
                    w.click("refresh-usage", cx);
                    assert_eq!(
                        shark.read(cx).usage_queries,
                        [2; 2],
                        "overlapping refresh coalesces"
                    );
                })
                .unwrap();
            }
            2 => {
                press_in(window)(cx, &["escape"]);
                cx.update_window(window, |_, w, cx| {
                    assert!(shark.read(cx).usage_panel.is_none());
                    assert!(shark.read(cx).sidebar_focus.is_focused(w));
                })
                .unwrap();
                cx.executor().advance_clock(Duration::from_secs(120));
                cx.run_until_parked();
                cx.update(|cx| {
                    assert_eq!(
                        shark.read(cx).usage_queries,
                        [2; 2],
                        "closed panel never polls"
                    )
                });
                on(cx, &|s, w, cx| s.show_usage(w, cx));
            }
            3 => {
                let (other, _) = open(cx, WindowOptions::default());
                cx.update_window(other, |_, w, _| w.activate_window())
                    .unwrap();
                cx.run_until_parked();
                cx.executor().advance_clock(Duration::from_secs(120));
                cx.run_until_parked();
                cx.update(|cx| {
                    assert_eq!(
                        shark.read(cx).usage_queries,
                        [3; 2],
                        "inactive window never polls"
                    )
                });
                cx.update_window(window, |_, w, _| w.activate_window())
                    .unwrap();
                cx.run_until_parked();
                cx.executor().advance_clock(Duration::from_secs(60));
                cx.run_until_parked();
            }
            _ => {}
        }
    }
    cx.update_window(window, |_, w, cx| {
        w.pointer_down(point(px(10.), px(300.)), MouseButton::Left, cx);
        w.pointer_up(point(px(10.), px(300.)), MouseButton::Left, cx);
        assert!(shark.read(cx).usage_panel.is_none());
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
    })
    .unwrap();
    let saved = State::load();
    assert!(saved.workspaces.is_empty() && saved.repos.is_empty());
    assert!(
        !home
            .join("Library/Application Support/shark/transcripts")
            .exists()
    );
}
