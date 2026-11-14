use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_plan_save_blocks_close_and_archive_until_retry(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    blank_task(cx, window, &shark);
    let (task, id, plan) = cx.update(|cx| {
        let s = shark.read(cx);
        let chat = s.active_chat().unwrap().read(cx);
        (
            s.active.clone().unwrap(),
            chat.meta.id.clone(),
            chat.plan.clone(),
        )
    });
    let path = crate::plan::store_path(&id);
    fs::create_dir_all(&path).unwrap(); // A directory cannot be replaced by the plan file.
    cx.update(|cx| {
        plan.update(cx, |p, cx| {
            p.add("p1".into(), "# Keep this plan".into(), cx);
            p.approve(cx);
            assert!(p.storage_error().is_some());
        })
    });
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        assert!(
            s.unsaved_data(Some(&task), cx)
                .iter()
                .any(|e| e.starts_with("Plans in "))
        );
        assert!(!s.storage_guard(true, w, cx));
        w.close_dialog(cx);
        assert!(!s.close_window(w, cx));
        w.close_dialog(cx);
        s.close_chat(w, cx);
        assert!(s.chats.contains_key(&id));
        s.remove_task(&task, true, w, cx);
        assert!(s.archiving.is_empty());
        assert!(s.task(&task).is_some());
    });
    fs::remove_dir(&path).unwrap();
    on(cx, &|s, _, cx| {
        s.retry_saves(cx);
        assert!(s.unsaved_data(None, cx).is_empty());
        assert!(plan.read(cx).storage_error().is_none());
    });
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["versions"][0]["text"], "# Keep this plan");
    assert_eq!(saved["approved"], 0);
    on(cx, &|s, w, cx| assert!(s.close_window(w, cx)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn unreadable_plans_are_protected_on_add_approve_and_retry(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    cx.update(|cx| gpui_kit::init(cx));
    let path = crate::plan::store_path("protected-plan");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for directory in [false, true] {
        if directory {
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"corrupt plan contents").unwrap();
        }
        cx.update(|cx| {
            let plan = cx.new(|cx| crate::plan::PlanPane::new("protected-plan", false, cx));
            plan.update(cx, |p, cx| {
                assert!(p.storage_error().is_some());
                p.add("p2".into(), "# New plan in memory".into(), cx);
                p.approve(cx);
                p.retry_save(cx);
                assert!(p.storage_error().is_some());
            });
        });
        if directory {
            assert!(path.is_dir());
        } else {
            assert_eq!(fs::read(&path).unwrap(), b"corrupt plan contents");
        }
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn task_operations_finish_before_closing(cx: &mut TestAppContext) {
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
        s.create_task_for_test(
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
    assert_eq!(State::load().tasks.len(), 1);
    on(cx, &|s, w, cx| {
        let web = crate::state::Attachment::fresh(&repos[1], "closing", "");
        s.queue_attachment("closing".into(), vec![web], w, cx);
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
    assert_eq!(State::load().tasks[0].repos.len(), 2);
    on(cx, &|s, w, cx| {
        s.remove_task("closing", false, w, cx);
        assert!(!s.archiving.is_empty());
        assert!(!s.close_window(w, cx), "archive must save its result first");
        assert!(!s.storage_guard(true, w, cx));
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).archiving.is_empty()
    })
    .await;
    assert!(State::load().tasks.is_empty());
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
        s.create_task_for_test(
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
        s.remove_task("archive-save", true, w, cx);
        assert!(w.has_active_dialog(cx));
    });
    let comments = review::comments_path("archive-save");
    fs::create_dir_all(&comments).unwrap();
    on(cx, &|host, _w, cx| {
        host.differ.update(cx, |s, _cx| {
        s.review_mut().store.comments.insert("archive-save".into(), vec![]);
        s.review_mut().store.edit("archive-save", |comments| {
            comments.push(serde_json::from_str(r#"{"id":1,"path":"api/file","start":[false,1],"end":[false,1],"text":"keep me"}"#).unwrap());
        });
        assert!(s.review().store.failed_saves.contains_key("archive-save"));
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
        s.remove_task("archive-save", false, w, cx);
    });
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).archiving.is_empty()
    })
    .await;
    on(cx, &|s, w, cx| {
        assert!(
            s.task("archive-save").is_some(),
            "Git refuses the untracked file"
        );
        assert!(
            s.editor.as_ref().unwrap().read(cx).dirty,
            "Git failure retains unsaved edits too"
        );
        s.remove_task("archive-save", true, w, cx);
    });
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).archiving.is_empty()
    })
    .await;
    on(cx, &|s, _, _cx| {
        assert!(s.task("archive-save").is_none());
        assert!(s.editor.is_none());
    });
    assert!(!path.exists());
}
