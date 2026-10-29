use super::*;
use gpui_kit::test::TestEventExt;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_and_reopened_chats_append(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "append-chats".into(),
            git::on_branch(&repos, "append-chats"),
            vec![],
            None,
            w,
            cx,
        );
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    press(cx, &["cmd-t", "cmd-t"]);
    on(cx, &|s, _, _cx| {
        s.active_ws_mut().unwrap().closed = ["picked", "retained", "latest"]
            .map(|id| s.state.new_chat(id.into()))
            .to_vec();
    });
    for (key, restored) in [
        (Some("cmd-t"), None),
        (Some("cmd-t"), None),
        (Some("cmd-shift-t"), Some("latest")),
        (None, Some("picked")),
    ] {
        press(cx, &["ctrl-1"]);
        let before = cx.update(|cx| shark.read(cx).active_ws().unwrap().chats.clone());
        if let Some(key) = key {
            press(cx, &[key]);
        } else {
            on(cx, &|s, w, cx| s.reopen_chat(Some(0), w, cx));
        }
        cx.update_window(window, |_, w, cx| {
            let s = shark.read(cx);
            let ws = s.active_ws().unwrap();
            assert_eq!(ws.chats.len(), before.len() + 1);
            assert_eq!(&ws.chats[..before.len()], before.as_slice());
            assert_eq!(ws.active_chat, before.len());
            if let Some(id) = restored {
                assert_eq!(ws.chats.last().unwrap().id, id);
                assert!(!ws.closed.iter().any(|chat| chat.id == id));
            }
            assert_eq!(s.tab, Tab::Chat);
            assert!(
                s.active_chat()
                    .unwrap()
                    .read(cx)
                    .composer
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(w)
            );
            let saved = State::load();
            assert_eq!(saved.workspaces[0].chats, ws.chats);
            assert_eq!(saved.workspaces[0].active_chat, ws.active_chat);
        })
        .unwrap();
    }
    cx.update(|cx| {
        let s = shark.read(cx);
        let closed = &s.active_ws().unwrap().closed;
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id, "retained");
    });
    fs::remove_dir_all(&home).ok();
}

