use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn uncommitted_dots_and_file_discard(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let commit = |dir: &Path| {
        git::git(dir, &["add", "."]).unwrap();
        git::git(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "files",
            ],
        )
        .unwrap();
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
        s.show_tree = true;
        s.show(Tab::Diff, w, cx);
    });
    let refresh = |cx: &mut TestAppContext| on(cx, &|s, _, cx| s.refresh_diff(cx));
    let has_discard = |s: &Shark, p: &str, cx: &Context<Shark>| {
        s.file_menu(p, cx).iter().any(
            |e| matches!(e, Entry::Item { label, .. } if label == "Discard uncommitted changes…"),
        )
    };
    shark.update(cx, |s, cx| {
        assert_eq!(s.uncommitted.len(), 4);
        assert!(s.uncommitted.contains("api/back.txt"));
        assert!(
            s.diff_files.iter().any(|f| f.path() == "api/back.txt"),
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
            .tree_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.file.is_some())
        {
            assert_eq!(
                w.find_all(("uncommitted", ix)).len(),
                usize::from(s.uncommitted.contains(&row.path)),
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
            .tree_rows
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
    cx.update(|cx| assert!(!shark.read(cx).uncommitted.contains("api/file.txt")));

    on(cx, &|s, w, cx| s.toggle_uncommitted(w, cx));
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.state.uncommitted_only);
        assert!(s.uncommitted.contains("api/back.txt"));
        assert!(!s.diff_files.iter().any(|f| f.path() == "api/committed.txt"));
    });
    on(cx, &|s, w, cx| s.toggle_uncommitted(w, cx));

    // Committing without changing the base patch must still remove the dot.
    fs::write(api.join("file.txt"), "another feature\n").unwrap();
    refresh(cx);
    cx.update(|cx| assert!(shark.read(cx).uncommitted.contains("api/file.txt")));
    let fingerprint = cx.update(|cx| shark.read(cx).diff_fp);
    git::git(&api, &["add", "file.txt"]).unwrap();
    git::git(
        &api,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "more work",
        ],
    )
    .unwrap();
    refresh(cx);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.diff_fp, fingerprint,
            "committing does not change the base patch"
        );
        assert!(!s.uncommitted.contains("api/file.txt"));
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
