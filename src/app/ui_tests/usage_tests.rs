use super::*;
use crate::usage::{Allowance, Failure, Status};
use gpui_kit::test::TestEventExt;

fn sample() -> Vec<Allowance> {
    vec![
        Allowance {
            label: "5 hours".into(),
            remaining: Some(20.),
            resets_at: Some(1793184600.),
            window: Some(5. * 3600.),
        },
        Allowance {
            label: "Weekly".into(),
            remaining: Some(0.),
            resets_at: Some(1793311200.),
            window: Some(604800.),
        },
        Allowance {
            label: "Fable".into(),
            remaining: None,
            resets_at: None,
            window: None,
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
        assert!(w.find("usage-status-claude").visible());
        assert!(w.find("usage-status-codex").visible());
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
        assert!(w.find("usage-claude-0").visible());
        assert!(w.try_find("usage-status-claude").is_none());
        assert!(w.find("usage-status-codex").visible());
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
            w.find("usage-claude-0").visible(),
            "temporary errors keep previous rows"
        );
        assert!(w.find("usage-status-claude").visible());
        assert!(w.try_find("usage-codex-0").is_none());
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
        w.pointer_down(
            point(w.viewport_size().width - px(10.), px(300.)),
            MouseButton::Left,
            cx,
        );
        w.pointer_up(
            point(w.viewport_size().width - px(10.), px(300.)),
            MouseButton::Left,
            cx,
        );
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
async fn usage_footer_availability_and_click(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("show-usage").visible(),
            "no results keeps a neutral control"
        );
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.usage[0].finish(Ok(vec![Allowance {
            label: "Weekly".into(),
            remaining: None,
            resets_at: None,
            window: None,
        }]));
        s.usage[1].finish(Ok(sample()));
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("show-usage").is_none());
        assert!(
            w.find("usage-trigger-claude").visible(),
            "unknown allowance still gets a neutral ring"
        );
        assert!(w.find("usage-trigger-codex").visible());
        w.click("usage-trigger-claude", cx);
        assert!(shark.read(cx).usage_panel.is_some());
        w.press("escape", cx);
        w.click("usage-trigger-codex", cx);
        assert!(shark.read(cx).usage_panel.is_some());
        w.press("escape", cx);
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
            w.find("usage-trigger-claude").visible(),
            "temporary failure keeps cached ring"
        );
        assert!(
            w.try_find("usage-trigger-codex").is_none(),
            "invalid auth removes ring"
        );
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.usage[0].finish(Err(Failure::Unsupported));
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("show-usage").visible());
        w.click("show-usage", cx);
        w.render_frame(cx);
        assert!(w.find("usage-status-claude").visible());
        assert!(w.find("usage-status-codex").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn usage_polling_startup_cadence_and_activation(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, _| {
        assert_eq!(
            s.usage_queries, [1; 2],
            "startup fetches without opening details"
        );
        assert!(s.usage_panel.is_none());
        for snapshot in &mut s.usage {
            snapshot.finish(Ok(sample()));
        }
        w.activate_window();
    });
    cx.executor().advance_clock(Duration::from_secs(299));
    cx.run_until_parked();
    on(cx, &|s, w, cx| {
        assert_eq!(s.usage_queries, [1; 2]);
        s.show_usage(w, cx);
        s.refresh_usage(cx);
        assert_eq!(s.usage_queries, [2; 2], "open and refresh coalesce");
        for snapshot in &mut s.usage {
            snapshot.finish(Ok(sample()));
        }
        s.close_usage(w, cx);
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert_eq!(
            s.usage_queries, [3; 2],
            "opening details does not move the five-minute deadline"
        );
        assert!(s.usage_panel.is_none(), "closed details still poll");
        assert!(s.usage_poll.is_some());
        for snapshot in &mut s.usage {
            snapshot.finish(Ok(sample()));
        }
    });
    let (other, _) = open(cx, WindowOptions::default());
    cx.update_window(other, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    on(cx, &|s, _, _| assert!(s.usage_poll.is_none()));
    cx.executor().advance_clock(Duration::from_secs(100));
    cx.run_until_parked();
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert_eq!(
            s.usage_queries, [3; 2],
            "fresh activation waits for original deadline"
        )
    });
    cx.executor().advance_clock(Duration::from_secs(199));
    cx.run_until_parked();
    on(cx, &|s, _, _| assert_eq!(s.usage_queries, [3; 2]));
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert_eq!(s.usage_queries, [4; 2]);
        for snapshot in &mut s.usage {
            snapshot.finish(Ok(sample()));
        }
    });
    cx.update_window(other, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(600));
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert_eq!(s.usage_queries, [4; 2], "inactive window does not poll")
    });
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert_eq!(
            s.usage_queries, [5; 2],
            "overdue activation fetches immediately"
        )
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
            for rail in [false, true] {
                for available in ["both", "one", "none"] {
                    for view in ["home", "chat", "diff", "file"] {
                        cx.update_window(window, |_, w, cx| {
                            shark.update(cx, |s, cx| {
                                s.close_usage(w, cx);
                                s.rail = rail;
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
                                let today = crate::usage::activity::local_day(crate::usage::now());
                                s.usage_activity[0] = crate::usage::activity::Daily {
                                    first_day: today - 139,
                                    tokens: (0..140u64)
                                        .map(|i| match i * 7919 % 11 {
                                            0..=2 => 0,
                                            n => n * n * 40_000 + i * 9_000,
                                        })
                                        .collect(),
                                };
                                s.show_usage(w, cx);
                                s.usage[0].finish(Ok(sample()));
                                s.usage[1].finish(Ok(vec![Allowance {
                                    label: "Weekly".into(),
                                    remaining: Some(68.),
                                    resets_at: Some(1793774588.),
                                    window: Some(604800.),
                                }]));
                                if available != "both" {
                                    s.usage[1].finish(Err(Failure::MissingCli));
                                }
                                if available == "none" {
                                    s.usage[0].finish(Err(Failure::SignIn));
                                }
                                cx.notify();
                            });
                            w.render_frame(cx);
                            // Anchor geometry is captured in prepaint.
                            w.refresh();
                            w.render_frame(cx);
                            let footer = w.find("usage-footer").bounds();
                            let button = w
                                .find(if available == "none" {
                                    "show-usage"
                                } else {
                                    "usage-trigger-claude"
                                })
                                .bounds();
                            let panel = w.find("usage-panel").bounds();
                            assert!(
                                button.left() >= footer.left() && button.right() <= footer.right()
                            );
                            assert!(
                                button.top() >= footer.top() && button.bottom() <= footer.bottom()
                            );
                            assert!(
                                (footer.bottom() - px(720.)).abs() <= px(1.),
                                "footer stays at bottom: {footer:?}"
                            );
                            assert!(
                                (panel.bottom() - footer.top() + px(4.)).abs() <= px(1.),
                                "{view} {label} {width}: panel={panel:?}, footer={footer:?}"
                            );
                            assert!(panel.left() >= px(8.) && panel.top() >= px(8.));
                            assert!(w.find("refresh-usage").visible());
                            if available == "both" {
                                let codex = w.find("usage-trigger-codex").bounds();
                                assert!(w.find("usage-codex-0").visible());
                                assert!(codex.top() >= button.bottom());
                            } else {
                                assert!(w.try_find("usage-trigger-codex").is_none());
                            }
                        })
                        .unwrap();
                        let screenshot =
                            cx.capture_screenshot(window).expect("real usage renderer");
                        if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                            let dir = PathBuf::from(dir);
                            fs::create_dir_all(&dir).unwrap();
                            screenshot
                                .save(dir.join(format!(
                                    "usage-{view}-{label}-{}-{rail}-{available}.png",
                                    width as usize
                                )))
                                .unwrap();
                        }
                        if view != "home" || available != "both" {
                            continue;
                        }
                        // The hover card over a seeded heatmap, panel closed.
                        cx.update_window(window, |_, w, cx| {
                            shark.update(cx, |s, cx| {
                                s.close_usage(w, cx);
                                cx.notify();
                            });
                            w.render_frame(cx);
                            w.hover("usage-trigger-claude", cx);
                        })
                        .unwrap();
                        cx.advance_clock(Duration::from_secs(1));
                        cx.update_window(window, |_, w, cx| {
                            w.render_frame(cx);
                            let card = w.find("usage-card-claude").bounds();
                            assert!(card.top() >= px(0.) && card.right() <= px(width));
                            let footer = w.find("usage-footer").bounds();
                            assert!(card.bottom() <= footer.top() + px(8.), "{card:?}");
                        })
                        .unwrap();
                        let screenshot = cx.capture_screenshot(window).expect("hover card");
                        if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                            screenshot
                                .save(PathBuf::from(dir).join(format!(
                                    "usage-hover-{label}-{}-{rail}.png",
                                    width as usize
                                )))
                                .unwrap();
                        }
                        cx.update_window(window, |_, w, cx| {
                            w.pointer_move(point(px(width - 10.), px(10.)), None, cx);
                        })
                        .unwrap();
                        cx.advance_clock(Duration::from_secs(1));
                    }
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
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.usage_live = true;
            s.usage = Default::default();
            s.usage_queries = vec![0; 2];
            s.usage_poll = None;
            s.usage_due = None;
            s.usage_polling(w, cx);
            s.sidebar_focus.focus(w, cx);
            w.activate_window();
        });
        // Open in the same update so startup and palette requests overlap.
        w.press("cmd-k", cx);
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
                assert!(
                    w.find(format!("usage-trigger-{}", provider.source().id()))
                        .visible()
                );
                if s.usage_panel.is_some() {
                    assert!(
                        w.find(format!("usage-{}-0", provider.source().id()))
                            .visible()
                    );
                }
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
                cx.executor().advance_clock(Duration::from_secs(300));
                cx.run_until_parked();
                cx.update(|cx| {
                    assert_eq!(
                        shark.read(cx).usage_queries,
                        [3; 2],
                        "closed panel polls at five minutes"
                    )
                });
            }
            3 => {
                let (other, _) = open(cx, WindowOptions::default());
                cx.update_window(other, |_, w, _| w.activate_window())
                    .unwrap();
                cx.run_until_parked();
                cx.executor().advance_clock(Duration::from_secs(600));
                cx.run_until_parked();
                cx.update(|cx| {
                    assert_eq!(
                        shark.read(cx).usage_queries,
                        [3; 2],
                        "inactive window never polls"
                    )
                });
                cx.update_window(window, |_, w, cx| {
                    w.activate_window();
                    shark.update(cx, |s, cx| s.show_usage(w, cx));
                })
                .unwrap();
                cx.run_until_parked();
            }
            _ => {}
        }
    }
    cx.update_window(window, |_, w, cx| {
        w.pointer_down(
            point(w.viewport_size().width - px(10.), px(300.)),
            MouseButton::Left,
            cx,
        );
        w.pointer_up(
            point(w.viewport_size().width - px(10.), px(300.)),
            MouseButton::Left,
            cx,
        );
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