/// File focus and chat identity survive every way of selecting or closing a chat.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn file_chat_focus_and_hover(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "tabs-focus".into(),
            git::on_branch(&repos, "tabs-focus"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    fs::write(home.join("shark/workspaces/tabs-focus/api/a.txt"), "text\n").unwrap();
    on(cx, &|s, w, cx| {
        s.new_chat(w, cx);
        s.show(Tab::Feature(Feature::Diff), w, cx);
        s.open_path("api/a.txt".into(), w, cx);
    });
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "text\n"
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("unsaved ", cx))
        .unwrap();
    let ids = cx.update(|cx| {
        shark
            .read(cx)
            .active_ws()
            .unwrap()
            .chats
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
    });
    let chat = cx.update(|cx| shark.read(cx).chats[&ids[1]].clone());
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    for mode in [crate::state::Mode::Dark, crate::state::Mode::Light] {
        for width in [800., 1440.] {
            on(cx, &|s, w, cx| {
                crate::settings::apply(
                    &crate::state::Appearance {
                        mode,
                        ..Default::default()
                    },
                    cx,
                );
                w.resize(size(px(width), px(700.)));
                s.differ.update(cx, |d, _| *d.show_tree_mut() = false);
                s.select_file(0, w, cx);
            });
            cx.update_window(window, |_, w, _| w.activate_window())
                .unwrap();
            cx.run_until_parked();
            for active in [true, false] {
                if !active {
                    VisualTestContext::from_window(window, cx).deactivate_window();
                }
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    let pane = w.find("file-editor-pane").bounds();
                    let island = w.find("content-island").bounds();
                    assert!(
                        (pane.size.width - island.size.width).abs() <= px(2.),
                        "editor fills central width"
                    );
                    assert!(!shark.read(cx).panes(cx).contains(&Pane::Chat(0)));
                    assert_eq!(w.is_window_active(), active);
                    // Hover each tab; only its close control appears.
                    for (ix, (tab, close)) in [
                        (format!("chat-tab-{}", ids[0]), "close-chat"),
                        (format!("chat-tab-{}", ids[1]), "close-chat"),
                        ("diff-tab".into(), "close-diff"),
                        ("file-tab-api/a.txt".into(), "close-file-tab"),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        // Bring each tab into view before checking its hover controls.
                        shark.read(cx).tab_scroll[0].scroll_to_item(ix);
                        w.refresh();
                        w.render_frame(cx);
                        w.pointer_move(point(px(0.), px(690.)), None, cx);
                        let before = w.within(SharedString::from(tab.clone())).find(close);
                        assert!(!before.visible(), "hidden: {tab}, active={active}");
                        let bounds = w.find(SharedString::from(tab.clone())).bounds();
                        let label = w
                            .within(SharedString::from(tab.clone()))
                            .find("tab-label")
                            .bounds();
                        assert!(
                            label.right() > before.bounds().left(),
                            "hidden close button leaves its space available to the label: {tab}"
                        );
                        w.pointer_move(bounds.center(), None, cx);
                        let after = w.within(SharedString::from(tab.clone())).find(close);
                        assert!(after.visible(), "hovered: {tab}, active={active}");
                        assert_eq!(before.bounds(), after.bounds());
                        assert_eq!(bounds, w.find(SharedString::from(tab)).bounds());
                        assert_eq!(
                            w.find_all("close-chat")
                                .into_iter()
                                .filter(|b| b.visible())
                                .count(),
                            usize::from(ix < 2)
                        );
                    }
                    assert!(
                        chat.read(cx).working(),
                        "file layout keeps the agent running"
                    );
                })
                .unwrap();
            }
        }
    }
    cx.update_window(window, |_, w, _| w.activate_window())
        .unwrap();
    // Cancellation consumes the first Escape; the next leaves for the visible sidebar.
    on(cx, &|s, w, cx| {
        s.set_vim(false, cx);
        s.select_file(0, w, cx);
        editor
            .read(cx)
            .input
            .clone()
            .update(cx, |i, cx| i.add_selection(2..2, cx));
    });
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(editor.read(cx).is_focused(w, cx))
    })
    .unwrap();
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
        assert_eq!(shark.read(cx).tab, Tab::File);
    })
    .unwrap();
    press(cx, &["cmd-alt-right"]);
    cx.update_window(window, |_, w, cx| {
        assert!(editor.read(cx).is_focused(w, cx))
    })
    .unwrap();
    on(cx, &|s, w, cx| s.toggle_tree(w, cx));
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
        assert!(!shark.read(cx).tree_shown(cx));
    })
    .unwrap();
    // Chat selection, creation, cycling and reopening always reveal and focus the composer.
    for key in ["ctrl-1", "cmd-t", "cmd-shift-]", "cmd-shift-["] {
        on(cx, &|s, w, cx| s.select_file(0, w, cx));
        press(cx, &[key]);
        cx.update_window(window, |_, w, cx| {
            let s = shark.read(cx);
            assert_eq!(s.tab, Tab::Chat, "{key}");
            assert!(
                s.active_chat()
                    .unwrap()
                    .read(cx)
                    .composer
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(w)
            );
            assert_eq!(
                editor.read(cx).input.read(cx).value().as_ref(),
                "unsaved text\n"
            );
        })
        .unwrap();
    }
    // Close an earlier background chat by clicking × without selecting it or moving focus.
    on(cx, &|s, w, cx| {
        s.select_chat(s.active_ws().unwrap().chats.len() - 1, w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let focus = w.focused(cx);
        let selected = shark.read(cx).active_chat().cloned();
        let tab = SharedString::from(format!("chat-tab-{}", ids[0]));
        w.pointer_move(w.find(tab.clone()).bounds().center(), None, cx);
        w.within(tab).click("close-chat", cx);
        assert_eq!(shark.read(cx).active_chat(), selected.as_ref());
        assert_eq!(w.focused(cx), focus);
    })
    .unwrap();
    // A running chat's confirmation remains tied to its ID even when reordered.
    on(cx, &|s, w, cx| {
        s.select_file(0, w, cx);
    });
    let running_tab = SharedString::from(format!("chat-tab-{}", ids[1]));
    let click_running = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.pointer_move(w.find(running_tab.clone()).bounds().center(), None, cx);
            w.within(running_tab.clone()).click("close-chat", cx);
            assert!(w.has_active_dialog(cx));
            assert_eq!(shark.read(cx).tab, Tab::File);
        })
        .unwrap();
        cx.run_until_parked();
    };
    click_running(cx);
    press(cx, &["escape"]);
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    cx.update(|cx| assert!(shark.read(cx).chats.contains_key(&ids[1])));
    click_running(cx);
    on(cx, &|s, _, cx| {
        let target = s.active_ws().unwrap().chats[0].id.clone();
        s.move_tab(
            "tabs-focus",
            &TabId::Chat(ids[1].clone()),
            &TabId::Chat(target),
            cx,
        );
        chat.update(cx, |c, _| c.meta.session_id = Some("recover-me".into()));
        s.active_ws_mut()
            .unwrap()
            .chats
            .iter_mut()
            .find(|c| c.id == ids[1])
            .unwrap()
            .session_id = Some("recover-me".into());
    });
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!shark.read(cx).chats.contains_key(&ids[1]));
        assert_eq!(shark.read(cx).tab, Tab::File);
        assert!(editor.read(cx).is_focused(w, cx));
    })
    .unwrap();
    press(cx, &["cmd-shift-t"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        assert_eq!(s.active_chat().unwrap().read(cx).meta.id, ids[1]);
        assert!(
            s.active_chat()
                .unwrap()
                .read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
    })
    .unwrap();
    // Closing all chats still leaves one fresh chat and never closes retained files.
    on(cx, &|s, w, cx| {
        while s.active_ws().unwrap().chats.len() > 1 {
            s.close_chat(w, cx);
        }
        s.close_chat(w, cx);
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active_ws().unwrap().chats.len(), 1);
        assert!(s.active_chat().unwrap().read(cx).meta.session_id.is_none());
        assert!(editor.read(cx).dirty);
    });
    fs::remove_dir_all(&home).ok();
}

