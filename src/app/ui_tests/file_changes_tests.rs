use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn uncommitted_dots_and_file_discard(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let commit = |dir: &Path| {
        git::git(dir, &["add", "."]).unwrap();
        git::commit(dir, &["-qm", "files"]);
    };
    for repo in &repos {
        fs::write(repo.path.join("file.txt"), "base\n").unwrap();
        fs::write(repo.path.join("back.txt"), "base\n").unwrap();
        commit(&repo.path);
    }
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
            "discard".into(),
            git::on_branch(&repos, "discard"),
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
    let dir = home.join("shark/workspaces/discard");
    let api = dir.join("api");
    fs::write(api.join("file.txt"), "feature\n").unwrap();
    fs::write(api.join("back.txt"), "feature\n").unwrap();
    fs::write(api.join("committed.txt"), "committed\n").unwrap();
    commit(&api);
    fs::write(api.join("file.txt"), "staged\n").unwrap();
    git::git(&api, &["add", "file.txt"]).unwrap();
    fs::write(api.join("file.txt"), "unstaged\n").unwrap();
    fs::write(api.join("back.txt"), "base\n").unwrap();
    fs::write(api.join("new.txt"), "new\n").unwrap();
    fs::write(dir.join("web/file.txt"), "web edits\n").unwrap();
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, _| *d.show_tree_mut() = true);
        s.show(Tab::Feature(Feature::Diff), w, cx);
    });
    let refresh = |cx: &mut TestAppContext| on(cx, &|s, _, cx| s.refresh_diff(cx));
    let has_discard = |s: &Shark, p: &str, cx: &mut Context<Shark>| {
        s.file_menu(p, cx).iter().any(
            |e| matches!(e, Entry::Item { label, .. } if label == "Discard uncommitted changes…"),
        )
    };
    shark.update(cx, |s, cx| {
        assert_eq!(s.differ.read(cx).uncommitted().len(), 4);
        assert!(s.differ.read(cx).uncommitted().contains("api/back.txt"));
        assert!(
            s.differ
                .read(cx)
                .diff_files()
                .iter()
                .any(|f| f.path() == "api/back.txt"),
            "local edits equal to base still get a row"
        );
        assert!(has_discard(s, "api/file.txt", cx));
        assert!(!has_discard(s, "api/committed.txt", cx));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.simulate_next_frame(cx);
        let s = shark.read(cx);
        for (ix, row) in s
            .differ
            .read(cx)
            .tree_rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| r.file.is_some())
        {
            assert_eq!(
                w.find_all(("uncommitted", ix)).len(),
                usize::from(s.differ.read(cx).uncommitted().contains(&row.path)),
                "{}",
                row.path
            );
        }
    })
    .unwrap();

    // Use the actual right-click menu; cancel must preserve both index and worktree.
    cx.update_window(window, |_, w, cx| {
        let ix = shark
            .read(cx)
            .differ
            .read(cx)
            .tree_rows()
            .iter()
            .position(|r| r.path == "api/file.txt")
            .unwrap();
        let position = w.find(("tree-row", ix)).bounds().center();
        w.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Right,
                position,
                click_count: 1,
                ..Default::default()
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    press(cx, &["d", "enter"]);
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    assert_eq!(
        fs::read_to_string(api.join("file.txt")).unwrap(),
        "unstaged\n"
    );
    press(cx, &["escape"]);
    assert_eq!(git::git(&api, &["show", ":file.txt"]).unwrap(), "staged");

    let chat = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    chat.update(cx, |c, _| c.busy = true);
    on(cx, &|s, w, cx| {
        s.discard_uncommitted("discard", "api/file.txt", w, cx);
        assert!(
            !w.has_active_dialog(cx),
            "running agents block destructive actions"
        );
    });
    chat.update(cx, |c, _| c.busy = false);

    // A save that starts while the confirmation is open must prevent discard.
    on(cx, &|s, w, cx| s.open_path("api/file.txt".into(), w, cx));
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "unstaged\n"
    })
    .await;
    on(cx, &|s, w, cx| {
        s.discard_uncommitted("discard", "api/file.txt", w, cx)
    });
    editor.update(cx, |e, _| e.saving = true);
    press(cx, &["enter"]);
    assert_eq!(
        fs::read_to_string(api.join("file.txt")).unwrap(),
        "unstaged\n"
    );
    editor.update(cx, |e, _| e.saving = false);
    cx.update_window(window, |_, w, cx| w.input("unsaved ", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(editor.read(cx).dirty));
    on(cx, &|s, w, cx| {
        s.discard_uncommitted("discard", "api/file.txt", w, cx)
    });
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "feature\n"
    })
    .await;
    assert_eq!(git::git(&api, &["show", ":file.txt"]).unwrap(), "feature");
    assert_eq!(
        fs::read_to_string(dir.join("web/file.txt")).unwrap(),
        "web edits\n"
    );
    cx.update(|cx| {
        assert!(
            !shark
                .read(cx)
                .differ
                .read(cx)
                .uncommitted()
                .contains("api/file.txt")
        )
    });

    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.toggle_uncommitted(w, cx))
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.state.uncommitted_only);
        assert!(s.differ.read(cx).uncommitted().contains("api/back.txt"));
        assert!(
            !s.differ
                .read(cx)
                .diff_files()
                .iter()
                .any(|f| f.path() == "api/committed.txt")
        );
    });
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.toggle_uncommitted(w, cx))
    });

    // Committing without changing the base patch must still remove the dot.
    fs::write(api.join("file.txt"), "another feature\n").unwrap();
    refresh(cx);
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .differ
                .read(cx)
                .uncommitted()
                .contains("api/file.txt")
        )
    });
    let fingerprint = cx.update(|cx| shark.read(cx).differ.read(cx).diff_fp());
    git::git(&api, &["add", "file.txt"]).unwrap();
    git::commit(&api, &["-qm", "more work"]);
    refresh(cx);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.differ.read(cx).diff_fp(),
            fingerprint,
            "committing does not change the base patch"
        );
        assert!(!s.differ.read(cx).uncommitted().contains("api/file.txt"));
    });

    // New files disappear, including their open tab, after confirmation.
    on(cx, &|s, w, cx| s.open_path("api/new.txt".into(), w, cx));
    on(cx, &|s, w, cx| {
        s.discard_uncommitted("discard", "api/new.txt", w, cx)
    });
    press(cx, &["enter"]);
    assert!(!api.join("new.txt").exists());
    cx.update(|cx| {
        assert!(
            !shark
                .read(cx)
                .file_tabs
                .iter()
                .any(|t| t.editor.read(cx).path == "api/new.txt")
        )
    });
    fs::remove_dir_all(home).unwrap();
}

