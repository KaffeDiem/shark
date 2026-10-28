use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn qa_file_menu_comment_focuses_the_comment_input(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_path("api/shared.rs".into(), w, cx);
    });
    let action = shark.update(cx, |s, cx| {
        s.file_menu("api/shared.rs", cx)
            .into_iter()
            .find_map(|entry| match entry {
                Entry::Item {
                    label, on_click, ..
                } if label == "Comment" => Some(on_click),
                _ => None,
            })
            .unwrap()
    });
    cx.update_window(window, |_, w, cx| action(w, cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Diff));
        let composer = s.differ.read(cx).review().composer.as_ref().unwrap();
        assert!(
            composer
                .input
                .read(cx)
                .focus_handle(cx)
                .contains_focused(w, cx),
            "Comment menu should leave focus in the new composer"
        );
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.input("Review this change", cx);
        let composer = shark
            .read(cx)
            .differ
            .read(cx)
            .review()
            .composer
            .as_ref()
            .unwrap();
        assert_eq!(composer.input.read(cx).value(), "Review this change");
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| {
            d.begin_edit("api/shared.rs".into(), w, cx);
            assert!(d.review().composer.is_some());
            d.activate(w, cx);
            assert!(d.editing().as_ref().unwrap().focused(w, cx));
            d.comment_in("api/shared.rs", w, cx);
            d.activate(w, cx);
            let composer = d.review().composer.as_ref().unwrap();
            assert!(
                composer
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(w, cx)
            );
        });
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn qa_discard_pending_does_not_delete_another_workspaces_comments(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| {
            for workspace in ["boundary-a", "boundary-b"] {
                d.review_mut().edit(workspace, |comments| {
                    comments.push(
                        serde_json::from_value(serde_json::json!({
                            "id": 1, "path": "api/shared.rs", "start": [false, 1],
                            "end": [false, 1], "text": workspace
                        }))
                        .unwrap(),
                    );
                });
            }
            d.discard_pending(w, cx);
        });
        assert!(w.has_active_dialog(cx));
        s.open_workspace_now(1, w, cx);
    });
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        let d = shark.read(cx).differ.read(cx);
        assert_eq!(d.workspace_name(), Some("boundary-b"));
        assert_eq!(
            d.ws_comments().len(),
            1,
            "A's confirmation must preserve B's comments"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn qa_viewed_marks_survive_a_host_snapshot_before_event_delivery(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.differ
            .update(cx, |d, cx| d.toggle_viewed_path("api/shared.rs", cx));
        assert!(s.active_ws().unwrap().viewed.is_empty());
        s.configure_differ(cx);
        assert!(s.differ.read(cx).viewed_now().contains("api/shared.rs"));
    });
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .active_ws()
                .unwrap()
                .viewed
                .contains_key("api/shared.rs")
        );
    });
}