/// Diff closure survives reloads and never leaks into fallback or visible-tab indexing.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn diff_closure_navigation_and_discard(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "closed-diff".into(),
            git::on_branch(&repos, "closed-diff"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    fs::write(
        home.join("shark/workspaces/closed-diff/api/a.txt"),
        "text\n",
    )
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(shark.read(cx).active_ws().unwrap().diff_closed);
        assert!(w.find_all("diff-tab").is_empty());
    })
    .unwrap();
    press(cx, &["ctrl-tab", "ctrl-2"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    press(cx, &["cmd-shift-d"]);
    on(cx, &|s, w, cx| s.open_path("api/a.txt".into(), w, cx));
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "text\n"
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_move(w.find("diff-tab").bounds().center(), None, cx);
        w.click("close-diff", cx);
        assert_eq!(shark.read(cx).tab, Tab::File);
        assert!(editor.read(cx).is_focused(w, cx));
    })
    .unwrap();
    assert!(State::load().workspaces[0].diff_closed);
    for (key, tab) in [
        ("ctrl-1", Tab::Chat),
        ("ctrl-2", Tab::File),
        ("ctrl-tab", Tab::Chat),
        ("ctrl-shift-tab", Tab::File),
        ("ctrl-3", Tab::File),
    ] {
        press(cx, &[key]);
        cx.update(|cx| assert_eq!(shark.read(cx).tab, tab, "{key}"));
    }
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(
            Modifiers {
                control: true,
                ..Default::default()
            },
            cx,
        );
        w.render_frame(cx);
        assert!(w.find_all("diff-tab").is_empty());
    })
    .unwrap();
    {
        let mut visual = VisualTestContext::from_window(window, cx);
        assert!(visual.debug_bounds("kbd:ctrl-2").is_some());
        assert!(visual.debug_bounds("kbd:ctrl-3").is_none());
    }
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(Modifiers::default(), cx)
    })
    .unwrap();
    // Differ reopens the same artifact with either internal tree preference.
    for tree in [false, true] {
        on(cx, &|s, _, cx| {
            s.differ.update(cx, |d, _| *d.show_tree_mut() = tree)
        });
        // Open, click again, then refocus the existing tab from Chat.
        for from_chat in [false, false, true] {
            if from_chat {
                on(cx, &|s, w, cx| s.select_chat(0, w, cx));
            }
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                w.click("feature-differ", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let s = shark.read(cx);
                assert_eq!(s.tab, Tab::Feature(Feature::Diff));
                assert_eq!(w.find_all("diff-tab").len(), 1);
                assert!(
                    s.differ
                        .read(cx)
                        .diff()
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(w)
                );
            })
            .unwrap();
        }
        assert!(!State::load().workspaces[0].diff_closed);
        press(cx, &["cmd-w"]);
        cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
        assert!(State::load().workspaces[0].diff_closed);
    }
    // Last-file fallback respects closure, and does not reopen Diff as a side effect.
    on(cx, &|s, w, cx| s.select_file(0, w, cx));
    press(cx, &["cmd-w"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).tab, Tab::Chat);
        assert!(shark.read(cx).file_tabs.is_empty());
    });
    assert!(State::load().workspaces[0].diff_closed);
    press(cx, &["cmd-shift-d"]);
    assert!(!State::load().workspaces[0].diff_closed);
    on(cx, &|s, w, cx| s.open_path("api/a.txt".into(), w, cx));
    press(cx, &["cmd-w"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Feature(Feature::Diff)));
    // Inline edits cannot be lost through × or Cmd-W; cancel preserves both edit and tab.
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !shark.read(cx).differ.read(cx).diff_files().is_empty()
    })
    .await;
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/a.txt".into()), w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).editing().is_some()
    })
    .await;
    on(cx, &|s, _, cx| {
        s.differ
            .update(cx, |d, _| d.editing_mut().as_mut().unwrap().dirty = true)
    });
    press(cx, &["cmd-w"]);
    press(cx, &["escape"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Diff));
        assert!(s.differ.read(cx).editing().as_ref().unwrap().dirty);
        assert!(!s.active_ws().unwrap().diff_closed);
    });
    // Background inline-edit discard restores Chat's existing focus.
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_move(w.find("diff-tab").bounds().center(), None, cx);
        w.click("close-diff", cx);
        assert!(w.has_active_dialog(cx));
    })
    .unwrap();
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        assert!(s.differ.read(cx).editing().is_none());
        assert!(s.active_ws().unwrap().diff_closed);
        assert!(
            s.active_chat()
                .unwrap()
                .read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
    })
    .unwrap();
    // Workspace switches retain each workspace's flag.
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "open-diff".into(),
            git::on_branch(&repos, "open-diff"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("open-diff")
    })
    .await;
    on(cx, &|s, w, cx| {
        s.show(Tab::Feature(Feature::Diff), w, cx);
        assert!(!s.active_ws().unwrap().diff_closed);
        let ix = s
            .state
            .workspaces
            .iter()
            .position(|ws| ws.name == "closed-diff")
            .unwrap();
        s.open_workspace(ix, w, cx);
        assert!(s.active_ws().unwrap().diff_closed);
    });
    // Even a stale last-open Diff entry cannot reopen a deliberately closed tab.
    on(cx, &|s, _, _cx| {
        s.state.last_open = Some(("closed-diff".into(), "diff".into()));
        let _ = s.state.save();
    });
    let (restored_window, restored) = open(cx, WindowOptions::default());
    cx.run_until_parked();
    cx.update_window(restored_window, |_, w, cx| {
        w.render_frame(cx);
        let s = restored.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        assert!(s.active_ws().unwrap().diff_closed);
        assert!(s.file_tabs.is_empty());
        assert!(w.find_all("diff-tab").is_empty());
    })
    .unwrap();
    fs::remove_dir_all(&home).ok();
}
