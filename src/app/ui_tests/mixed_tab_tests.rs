use super::*;
use gpui_kit::test::TestEventExt;

fn fixture(home: &Path) -> State {
    let repos = repos(home, &["api"]);
    let mut state = State {
        repos: repos.clone(),
        ..Default::default()
    };
    for name in ["mixed", "other"] {
        let mut ws = Workspace {
            name: name.into(),
            branch: "main".into(),
            repos: repos
                .iter()
                .map(|r| crate::state::WsRepo {
                    name: r.name.clone(),
                    src: r.path.clone(),
                    base: "main".into(),
                    branch: None,
                })
                .collect(),
            ..Default::default()
        };
        ws.migrate();
        init_repo(&ws.dir().join("api"));
        fs::write(ws.dir().join("api/a.txt"), "alpha\n").unwrap();
        fs::write(ws.dir().join("api/b.txt"), "bravo\n").unwrap();
        ws.open_tab(TabId::File("api/a.txt".into()));
        ws.diff_closed = false;
        ws.open_tab(TabId::Diff);
        ws.open_tab(TabId::File("api/b.txt".into()));
        state.workspaces.push(ws);
    }
    state.last_open = Some(("mixed".into(), "chat".into()));
    state
}

fn element(id: &TabId) -> SharedString {
    match id {
        TabId::Chat(id) => format!("chat-tab-{id}"),
        TabId::Diff => "diff-tab".into(),
        TabId::File(path) => format!("file-tab-{path}"),
    }
    .into()
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn mixed_drag_preserves_buffers_and_navigation(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        fixture(&home),
        WindowOptions {
            app_owns_titlebar_drag: true,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1440.), px(820.)),
            ))),
            ..Default::default()
        },
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    let chat = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let empty = w.find("titlebar-empty").bounds().center();
        let tab = w.find("diff-tab").bounds().center();
        w.pointer_down(empty, MouseButton::Left, cx);
        assert_eq!(shark.read(cx).chrome_pressed.get(), Some("titlebar-empty"));
        w.pointer_up(tab, MouseButton::Left, cx);
        assert_eq!(shark.read(cx).chrome_pressed.get(), None);
        let control = w.find("new-chat").bounds().center();
        w.pointer_down(control, MouseButton::Left, cx);
        assert_eq!(shark.read(cx).chrome_pressed.get(), None);
        w.pointer_up(empty, MouseButton::Left, cx);
    })
    .unwrap();
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    cx.update_window(window, |_, w, cx| w.input("draft stays here", cx))
        .unwrap();
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update_window(window, |_, w, cx| w.input("unsaved ", cx))
        .unwrap();
    let dirty = cx.update(|cx| editor.read(cx).input.read(cx).value().to_string());
    let ids = [
        TabId::Chat("mixed".into()),
        TabId::Diff,
        TabId::File("api/a.txt".into()),
    ];
    // Every kind is both source and target, using actual pointer drags.
    for (from, to) in [(0, 1), (1, 0), (0, 2), (2, 0), (1, 2), (2, 1)] {
        let mut expected = cx.update(|cx| shark.read(cx).active_ws().unwrap().tab_order.clone());
        let a = expected.iter().position(|t| t == &ids[from]).unwrap();
        let b = expected.iter().position(|t| t == &ids[to]).unwrap();
        move_item(&mut expected, a, b);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(editor.read(cx).is_focused(w, cx), "focus before drag");
            let start = w.find(element(&ids[from])).bounds().center();
            let end = w.find(element(&ids[to])).bounds().center();
            w.pointer_down(start, MouseButton::Left, cx);
            assert!(editor.read(cx).is_focused(w, cx), "focus after press");
            w.pointer_move(start + point(px(8.), px(0.)), Some(MouseButton::Left), cx);
            // Crossing empty chrome must never arm a window drag.
            let empty = w.find("titlebar-empty").bounds().center();
            w.pointer_move(empty, Some(MouseButton::Left), cx);
            assert_eq!(shark.read(cx).chrome_pressed.get(), None);
            w.pointer_move(end, Some(MouseButton::Left), cx);
            w.pointer_up(end, MouseButton::Left, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            let s = shark.read(cx);
            assert_eq!(s.active_ws().unwrap().tab_order, expected);
            assert_eq!(s.editor.as_ref(), Some(&editor));
            assert_eq!(s.tab, Tab::File);
            assert!(editor.read(cx).is_focused(w, cx));
            assert!(editor.read(cx).dirty);
            assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), dirty);
            assert_eq!(
                chat.read(cx).composer.read(cx).value().as_ref(),
                "draft stays here"
            );
            assert_eq!(State::load().workspaces[0].tab_order, expected);
        })
        .unwrap();
    }
    let order = cx.update(|cx| shark.read(cx).active_ws().unwrap().tab_order.clone());
    for (workspace, from, to) in [
        ("other", ids[0].clone(), ids[1].clone()),
        ("mixed", ids[0].clone(), ids[0].clone()),
        ("mixed", TabId::File("api/gone".into()), ids[1].clone()),
        ("mixed", ids[0].clone(), TabId::Chat("closed".into())),
    ] {
        on(cx, &|s, _, cx| s.move_tab(workspace, &from, &to, cx));
        cx.update(|cx| assert_eq!(shark.read(cx).active_ws().unwrap().tab_order, order));
    }
    press(cx, &["cmd-z"]);
    cx.update(|cx| assert_eq!(editor.read(cx).input.read(cx).value().as_ref(), "alpha\n"));
    for (ix, id) in order.iter().enumerate() {
        press(cx, &[&format!("ctrl-{}", ix + 1)]);
        cx.update(|cx| assert_eq!(shark.read(cx).active_tab_index(cx), Some(ix), "{id:?}"));
        press(cx, &["ctrl-tab"]);
        cx.update(|cx| {
            assert_eq!(
                shark.read(cx).active_tab_index(cx),
                Some((ix + 1) % order.len())
            )
        });
    }
    on(cx, &|s, w, cx| {
        s.new_chat(w, cx);
        assert!(matches!(
            s.active_ws().unwrap().tab_order.last(),
            Some(TabId::Chat(_))
        ));
        let id = s.active_ws().unwrap().tab_order.last().unwrap().clone();
        s.move_tab("mixed", &id, &TabId::File("api/a.txt".into()), cx);
        s.cycle_chat(1, w, cx);
        assert_eq!(s.active_chat(), Some(&chat));
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn restored_files_switch_discard_retry_and_restart(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    let bad = [
        "api/missing.txt",
        "api/binary",
        "api/large",
        "api/unreadable",
        "api/outside",
        "../escape",
        "/tmp/escape",
        "unknown/a.txt",
        "api/./a.txt",
        "api//a.txt",
        "api/../api/a.txt",
    ];
    let dir = state.workspaces[0].dir();
    fs::write(dir.join("api/binary"), [0, 255]).unwrap();
    fs::File::create(dir.join("api/large"))
        .unwrap()
        .set_len(crate::editor::MAX_BYTES + 1)
        .unwrap();
    fs::write(dir.join("api/unreadable"), "secret").unwrap();
    use std::os::unix::fs::{PermissionsExt, symlink};
    fs::set_permissions(dir.join("api/unreadable"), fs::Permissions::from_mode(0)).unwrap();
    fs::write(home.join("outside.txt"), "outside").unwrap();
    symlink(home.join("outside.txt"), dir.join("api/outside")).unwrap();
    for path in bad {
        state.workspaces[0].open_tab(TabId::File(path.into()));
    }
    let saved = state.workspaces[0].tab_order.clone();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        assert!(s.editor.is_none());
        assert!(
            s.active_chat()
                .unwrap()
                .read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
        assert_eq!(s.active_ws().unwrap().tab_order, saved);
    })
    .unwrap();
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update_window(window, |_, w, cx| w.input("discard me ", cx))
        .unwrap();
    on(cx, &|s, w, cx| s.open_workspace_named("other", w, cx));
    press(cx, &["escape"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active.as_deref(), Some("mixed"));
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&editor));
        assert!(editor.read(cx).dirty);
        assert_eq!(shark.read(cx).active_ws().unwrap().tab_order, saved);
    });
    on(cx, &|s, w, cx| s.open_workspace_named("other", w, cx));
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("other") && shark.read(cx).file_tabs.len() == 2
    })
    .await;
    fs::write(dir.join("api/missing.txt"), "now available\n").unwrap();
    on(cx, &|s, w, cx| s.open_workspace_named("mixed", w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 3
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active_ws().unwrap().tab_order, saved);
        let a = &s.file_tabs[0].editor;
        assert_ne!(a, &editor);
        assert_eq!(a.read(cx).input.read(cx).value().as_ref(), "alpha\n");
        assert!(!a.read(cx).dirty);
    });
    // A -> B -> A before any background read lands: generation, not just name, guards it.
    on(cx, &|s, w, cx| {
        s.open_workspace_named("other", w, cx);
        s.open_workspace_named("mixed", w, cx);
        s.open_workspace_named("other", w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("other"));
        assert_eq!(s.file_tabs.len(), 2);
    });
    on(cx, &|s, w, cx| s.open_workspace_named("mixed", w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 3
    })
    .await;
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    cx.update_window(window, |_, w, cx| w.input("saved ", cx))
        .unwrap();
    press(cx, &["cmd-s"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !shark.read(cx).editor.as_ref().unwrap().read(cx).dirty
    })
    .await;
    let (again_window, again) = open(cx, WindowOptions::default());
    cx.wait_for(again_window, Duration::from_secs(10), |_, cx| {
        again.read(cx).file_tabs.len() == 3
    })
    .await;
    cx.update(|cx| {
        let s = again.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        assert_eq!(s.active_ws().unwrap().tab_order, saved);
        assert_eq!(
            s.file_tabs[0]
                .editor
                .read(cx)
                .input
                .read(cx)
                .value()
                .as_ref(),
            "saved alpha\n"
        );
    });
    // Replacement retains the slot; explicit close removes it, and reopening appends.
    fs::write(dir.join("api/c.txt"), "charlie").unwrap();
    let again_on = on_app(again_window, again.clone());
    again_on(cx, &|s, w, cx| {
        s.select_file(0, w, cx);
        s.open_path("api/c.txt".into(), w, cx);
        assert_eq!(
            s.active_ws().unwrap().tab_order[1],
            TabId::File("api/c.txt".into())
        );
        s.close_file(w, cx);
        assert!(
            !s.active_ws()
                .unwrap()
                .tab_order
                .contains(&TabId::File("api/c.txt".into()))
        );
        s.open_path_tab("api/c.txt".into(), w, cx);
        assert_eq!(
            s.active_ws().unwrap().tab_order.last(),
            Some(&TabId::File("api/c.txt".into()))
        );
    });
    fs::set_permissions(
        dir.join("api/unreadable"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pending_restore_survives_closing_last_live_editor(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.open_workspace_named("other", w, cx);
        s.open_path_tab("api/a.txt".into(), w, cx);
        assert_eq!(s.file_tabs.len(), 1);
        s.close_file(w, cx);
        assert!(s.file_tabs.is_empty());
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 1
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.file_tabs[0].editor.read(cx).path, "api/b.txt");
        assert!(
            !s.active_ws()
                .unwrap()
                .tab_order
                .contains(&TabId::File("api/a.txt".into()))
        );
        assert!(
            s.ws("mixed")
                .unwrap()
                .tab_order
                .contains(&TabId::File("api/a.txt".into()))
        );
    });
}