fn fixture(home: &Path) -> State {
    let repos = repos(home, &["api"]);
    let mut state = State {
        repos: repos.clone(),
        ..Default::default()
    };
    for name in ["boundary-a", "boundary-b"] {
        let mut ws = Workspace {
            name: name.into(),
            branch: "main".into(),
            repos: vec![crate::state::WsRepo {
                name: "api".into(),
                src: repos[0].path.clone(),
                base: "main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        ws.migrate();
        init_repo(&ws.dir().join("api"));
        fs::write(ws.dir().join("api/shared.rs"), format!("{name}\n")).unwrap();
        state.workspaces.push(ws);
    }
    state.last_open = Some(("boundary-a".into(), "diff".into()));
    state
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stale_refresh_cannot_replace_newer_results_or_another_workspace(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (a, b) = (state.workspaces[0].clone(), state.workspaces[1].clone());
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let differ = s.differ.clone();
            let old = differ.read(cx).diff_gen();
            differ.update(cx, |d, cx| {
                *d.diff_gen_mut() += 1;
                *d.diff_loaded_mut() = false;
                d.complete_refresh(&a.name, old, DiffData::load(&a, false), cx);
                assert!(
                    !d.diff_loaded(),
                    "older generation must not install a result"
                );
                d.complete_refresh(&a.name, d.diff_gen(), DiffData::load(&a, false), cx);
                assert!(d.diff_loaded());
            });
            s.open_workspace_now(1, w, cx);
            assert_eq!(s.differ, differ, "Differ is retained across workspaces");
            differ.update(cx, |d, cx| {
                // Even matching generation cannot admit another workspace's identical relative path.
                d.complete_refresh(&a.name, d.diff_gen(), DiffData::load(&a, false), cx);
                assert!(!d.diff_loaded());
                d.complete_refresh(&b.name, d.diff_gen(), DiffData::load(&b, false), cx);
                assert!(d.diff_loaded());
                assert_eq!(d.workspace_name(), Some(b.name.as_str()));
            });
        })
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn discard_dialog_cannot_clear_a_replacement_buffer_with_the_same_path(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/shared.rs".into()), w, cx);
        s.differ
            .update(cx, |d, cx| d.paste_lines("unsaved A", w, cx));
        s.open_workspace(1, w, cx);
        assert!(w.has_active_dialog(cx));
        // Simulate a workspace replacement while the old confirmation is still open.
        s.open_workspace_now(1, w, cx);
        s.differ.update(cx, |d, cx| {
            let ws = d.workspace().clone().unwrap();
            d.apply_diff(DiffData::load(&ws, false), cx);
            d.begin_edit("api/shared.rs".into(), w, cx);
            d.paste_lines("unsaved B", w, cx);
        });
    });
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        let edit = s.differ.read(cx).edit_snapshot().unwrap();
        assert_eq!(edit.workspace, "boundary-b");
        assert!(edit.dirty);
        assert!(
            s.differ
                .read(cx)
                .editing()
                .as_ref()
                .unwrap()
                .buf
                .text()
                .contains("unsaved B")
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn overwrite_dialog_cannot_save_another_workspace_same_relative_path(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (a, b) = (
        state.workspaces[0].dir().join("api/shared.rs"),
        state.workspaces[1].dir().join("api/shared.rs"),
    );
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/shared.rs".into()), w, cx);
        s.differ
            .update(cx, |d, cx| d.paste_lines("unsaved A", w, cx));
        fs::write(&a, "external edit\n").unwrap();
        s.differ.update(cx, |d, cx| d.save_edit(false, w, cx));
    });
    cx.wait_for(window, Duration::from_secs(10), |w, cx| {
        w.has_active_dialog(cx)
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_workspace_now(1, w, cx);
        s.differ.update(cx, |d, cx| {
            let ws = d.workspace().clone().unwrap();
            d.apply_diff(DiffData::load(&ws, false), cx);
            d.begin_edit("api/shared.rs".into(), w, cx);
            d.paste_lines("unsaved B", w, cx);
        });
    });
    press_in(window)(cx, &["enter"]);
    assert_eq!(fs::read_to_string(a).unwrap(), "external edit\n");
    assert_eq!(fs::read_to_string(b).unwrap(), "boundary-b\n");
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .differ
                .read(cx)
                .edit_snapshot()
                .unwrap()
                .dirty
        )
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stale_file_menu_cannot_act_on_another_workspace(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    let action = shark.update(cx, |s, cx| {
        s.file_menu("api/shared.rs", cx)
            .into_iter()
            .find_map(|entry| match entry {
                Entry::Item {
                    label, on_click, ..
                } if label == "Viewed" => Some(on_click),
                _ => None,
            })
            .unwrap()
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_workspace_now(1, w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    cx.update_window(window, |_, w, cx| action(w, cx)).unwrap();
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.active_ws().unwrap().viewed.is_empty());
        assert!(s.differ.read(cx).viewed_now().is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn discard_revalidates_editors_that_changed_while_the_dialog_was_open(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        fs::write(s.active_ws().unwrap().dir().join("api/other.rs"), "other\n").unwrap();
        s.open_path_tab("api/other.rs".into(), w, cx);
        s.edit_working_copy(Some("api/shared.rs".into()), w, cx);
        s.differ
            .update(cx, |d, cx| d.paste_lines("unsaved inline", w, cx));
        s.open_workspace(1, w, cx);
        assert!(w.has_active_dialog(cx));
        // Another editor changes after the combined discard snapshot was taken.
        s.file_tabs[0].editor.update(cx, |e, _| e.dirty = true);
    });
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("boundary-a"));
        assert!(s.file_tabs[0].editor.read(cx).dirty);
        assert!(s.differ.read(cx).edit_snapshot().unwrap().dirty);
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn deleted_diff_file_can_select_an_existing_editor(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    let deleted = || {
        DiffFile::parse("diff --git a/api/shared.rs b/api/shared.rs\ndeleted file mode 100644\n--- a/api/shared.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n").unwrap()
    };
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| {
            *d.diff_files_mut() = deleted();
            d.open_file(0, w, cx);
        });
    });
    cx.update(|cx| assert!(shark.read(cx).file_tabs.is_empty()));
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_path("api/shared.rs".into(), w, cx);
        s.show(Tab::Feature(Feature::Diff), w, cx);
        s.differ.update(cx, |d, cx| {
            *d.diff_files_mut() = deleted();
            d.open_file(0, w, cx);
        });
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::File);
        assert_eq!(s.file_tabs.len(), 1);
        assert_eq!(s.editor.as_ref().unwrap().read(cx).path, "api/shared.rs");
    });
}
