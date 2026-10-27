use super::super::workspace_picker::WorkspacePicker;
use super::*;
use gpui_kit::test::TestEventExt;

fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    test_home();
    boot(
        cx,
        State {
            workspaces: ["current", "recent", "older", "unknown"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| Workspace {
                    name: name.into(),
                    branch: name.into(),
                    last_interacted_at: [Some(30), Some(20), Some(10), None][i],
                    repos: vec![crate::state::WsRepo {
                        name: if i == 1 { "payments" } else { "api" }.into(),
                        src: PathBuf::new(),
                        base: "main".into(),
                        branch: None,
                    }],
                    ..Default::default()
                })
                .collect(),
            sections: vec![Section {
                name: "Folded".into(),
                icon: "folder".into(),
                collapsed: true,
                workspaces: vec!["recent".into()],
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    )
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_picker_navigation_and_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let press = press_in(window);
    let on = on_app(window, shark.clone());
    // Home chooses the newest workspace, including all folded entries.
    press(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(Modifiers::default(), cx);
        assert!(w.has_active_dialog(cx));
    })
    .unwrap();
    press(cx, &["enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("current")));
    on(cx, &|s, _, _| s.rail = true);
    press(cx, &["cmd-shift-o", "enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("recent"));
        assert!(!s.state.sections[0].collapsed);
        assert!(s.state.workspaces[1].last_interacted_at.unwrap() > 20);
    });
    let focus = cx
        .update_window(window, |_, w, cx| w.focused(cx).unwrap())
        .unwrap();
    press(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| w.input("pay", cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx), "one Escape after typing");
        assert_eq!(w.focused(cx), Some(focus.clone()));
    })
    .unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    press(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| w.input("pay", cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, &["enter"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active.as_deref(), Some("recent"));
        assert_eq!(
            shark.read(cx).tab,
            Tab::Diff,
            "current selection preserves view"
        );
    });
    on(cx, &|s, w, cx| s.enter_home(w, cx));
    press(cx, &["cmd-shift-o", "enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("recent")));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_picker_keeps_identity_during_updates(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let picker = cx
        .update_window(window, |_, w, cx| {
            let picker = cx.new(|cx| {
                WorkspacePicker::new(
                    WorkspacePicker::snapshot(shark.read(cx), cx),
                    Some("current".into()),
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
    press_in(window)(cx, &["down"]);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.workspaces[3].last_interacted_at = Some(100);
        s.state.workspaces.remove(0);
        let ws = &s.state.workspaces[0];
        let mut repo = crate::sync::Repo::fresh(&ws.repos[0], ws);
        repo.pr = Some(Err("offline".into()));
        s.attn.set_checked("recent", vec![repo]);
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let p = picker.read(cx);
        assert_eq!(p.hits, ["recent", "older", "unknown"]);
        assert_eq!(
            p.hits[p.state.read(cx).selected_index().unwrap().row],
            "older"
        );
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("older")));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_picker_discard_revalidates_destination(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.open_workspace(0, w, cx));
    let path = cx.update(|cx| {
        shark
            .read(cx)
            .active_ws()
            .unwrap()
            .dir()
            .join("api/edit.txt")
    });
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "original").unwrap();
    on(cx, &|s, w, cx| {
        s.open_path_tab("api/edit.txt".into(), w, cx)
    });
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "original"
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("changed ", cx))
        .unwrap();
    cx.run_until_parked();
    for scenario in ["cancel", "archiving", "removed"] {
        press_in(window)(cx, &["cmd-shift-o", "enter"]);
        cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
            .unwrap();
        on(cx, &|s, _, _| match scenario {
            "archiving" => {
                s.archiving.insert("recent".into());
            }
            "removed" => {
                s.state.workspaces.remove(1);
            }
            _ => {}
        });
        press_in(window)(
            cx,
            &[if scenario == "cancel" {
                "escape"
            } else {
                "enter"
            }],
        );
        cx.update(|cx| {
            let s = shark.read(cx);
            assert_eq!(s.active.as_deref(), Some("current"));
            assert_eq!(s.editor.as_ref(), Some(&editor));
            assert!(editor.read(cx).dirty, "{scenario} keeps dirty state");
            if let Some(ws) = s.ws("recent") {
                assert_eq!(ws.last_interacted_at, Some(20));
            }
        });
        on(cx, &|s, _, _| s.archiving.clear());
    }
    assert_eq!(fs::read_to_string(path).unwrap(), "original");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_interaction_excludes_background_and_flushes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.state.last_open = Some(("current".into(), "chat".into()));
        s.restore_last_open(w, cx);
        assert_eq!(s.state.workspaces[0].last_interacted_at, Some(30));
    });
    press_in(window)(cx, &["cmd-shift-o", "down", "escape"]);
    on(cx, &|s, _, cx| {
        s.turn_done("current", cx);
        assert_eq!(s.state.workspaces[0].last_interacted_at, Some(30));
    });
    press_in(window)(cx, &["a"]);
    on(cx, &|s, w, cx| {
        assert!(
            s.state.workspaces[0].last_interacted_at.unwrap() > 30,
            "keyboard interaction"
        );
        s.flush_interaction();
        let recorded = s.state.workspaces[0].last_interacted_at;
        assert_eq!(State::load().workspaces[0].last_interacted_at, recorded);
        s.state.workspaces[0].last_interacted_at = Some(31);
        s.enter_home(w, cx);
    });
    press_in(window)(cx, &["b"]);
    on(cx, &|s, w, cx| {
        assert_eq!(s.state.workspaces[0].last_interacted_at, Some(31));
        s.toggle_home(w, cx);
        s.record_interaction("current", cx);
        assert!(s.storage_guard(false, w, cx));
        assert_eq!(
            State::load().workspaces[0].last_interacted_at,
            s.state.workspaces[0].last_interacted_at
        );
    });
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, label) in [
        (crate::state::Mode::Light, "light"),
        (crate::state::Mode::Dark, "dark"),
    ] {
        for width in [1000., 320.] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(560.)),
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
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.rail = width < 400.;
                    s.state.workspaces = ["checkout-redesign", "fix-auth-with-a-very-long-workspace-name", "many-repositories", "unknown"].into_iter().enumerate().map(|(i, name)| Workspace {
                        name: name.into(), branch: name.into(), last_interacted_at: (i < 3).then_some(super::super::interaction::now_ms().saturating_sub(i as u64 * 720_000)),
                        repos: (0..[2, 1, 18, 3][i]).map(|r| crate::state::WsRepo {
                            name: if r == 0 { "web-with-a-long-repository-name".into() } else { format!("payments-{r}") }, src: PathBuf::new(), base: "main".into(), branch: None,
                        }).collect(), ..Default::default()
                    }).collect();
                    for ws in &s.state.workspaces {
                        let repos = ws.repos.iter().enumerate().map(|(i, r)| {
                            let mut repo = crate::sync::Repo::fresh(r, ws);
                            repo.pr = match i % 7 {
                                0..=3 => Some(Ok(crate::sync::parse_prs(&format!(r#"[{{"number":{},"state":"{}","isDraft":{},"statusCheckRollup":[{{"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}}]}}]"#, 142 + i, ["OPEN", "OPEN", "MERGED", "CLOSED"][i % 7], i == 0)))),
                                4 => Some(Ok(None)), 5 => None, _ => Some(Err("offline".into())),
                            };
                            repo
                        }).collect();
                        s.attn.set_checked(&ws.name, repos);
                    }
                    s.active = Some("fix-auth-with-a-very-long-workspace-name".into());
                    s.home.on = false;
                    s.open_workspace_picker(w, cx);
                });
                w.render_frame(cx);
                let first = w.find("workspace-result-checkout-redesign");
                assert!(first.visible());
                let bounds = first.bounds();
                assert!(bounds.left() >= px(16.) && bounds.right() <= px(width - 16.), "{label}, {width}: {bounds:?}");
                assert!(bounds.top() > px(0.) && bounds.bottom() < px(560.));
            }).unwrap();
            let image = cx
                .capture_screenshot(window)
                .expect("workspace picker Metal rendering");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                image
                    .save(dir.join(format!("workspace-picker-{label}-{}.png", width as usize)))
                    .unwrap();
            }
            cx.update_window(window, |_, w, cx| {
                w.press("down", cx);
                w.press("down", cx);
                w.press("down", cx);
                w.render_frame(cx);
                assert!(
                    w.find("workspace-result-unknown").visible(),
                    "selection scrolls past a row taller than the viewport"
                );
            })
            .unwrap();
            let image = cx.capture_screenshot(window).unwrap();
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                image
                    .save(PathBuf::from(dir).join(format!(
                        "workspace-picker-{label}-{}-scrolled.png",
                        width as usize
                    )))
                    .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    println!(
        "workspace picker pixels: both themes, narrow/wide, mixed PR states, long names and tall-row scrolling passed"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_interaction_mouse_scroll_and_dialog_success(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.open_workspace(0, w, cx);
        s.flush_interaction();
        s.state.workspaces[0].last_interacted_at = Some(1);
    });
    cx.update_window(window, |_, w, cx| w.click("content-island", cx))
        .unwrap();
    on(cx, &|s, _, _| {
        assert!(
            s.state.workspaces[0].last_interacted_at.unwrap() > 1,
            "click inside content"
        );
        s.state.workspaces[0].last_interacted_at = Some(1);
    });
    cx.update_window(window, |_, w, cx| {
        w.scroll(
            "content-island",
            ScrollDelta::Pixels(point(px(0.), px(-50.))),
            cx,
        )
    })
    .unwrap();
    on(cx, &|s, _, _| {
        assert!(
            s.state.workspaces[0].last_interacted_at.unwrap() > 1,
            "scroll inside content"
        );
        s.state.workspaces[0].last_interacted_at = Some(1);
    });
    press_in(window)(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| {
        w.scroll(
            "workspace-result-recent",
            ScrollDelta::Pixels(point(px(0.), px(-50.))),
            cx,
        )
    })
    .unwrap();
    press_in(window)(cx, &["escape"]);
    on(cx, &|s, w, cx| {
        assert_eq!(
            s.state.workspaces[0].last_interacted_at,
            Some(1),
            "picker scroll doesn't count"
        );
        w.activate_window();
        s.refresh_attention(cx);
    });
    on(cx, &|s, _, _| {
        assert_eq!(s.state.workspaces[0].last_interacted_at, Some(1))
    });
    // A model menu selection succeeds through ChatEvent::Picked; mere menu browsing does not.
    on(cx, &|s, w, cx| s.pick_model(w, cx));
    press_in(window)(cx, &["escape"]);
    on(cx, &|s, w, cx| {
        assert_eq!(s.state.workspaces[0].last_interacted_at, Some(1));
        s.set_model(0, w, cx);
    });
    on(cx, &|s, _, _| {
        assert!(s.state.workspaces[0].last_interacted_at.unwrap() > 1)
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_picker_empty_states_and_click(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    press_in(window)(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| w.input("zzzz", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("workspace-picker-no-match").visible());
        assert!(w.try_find("workspace-picker-empty").is_none());
    })
    .unwrap();
    press_in(window)(cx, &["escape", "cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| w.click("workspace-result-recent", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("recent")));
    on_app(window, shark)(cx, &|s, w, cx| {
        s.state.workspaces.clear();
        s.active = None;
        s.enter_home(w, cx);
    });
    press_in(window)(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("workspace-picker-empty").visible());
        assert!(w.try_find("workspace-picker-no-match").is_none());
    })
    .unwrap();
    press_in(window)(cx, &["enter", "escape"]);
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_interaction_coalesces_and_restores(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, _, cx| s.record_interaction("recent", cx));
    assert_eq!(
        State::load().workspaces[1].last_interacted_at,
        Some(20),
        "no immediate write"
    );
    cx.executor().advance_clock(Duration::from_millis(500));
    on(cx, &|s, _, cx| s.record_interaction("recent", cx));
    assert_eq!(State::load().workspaces[1].last_interacted_at, Some(20));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    on(cx, &|s, _, _| {
        assert!(
            s.interaction_save.is_none(),
            "continuous input doesn't defer the first save"
        );
        assert_eq!(
            State::load().workspaces[1].last_interacted_at,
            s.state.workspaces[1].last_interacted_at
        );
    });
    let (restored_window, restored) = open(cx, WindowOptions::default());
    press_in(restored_window)(cx, &["cmd-shift-o", "enter"]);
    cx.update(|cx| {
        assert_eq!(
            restored.read(cx).active.as_deref(),
            Some("recent"),
            "persisted recency selects recent after restart"
        )
    });
}
