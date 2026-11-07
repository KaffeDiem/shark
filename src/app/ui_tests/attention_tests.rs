use super::*;
use gpui_kit::test::TestEventExt;

fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    test_home();
    cx.update(|cx| cx.set_app_identity("io.github.kaffediem.shark", "Shark"));
    boot(
        cx,
        State {
            workspaces: ["one", "two"]
                .map(|name| Workspace {
                    name: name.into(),
                    branch: name.into(),
                    chats: ["first", "second"]
                        .map(|id| ChatMeta {
                            id: format!("{name}-{id}"),
                            ..Default::default()
                        })
                        .to_vec(),
                    ..Default::default()
                })
                .to_vec(),
            ..Default::default()
        },
        WindowOptions::default(),
    )
}

fn click(cx: &mut TestAppContext, tag: SharedString) {
    cx.simulate_system_notification_response(SystemNotificationResponse {
        tag,
        action_id: None,
    });
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn notification_opens_finished_chat(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.open_workspace(0, w, cx));
    let chat = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    let done = |cx: &mut TestAppContext| {
        cx.update(|cx| chat.update(cx, |_, cx| cx.emit(crate::chat::ChatEvent::TurnDone)));
        cx.run_until_parked();
    };
    done(cx);
    assert!(
        cx.shown_system_notifications().is_empty(),
        "visible chat stays quiet"
    );
    on(cx, &|s, w, cx| s.select_chat(1, w, cx));
    done(cx);
    on(cx, &|s, w, cx| s.show(Tab::Feature(Feature::Diff), w, cx));
    done(cx);
    assert!(
        cx.shown_system_notifications().is_empty(),
        "other tabs in the visible workspace keep the existing notification policy"
    );

    for view in ["other chat", "other agent", "home", "diff", "inactive"] {
        on(cx, &|s, w, cx| s.open_workspace(1, w, cx));
        let before = cx.shown_system_notifications().len();
        done(cx);
        let notifications = cx.shown_system_notifications();
        assert_eq!(notifications.len(), before + 1, "{view}");
        let notification = notifications.last().unwrap();
        assert_eq!(notification.body.as_ref(), "The agent finished its turn");
        // Identity, not the tab's former index, must survive until the click.
        on(cx, &|s, w, cx| {
            assert!(s.state.workspaces[0].move_tab(
                &TabId::Chat("one-first".into()),
                &TabId::Chat("one-second".into()),
            ));
            if view != "other agent" {
                s.open_workspace(0, w, cx);
                let second = s
                    .active_ws()
                    .unwrap()
                    .chats
                    .iter()
                    .position(|c| c.id == "one-second")
                    .unwrap();
                s.select_chat(second, w, cx);
                match view {
                    "home" => s.enter_home(w, cx),
                    "diff" => s.show(Tab::Feature(Feature::Diff), w, cx),
                    _ => {}
                }
            }
        });
        if view == "inactive" {
            VisualTestContext::from_window(window, cx).deactivate_window();
            on(cx, &|s, w, _| {
                assert!(!w.is_window_active());
                assert!(!s.attn.window_active);
            });
        }
        click(cx, notification.tag.clone());
        on(cx, &|s, w, cx| {
            assert_eq!(s.active.as_deref(), Some("one"), "{view}");
            assert!(!s.home_shown(), "{view}");
            assert_eq!(s.tab, Tab::Chat, "{view}");
            assert_eq!(s.active_chat(), Some(&chat), "{view}");
            assert!(w.is_window_active(), "{view}");
            assert!(!s.unread.contains("one-first"));
            assert!(
                chat.read(cx)
                    .composer
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(w)
            );
        });
        assert!(cx.delivered_system_notifications().is_empty());
    }
}

#[gpui_kit::test]
async fn notification_keeps_other_chats_unread(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    for home in [false, true] {
        use std::sync::atomic::Ordering::SeqCst;

        on(cx, &|s, w, cx| {
            s.open_workspace(0, w, cx);
            s.select_chat(0, w, cx);
            if home {
                s.enter_home(w, cx);
            } else {
                s.open_workspace(1, w, cx);
            }
            s.unread.extend(["one-first".into(), "one-second".into()]);
            s.turn_done("one", "one-second", cx);
        });
        let tag = cx.shown_system_notifications().last().unwrap().tag.clone();
        click(cx, tag);
        on(cx, &|s, _, cx| {
            assert_eq!(s.active.as_deref(), Some("one"));
            assert!(!s.home_shown());
            assert_eq!(s.tab, Tab::Chat);
            assert_eq!(s.active_chat().unwrap().read(cx).meta.id, "one-second");
            assert!(!s.unread.contains("one-second"));
            assert!(
                s.unread.contains("one-first"),
                "clicking another chat must not read the previous selection; Home={home}"
            );
        });
        assert_eq!(crate::attention::BADGE.load(SeqCst), 1);
        on(cx, &|s, w, cx| {
            if home {
                s.enter_home(w, cx);
            } else {
                s.open_workspace(1, w, cx);
            }
            s.turn_done("one", "one-first", cx);
        });
        let tag = cx.shown_system_notifications().last().unwrap().tag.clone();
        click(cx, tag);
        assert_eq!(crate::attention::BADGE.load(SeqCst), 0, "Home={home}");
    }
}

