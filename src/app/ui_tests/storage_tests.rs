use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_operations_finish_before_closing(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
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
            "closing".into(),
            git::on_branch(&repos[..1], "closing"),
            vec![],
            None,
            w,
            cx,
        );
        assert!(!s.creating.is_empty());
        assert!(
            !s.close_window(w, cx),
            "creation must save its result first"
        );
        assert!(!s.storage_guard(true, w, cx));
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    assert_eq!(State::load().workspaces.len(), 1);
    on(cx, &|s, w, cx| {
        s.add_repo_to_workspace("closing".into(), repos[1].clone(), w, cx);
        assert!(!s.adding_repos.is_empty());
        assert!(
            !s.close_window(w, cx),
            "repo addition must save its result first"
        );
        assert!(!s.storage_guard(true, w, cx));
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).adding_repos.is_empty()
    })
    .await;
    assert_eq!(State::load().workspaces[0].repos.len(), 2);
    on(cx, &|s, w, cx| {
        s.remove_workspace("closing", false, w, cx);
        assert!(!s.archiving.is_empty());
        assert!(!s.close_window(w, cx), "archive must save its result first");
        assert!(!s.storage_guard(true, w, cx));
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).archiving.is_empty()
    })
    .await;
    assert!(State::load().workspaces.is_empty());
    on(cx, &|s, w, cx| assert!(s.close_window(w, cx)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn blocked_archive_retains_dirty_buffers(cx: &mut TestAppContext) {
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
            "archive-save".into(),
            git::on_branch(&repos, "archive-save"),
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
    let path = home.join("shark/workspaces/archive-save/api/file");
    fs::write(&path, "original").unwrap();
    on(cx, &|s, w, cx| s.open_path("api/file".into(), w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .editor
            .as_ref()
            .unwrap()
            .read(cx)
            .input
            .read(cx)
            .value()
            == "original"
    })
    .await;
    on(cx, &|s, w, cx| {
        s.editor.as_ref().unwrap().update(cx, |e, cx| {
            e.input
                .update(cx, |i, cx| i.set_value("unsaved buffer", w, cx));
            e.dirty = true;
        });
        s.remove_workspace("archive-save", true, w, cx);
        assert!(w.has_active_dialog(cx));
    });
    let comments = review::comments_path("archive-save");
    fs::create_dir_all(&comments).unwrap();
    on(cx, &|host, _w, cx| {
        host.differ.update(cx, |s, _cx| {
        s.review_mut().comments.insert("archive-save".into(), vec![]);
        s.review_mut().edit("archive-save", |comments| {
            comments.push(serde_json::from_str(r#"{"id":1,"path":"api/file","start":[false,1],"end":[false,1],"text":"keep me"}"#).unwrap());
        });
        assert!(s.review().failed_saves.contains_key("archive-save"));
    })
    });
    press_in(window)(cx, &["enter"]);
    on(cx, &|s, _, cx| {
        assert!(s.archiving.is_empty());
        let editor = s.editor.as_ref().unwrap().read(cx);
        assert!(
            editor.dirty,
            "blocked archive must retain unsaved editor state"
        );
        assert_eq!(editor.input.read(cx).value(), "unsaved buffer");
    });
    assert_eq!(fs::read(&path).unwrap(), b"original");
    fs::remove_dir(comments).unwrap();
    on(cx, &|s, w, cx| {
        s.retry_saves(cx);
        s.remove_workspace("archive-save", false, w, cx);
    });
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).archiving.is_empty()
    })
    .await;
    on(cx, &|s, w, cx| {
        assert!(
            s.ws("archive-save").is_some(),
            "Git refuses the untracked file"
        );
        assert!(
            s.editor.as_ref().unwrap().read(cx).dirty,
            "Git failure retains unsaved edits too"
        );
        s.remove_workspace("archive-save", true, w, cx);
    });
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).archiving.is_empty()
    })
    .await;
    on(cx, &|s, _, _cx| {
        assert!(s.ws("archive-save").is_none());
        assert!(s.editor.is_none());
    });
    assert!(!path.exists());
}