/// A background restore can finish while the replacement discard dialog is open.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_restored_target_during_discard_stays_unique(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    state.workspaces[0].close_tab(&TabId::File("api/b.txt".into()));
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 1
    })
    .await;
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    cx.update_window(window, |_, w, cx| w.input("unsaved ", cx))
        .unwrap();
    let source = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    let source_text = cx.update(|cx| source.read(cx).input.read(cx).value().to_string());
    for discard in [false, true] {
        on(cx, &|s, w, cx| {
            assert!(source.read(cx).dirty);
            // Resume a saved tab restore just before replacing the dirty tab.
            s.active_ws_mut()
                .unwrap()
                .open_tab(TabId::File("api/b.txt".into()));
            s.restore_file_tabs(w, cx);
            s.open_path("api/b.txt".into(), w, cx);
            assert!(w.has_active_dialog(cx));
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).file_tabs.len() == 2
        })
        .await;
        let target = cx.update(|cx| {
            let target = shark
                .read(cx)
                .file_tabs
                .iter()
                .find(|t| t.editor.read(cx).path == "api/b.txt")
                .unwrap()
                .editor
                .clone();
            target
                .read(cx)
                .input
                .clone()
                .update(cx, |i, cx| i.set_selected_range(1..3, cx));
            target
        });
        press_in(window)(cx, &[if discard { "enter" } else { "escape" }]);
        cx.update_window(window, |_, w, cx| {
            let s = shark.read(cx);
            assert_eq!(
                s.file_tabs
                    .iter()
                    .filter(|t| t.editor.read(cx).path == "api/b.txt")
                    .count(),
                1
            );
            assert!(s.file_tabs.iter().any(|t| t.editor == target));
            assert_eq!(target.read(cx).input.read(cx).selected_range(), 1..3);
            assert_eq!(target.read(cx).input.read(cx).value().as_ref(), "bravo\n");
            if discard {
                assert_eq!(s.editor.as_ref(), Some(&target));
                assert!(!s.file_tabs.iter().any(|t| t.editor == source));
                assert!(
                    !s.active_ws()
                        .unwrap()
                        .tab_order
                        .contains(&TabId::File("api/a.txt".into()))
                );
                assert_eq!(
                    s.active_ws().unwrap().tab_order[1],
                    TabId::File("api/b.txt".into())
                );
                assert_eq!(
                    State::load().workspaces[0].tab_order,
                    s.active_ws().unwrap().tab_order
                );
            } else {
                assert_eq!(s.editor.as_ref(), Some(&source));
                assert!(source.read(cx).dirty);
                assert_eq!(source.read(cx).input.read(cx).value().as_ref(), source_text);
            }
            assert_eq!(
                fs::read_to_string(s.active_ws().unwrap().dir().join("api/a.txt")).unwrap(),
                "alpha\n"
            );
            w.render_frame(cx);
        })
        .unwrap();
        if !discard {
            on(cx, &|s, w, cx| s.close_editor(target.clone(), w, cx));
        }
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_tab_scale_and_restore_entity_lifetime(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    for count in [10, 100, 300] {
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                let ws = s.active_ws_mut().unwrap();
                ws.chats.truncate(1);
                ws.chats.extend((1..count).map(|i| crate::state::ChatMeta {
                    id: format!("mixed~scale-{i}"),
                    title: format!("Conversation {i}: a realistically sized review title"),
                    ..Default::default()
                }));
                let start = Instant::now();
                ws.normalize_tabs();
                eprintln!("review tabs={count}: normalize={:?}", start.elapsed());
                cx.notify();
            });
            w.render_frame(cx);
            let start = Instant::now();
            for _ in 0..3 {
                shark.update(cx, |_, cx| cx.notify());
                w.render_frame(cx);
            }
            eprintln!("review tabs={count}: mean frame={:?}", start.elapsed() / 3);
        })
        .unwrap();
    }
    let old = cx.update(|cx| {
        shark
            .read(cx)
            .file_tabs
            .iter()
            .map(|t| t.editor.downgrade())
            .collect::<Vec<_>>()
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_workspace_named("other", w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    assert!(
        old.iter().all(|editor| editor.upgrade().is_none()),
        "restored editors leaked after workspace exit"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_restore_large_file_batch_releases_buffers(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    let text = "A moderately sized line in a persisted file tab.\n".repeat(5_500);
    let ws = &mut state.workspaces[0];
    for i in 0..24 {
        let path = format!("api/stress-{i}.txt");
        fs::write(ws.dir().join(&path), &text).unwrap();
        ws.open_tab(TabId::File(path));
    }
    let start = Instant::now();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(30), |_, cx| {
        shark.read(cx).file_tabs.len() == 26
    })
    .await;
    eprintln!(
        "review restored 26 files / {} extra text bytes in {:?}",
        text.len() * 24,
        start.elapsed()
    );
    let old = cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.file_tabs.iter().all(|tab| !tab.editor.read(cx).dirty));
        s.file_tabs
            .iter()
            .map(|t| t.editor.downgrade())
            .collect::<Vec<_>>()
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_workspace_named("other", w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).file_tabs.len() == 2
    })
    .await;
    assert!(old.iter().all(|editor| editor.upgrade().is_none()));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_file_tab_scale(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    let ws = &mut state.workspaces[0];
    for i in 2..300 {
        let path = format!("api/tab-{i}.txt");
        fs::write(ws.dir().join(&path), "small text\n").unwrap();
        ws.open_tab(TabId::File(path));
    }
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(30), |_, cx| {
        shark.read(cx).file_tabs.len() == 300
    })
    .await;
    for count in [300, 100, 10] {
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.file_tabs.truncate(count);
                let paths: Vec<_> = s
                    .file_tabs
                    .iter()
                    .map(|t| t.editor.read(cx).path.clone())
                    .collect();
                s.active_ws_mut()
                    .unwrap()
                    .tab_order
                    .retain(|tab| match tab {
                        TabId::File(path) => paths.contains(path),
                        _ => true,
                    });
                assert_eq!(s.file_tabs.len(), count);
                cx.notify();
            });
            w.render_frame(cx);
            let start = Instant::now();
            for _ in 0..3 {
                shark.update(cx, |_, cx| cx.notify());
                w.render_frame(cx);
            }
            eprintln!(
                "review file tabs={count}: mean frame={:?}",
                start.elapsed() / 3
            );
        })
        .unwrap();
    }
}