#[gpui_kit::test]
async fn notification_stale_targets_are_safe(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.open_workspace(1, w, cx);
        s.turn_done("one", "one-first", cx);
    });
    let tag = cx.shown_system_notifications().last().unwrap().tag.clone();
    on(cx, &|s, w, cx| {
        s.state.workspaces[0].chats[0].session_id = Some("finished".into());
        s.open_workspace(0, w, cx);
        s.close_chat(w, cx);
        assert_eq!(s.active_ws().unwrap().closed[0].id, "one-first");
        s.open_workspace(1, w, cx);
    });
    click(cx, tag.clone());
    on(cx, &|s, w, cx| {
        assert_eq!(s.active.as_deref(), Some("one"));
        assert!(!s.home_shown());
        assert_eq!(s.tab, Tab::Chat);
        assert_eq!(s.active_chat().unwrap().read(cx).meta.id, "one-first");
        s.open_workspace(1, w, cx);
        s.archiving.insert("one".into());
    });
    click(cx, tag.clone());
    on(cx, &|s, _, _| {
        assert_eq!(s.active.as_deref(), Some("two"));
        s.state.workspaces.remove(0);
        s.archiving.clear();
    });
    click(cx, tag);
    click(cx, "invalid target".into());
    on(cx, &|s, _, _| assert_eq!(s.active.as_deref(), Some("two")));
}

#[gpui_kit::test]
async fn notification_for_evicted_chat_keeps_current_view(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.state.workspaces[0].chats[0].session_id = Some("finished".into());
        s.open_workspace(0, w, cx);
        s.close_chat(w, cx);
        assert_eq!(s.active_ws().unwrap().closed[0].id, "one-first");
        s.open_workspace(1, w, cx);
        s.turn_done("one", "one-first", cx);
        s.state.workspaces[0].closed.clear(); // Its retained history has since been evicted.
    });
    let tag = cx.shown_system_notifications().last().unwrap().tag.clone();
    for home in [false, true] {
        on(cx, &|s, w, cx| {
            if home {
                s.open_workspace(0, w, cx);
                s.enter_home(w, cx);
            }
            s.unread.insert("one-second".into());
        });
        let before = cx
            .update_window(window, |_, w, cx| w.notifications(cx).len())
            .unwrap();
        click(cx, tag.clone());
        on(cx, &|s, w, cx| {
            assert_eq!(s.active.as_deref(), Some(if home { "one" } else { "two" }));
            assert_eq!(s.home_shown(), home);
            assert!(s.unread.contains("one-second"));
            assert_eq!(
                w.notifications(cx).len(),
                before + 1,
                "unavailable-chat message"
            );
        });
    }
    on(cx, &|s, w, cx| {
        s.open_notification("one", None, w, cx);
        assert!(!s.home_shown());
        assert_eq!(s.active_chat().unwrap().read(cx).meta.id, "one-second");
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn notification_respects_unsaved_edits(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = fixture(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.open_workspace(1, w, cx);
        let path = s.active_ws().unwrap().dir().join("edit.txt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "original").unwrap();
        s.open_path_tab("edit.txt".into(), w, cx);
        s.turn_done("one", "one-second", cx);
    });
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    until(cx, window, |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "original"
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("changed ", cx))
        .unwrap();
    cx.run_until_parked();
    let tag = cx.shown_system_notifications().last().unwrap().tag.clone();
    click(cx, tag.clone());
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press_in(window)(cx, &["escape"]);
    on(cx, &|s, _, cx| {
        assert_eq!(s.active.as_deref(), Some("two"));
        assert!(editor.read(cx).dirty);
    });
    click(cx, tag);
    press_in(window)(cx, &["enter"]);
    on(cx, &|s, _, cx| {
        assert_eq!(s.active.as_deref(), Some("one"));
        assert!(!s.home_shown());
        assert_eq!(s.tab, Tab::Chat);
        assert_eq!(s.active_chat().unwrap().read(cx).meta.id, "one-second");
    });
}
