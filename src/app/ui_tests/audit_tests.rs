use super::*;

#[gpui_kit::test]
fn recreated_workspace_can_save_comments_after_unreadable_store(_: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let path = review::comments_path("recreated");
    fs::create_dir_all(&path).unwrap();
    let mut review = review::CommentStore::default();
    review.load("recreated");
    assert!(review.warning.is_some());
    fs::remove_dir(&path).unwrap();
    review.forget("recreated");
    review.edit("recreated", |comments| {
        comments.push(serde_json::from_str(r#"{"id":1,"path":"api/a","start":[false,1],"end":[false,1],"text":"new workspace"}"#).unwrap());
    });
    assert!(review.failed_saves.is_empty());
    assert!(fs::read_to_string(path).unwrap().contains("new workspace"));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn creation_reservations_and_unsaved_metadata(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web", "docs"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let state = crate::state::support_dir().join("state.json");
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    on(cx, &|s, w, cx| {
        for _ in 0..2 {
            s.create_workspace(
                "owned".into(),
                git::on_branch(&repos[..1], "owned"),
                vec![],
                None,
                w,
                cx,
            );
        }
        assert_eq!(s.creating.len(), 1);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    on(cx, &|s, w, cx| {
        assert_eq!(s.state.workspaces.len(), 1);
        assert!(s.state.workspaces[0].dir().join("api").exists());
        assert_eq!(s.unsaved_data(None, cx).len(), 1);
        s.add_repo_to_workspace("owned".into(), repos[1].clone(), w, cx);
        s.add_repo_to_workspace("owned".into(), repos[2].clone(), w, cx);
        assert_eq!(s.adding_repos.len(), 1);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).adding_repos.is_empty()
    })
    .await;
    on(cx, &|s, w, cx| {
        assert_eq!(s.state.workspaces[0].repos.len(), 2);
        s.add_repo_to_workspace("owned".into(), repos[2].clone(), w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces[0].repos.len() == 3
    })
    .await;
    fs::remove_dir(&state).unwrap();
    on(cx, &|s, _, cx| s.retry_saves(cx));
    assert_eq!(State::load().workspaces[0].repos.len(), 3);
    on(cx, &|s, _, cx| assert!(s.unsaved_data(None, cx).is_empty()));
}

#[gpui_kit::test]
fn failed_saves_retry_latest_comments_and_guard_close(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    let state = crate::state::support_dir().join("state.json");
    let comments = review::comments_path("retry");
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    fs::create_dir_all(&comments).unwrap();
    on(cx, &|s, w, cx| {
        // Load before injecting the failure: an unreadable original store remains protected.
        s.differ.update(cx, |d, _| {
        d.review_mut().store.comments.insert("retry".into(), vec![]);
        d.review_mut().store.edit("retry", |cs| cs.push(serde_json::from_str(r#"{"id":7,"path":"api/a","start":[false,1],"end":[false,1],"text":"first","github":"https://example.test/already-posted"}"#).unwrap()));
        d.review_mut().store.edit("retry", |cs| cs[0].text = "latest".into());
        });
        s.state.vim = true;
        assert!(s.state.save().is_err());
        assert_eq!(s.unsaved_data(None, cx).len(), 2);
        assert!(!s.close_window(w, cx));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("save-cancel-close", cx);
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        assert!(!w.has_active_dialog(cx));
        assert_eq!(s.unsaved_data(None, cx).len(), 2);
        s.quit(w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("save-retry-close", cx);
        w.render_frame(cx);
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        assert!(w.has_active_dialog(cx));
        assert_eq!(s.unsaved_data(None, cx).len(), 2);
        w.close_dialog(cx);
    });
    fs::remove_dir(&state).unwrap();
    fs::remove_dir(&comments).unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("retry-save", cx);
    })
    .unwrap();
    on(cx, &|s, _, cx| assert!(s.unsaved_data(None, cx).is_empty()));
    assert!(State::load().vim);
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(comments).unwrap()).unwrap();
    assert_eq!(saved["comments"][0]["text"], "latest");
    assert_eq!(
        saved["comments"][0]["github"],
        "https://example.test/already-posted"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn both_editors_keep_hard_link_buffers_and_quit_guards_recheck_saves(
    cx: &mut TestAppContext,
) {
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
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "save".into(),
            git::on_branch(&repos, "save"),
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
    let dir = home.join("shark/workspaces/save/api");
    for name in ["full", "inline"] {
        fs::write(dir.join(name), "original\n").unwrap();
        fs::hard_link(dir.join(name), dir.join(format!("{name}-link"))).unwrap();
    }
    on(cx, &|s, w, cx| {
        s.refresh_diff(cx);
        s.open_path("api/full".into(), w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        s.differ.read(cx).diff_files().len() == 4
            && s.editor
                .as_ref()
                .is_some_and(|e| e.read(cx).input.read(cx).value() == "original\n")
    })
    .await;
    on(cx, &|s, w, cx| {
        s.editor.clone().unwrap().update(cx, |e, cx| {
            e.input
                .update(cx, |i, cx| i.set_value("full buffer", w, cx));
            e.dirty = true;
            e.save(true, w, cx);
        });
    });
    cx.wait_for(window, Duration::from_secs(5), |_, cx| {
        !shark.read(cx).editor.as_ref().unwrap().read(cx).saving
    })
    .await;
    on(cx, &|s, w, cx| {
        let e = s.editor.as_ref().unwrap().read(cx);
        assert!(e.dirty);
        assert_eq!(e.input.read(cx).value(), "full buffer");
        s.edit_working_copy(Some("api/inline".into()), w, cx);
        s.differ.update(cx, |d, cx| {
            let e = d.editing_mut().as_mut().unwrap();
            e.buf.lines[0] = "inline buffer".into();
            e.dirty = true;
            d.save_edit(true, w, cx);
        });
    });
    cx.wait_for(window, Duration::from_secs(5), |_, cx| {
        !shark
            .read(cx)
            .differ
            .read(cx)
            .editing()
            .as_ref()
            .unwrap()
            .saving
    })
    .await;
    on(cx, &|host, _w, cx| {
        host.differ.update(cx, |s, _cx| {
            let e = s.editing().as_ref().unwrap();
            assert!(e.dirty);
            assert_eq!(e.buf.text(), "inline buffer\n");
        })
    });
    for name in ["full", "full-link", "inline", "inline-link"] {
        assert_eq!(fs::read(dir.join(name)).unwrap(), b"original\n");
    }
    let state = crate::state::support_dir().join("state.json");
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    on(cx, &|s, w, cx| {
        assert!(s.state.save().is_err());
        s.quit(w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("save-abandon-close", cx);
        w.render_frame(cx);
    })
    .unwrap();
    on(cx, &|host, w, cx| {
        host.differ.update(cx, |s, cx| {
            assert!(
                w.has_active_dialog(cx),
                "editor confirmation follows Quit without saving"
            );
            assert!(s.editing().as_ref().unwrap().dirty);
            w.close_dialog(cx);
        })
    });
    fs::remove_dir(&state).unwrap();
    on(cx, &|s, w, cx| {
        s.quit(w, cx);
        assert!(
            s.unsaved_data(None, cx).is_empty(),
            "Quit retries state before checking editor buffers"
        );
        assert!(w.has_active_dialog(cx));
        w.close_dialog(cx);
    });

    for quitting in [false, true] {
        on(cx, &|s, w, cx| {
            s.editor.as_ref().unwrap().update(cx, |e, _| e.dirty = true);
            if quitting {
                s.quit(w, cx);
            } else {
                assert!(!s.close_window(w, cx));
            }
            assert!(s.unsaved_data(None, cx).is_empty());
            assert!(w.has_active_dialog(cx));
        });
        // A background update fails to save while the editor-discard dialog is open.
        fs::remove_file(&state).unwrap();
        fs::create_dir(&state).unwrap();
        on(cx, &|s, _, _cx| {
            s.state.vim = !s.state.vim;
            assert!(s.state.save().is_err());
        });
        press_in(window)(cx, &["enter"]);
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(
                w.has_active_dialog(cx),
                "new save failures must block closing"
            );
            w.click("save-cancel-close", cx);
        })
        .unwrap();
        on(cx, &|s, _, cx| {
            assert_eq!(s.unsaved_data(None, cx).len(), 1);
            assert!(s.editor.as_ref().unwrap().read(cx).dirty);
            assert!(s.differ.read(cx).editing().as_ref().unwrap().dirty);
        });
        fs::remove_dir(&state).unwrap();
        on(cx, &|s, _, cx| s.retry_saves(cx));
    }
}