/// A PR target learned after opening a workspace must replace the trunk diff immediately.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stacked_pr_target_refreshes_diff_and_review_base(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let src = &repos[0].path;
    let g = |dir: &Path, args: &[&str]| {
        git::git(
            dir,
            &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
        )
        .unwrap()
    };
    fs::write(src.join("shared.txt"), "trunk\n").unwrap();
    g(src, &["add", "."]);
    g(src, &["commit", "-qm", "trunk"]);
    g(src, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fs::write(src.join("shared.txt"), "parent\n").unwrap();
    fs::write(src.join("parent.txt"), "parent only\n").unwrap();
    g(src, &["add", "."]);
    g(src, &["commit", "-qm", "parent"]);
    let parent = g(src, &["rev-parse", "HEAD"]);
    g(src, &["update-ref", "refs/remotes/origin/parent", "HEAD"]);
    g(src, &["branch", "child"]);
    let dir = home.join("shark/workspaces/pr-diff");
    let ws =
        git::create_workspace(&dir, "pr-diff", "child", &git::on_branch(&repos, "child")).unwrap();
    let wt = dir.join("api");
    fs::write(wt.join("shared.txt"), "child\n").unwrap();
    g(&wt, &["add", "."]);
    g(&wt, &["commit", "-qm", "child"]);
    let head = g(&wt, &["rev-parse", "HEAD"]);
    let (window, shark) = boot(
        cx,
        State {
            repos,
            workspaces: vec![ws],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.open_workspace(0, w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_loaded()
    })
    .await;
    let sync = cx.update(|cx| shark.read(cx).syncs["pr-diff"].clone());
    sync.update(cx, |g, cx| g.set_active(false, cx));
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .differ
                .read(cx)
                .diff_files()
                .iter()
                .any(|f| f.path() == "api/parent.txt")
        )
    });
    sync.update(cx, |g, cx| {
        g.set_pr("api", 7, false);
        g.set_pr_base("api", "parent", cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_base("api") == "origin/parent"
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(
            s.differ.read(cx).diff_error().is_none(),
            "{:?}",
            s.differ.read(cx).diff_error()
        );
        assert_eq!(
            s.differ
                .read(cx)
                .diff_files()
                .iter()
                .map(|f| f.path().as_str())
                .collect::<Vec<_>>(),
            ["api/shared.txt"]
        );
        assert_eq!(s.differ.read(cx).changes_stats().0, "origin/parent");
        assert_eq!(
            s.active_ws().unwrap().repos[0].base,
            "origin/main",
            "configured lifecycle base is preserved"
        );
    });
    // Re-quoting an old-side comment uses the displayed parent, not the configured trunk.
    on(cx, &|host, _w, cx| {
        host.differ.update(cx, |s, cx| {
            let c: review::Comment = serde_json::from_value(serde_json::json!({
                "id": 1, "path": "api/shared.txt", "start": [true, 1], "end": [true, 1],
                "text": "review", "status": "Outdated"
            }))
            .unwrap();
            s.edit_comments(cx, |comments| comments.push(c));
            s.requeue_outdated(cx);
            let c = &s.ws_comments()[0];
            assert_eq!(c.snapshot, "parent");
            assert_eq!(serde_json::to_value(c).unwrap()["rev"], parent);
        })
    });
    // Retargeting while editing updates the original while preserving the unsaved buffer.
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/shared.txt".into()), w, cx);
        s.differ.update(cx, |d, cx| d.paste_lines("unsaved", w, cx));
    });
    let buffer = cx.update(|cx| {
        shark
            .read(cx)
            .differ
            .read(cx)
            .editing()
            .as_ref()
            .unwrap()
            .buf
            .text()
    });
    sync.update(cx, |g, cx| g.set_pr_base("api", "main", cx));
    cx.run_until_parked();
    // Also apply a comparison that started before editing began.
    on(cx, &|s, _, cx| {
        let ws = s.active_ws().unwrap().clone();
        s.differ
            .update(cx, |d, cx| d.apply_diff(DiffData::load(&ws, false), cx));
        assert_eq!(s.differ.read(cx).diff_base("api"), "origin/main");
        let e = s.differ.read(cx).editing().as_ref().unwrap();
        assert_eq!(e.buf.text(), buffer);
        assert!(e.dirty && e.patch.contains("-trunk"));
        s.differ.update(cx, |d, cx| {
            d.edit_comments(cx, |comments| comments[0].status = review::Status::Outdated)
        });
        s.differ.update(cx, |d, cx| d.requeue_outdated(cx));
        assert_eq!(s.differ.read(cx).ws_comments()[0].snapshot, "trunk");
    });
    // A fetch can move the same target ref, including a rewritten parent.
    let trunk = g(&wt, &["rev-parse", "origin/main"]);
    for (revision, original) in [
        (&parent, "parent"),
        (&head, "child"), // The edited file leaves Git’s patch, then returns.
        (&parent, "parent"),
        (&trunk, "trunk"),
    ] {
        g(&wt, &["update-ref", "refs/remotes/origin/main", revision]);
        on(cx, &|s, _, cx| s.refresh_diff(cx));
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark
                .read(cx)
                .differ
                .read(cx)
                .editing()
                .as_ref()
                .is_some_and(|e| e.patch.contains(&format!("-{original}")))
        })
        .await;
        on(cx, &|host, _w, cx| {
            host.differ.update(cx, |s, cx| {
                assert_eq!(s.diff_base("api"), "origin/main");
                assert_eq!(s.editing().as_ref().unwrap().buf.text(), buffer);
                s.edit_comments(cx, |comments| comments[0].status = review::Status::Outdated);
                s.requeue_outdated(cx);
                assert_eq!(s.ws_comments()[0].snapshot, original);
            })
        });
    }
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.save_edit(false, w, cx))
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .differ
            .read(cx)
            .editing()
            .as_ref()
            .is_some_and(|e| !e.dirty && !e.saving)
    })
    .await;
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.leave_edit(w, cx))
    });
    assert_eq!(fs::read_to_string(wt.join("shared.txt")).unwrap(), buffer);
    // Saving and leaving keeps the new target, even though HEAD never moved.
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_base("api") == "origin/main"
    })
    .await;
    cx.update(|cx| {
        assert!(
            shark
                .read(cx)
                .differ
                .read(cx)
                .diff_files()
                .iter()
                .any(|f| f.path() == "api/parent.txt")
        )
    });
    assert_eq!(g(&wt, &["rev-parse", "HEAD"]), head);
    fs::remove_dir_all(home).unwrap();
}
