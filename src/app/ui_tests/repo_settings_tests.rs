use super::*;
use crate::state::RepoSettings;

/// Repos live in Settings, not the sidebar; leaving for a new agent closes Settings too.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repositories_page_lists_repos_and_new_agent_closes_settings(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find(("repo", 0usize)).is_none(), "not in the sidebar");
    })
    .unwrap();
    on(cx, &|s, w, cx| crate::settings::open_repos(&s.state, w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.right_click(("repo", 1usize), cx);
    })
    .unwrap();
    cx.read(|cx| assert!(shark.read(cx).menu.is_some(), "right-click opens its menu"));
    on(cx, &|s, _, cx| {
        s.menu = None;
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(("repo", 1usize), cx);
        w.render_frame(cx);
        assert!(
            w.try_find("settings-page").is_some(),
            "Settings stays underneath"
        );
        w.click("repo-new-workspace", cx);
        w.render_frame(cx);
        assert!(w.try_find("settings-page").is_none());
        assert!(w.try_find("repo-new-workspace").is_none());
    })
    .unwrap();
}

/// A queued attachment must keep its setup configuration, including while removal is open.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pending_attachment_prevents_removing_its_repo(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    for confirm_already_open in [false, true] {
        let home = test_home();
        let (window, shark, repos) =
            attach_tests::agent(cx, &home, &["api"], &[("api", "echo setup")]);
        let on = on_app(window, shark.clone());
        on(cx, &|s, w, cx| {
            super::super::repo_settings::open(s, repos[0].clone(), w, cx);
        });
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.scroll(
                "repo-new-workspace",
                ScrollDelta::Pixels(point(px(0.), px(-1000.))),
                cx,
            );
        })
        .unwrap();
        if confirm_already_open {
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                w.click("remove-repo", cx);
            })
            .unwrap();
        }
        on(cx, &|s, w, cx| {
            let name = s.active.clone().unwrap();
            s.setting_up.push(name.clone());
            s.queue_attachment(
                name,
                vec![crate::state::Attachment::fresh(
                    &repos[0], "pending", "main",
                )],
                w,
                cx,
            );
            assert_eq!(
                s.active_ws().unwrap().attaching[0].stage,
                crate::state::AttachStage::Queued
            );
        });
        if !confirm_already_open {
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                w.click("remove-repo", cx);
            })
            .unwrap();
        }
        press_in(window)(cx, &["enter"]);
        cx.read(|cx| {
            let s = shark.read(cx);
            assert_eq!(
                s.state.repos.len(),
                1,
                "confirmation open: {confirm_already_open}"
            );
            assert_eq!(s.state.repo_settings["api"].setup, ["echo setup"]);
        });
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
    }
}

/// A repo row opens its settings; its default model, setup, run (cmd-r) and archive scripts
/// apply to workspaces with it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repo_settings_scripts_and_model(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let settings = RepoSettings {
        model: Some("opus".into()),
        setup: vec!["echo $SHARK_WORKSPACE > $SHARK_ROOT_PATH/setup.txt".into()],
        run: vec!["echo $SHARK_PORT > $SHARK_ROOT_PATH/port.txt; sleep 30".into()],
        archive: vec!["echo bye > $SHARK_ROOT_PATH/archived.txt".into()],
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            repo_settings: [("api".to_string(), settings)].into(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);

    on(cx, &|s, w, cx| crate::settings::open_repos(&s.state, w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(("repo", 0usize), cx);
        w.render_frame(cx);
        assert!(
            w.try_find("repo-new-workspace").is_some(),
            "a repo row opens its settings"
        );
        w.close_all_dialogs(cx);
    })
    .unwrap();

    on(cx, &|s, w, cx| {
        s.create_workspace(
            "scripts".into(),
            git::on_branch(&repos, "scripts"),
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
    // Untracked files in the worktree would block archiving, so scripts write to the source.
    let wt = repos[0].path.clone();
    let model = |cx: &mut TestAppContext| {
        cx.read(|cx| shark.read(cx).active_ws().unwrap().chats[0].model.clone())
    };
    assert_eq!(
        model(cx).as_deref(),
        Some("opus"),
        "the repo's default model"
    );
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        fs::read_to_string(wt.join("setup.txt")).is_ok_and(|s| s == "scripts\n")
    })
    .await;

    press(cx, &["cmd-r"]);
    assert!(cx.read(|cx| shark.read(cx).running("scripts")));
    cx.wait_for(window, Duration::from_secs(10), |_, _| {
        fs::read_to_string(wt.join("port.txt")).is_ok_and(|s| s == "51000\n")
    })
    .await;
    press(cx, &["cmd-r"]);
    assert!(
        !cx.read(|cx| shark.read(cx).running("scripts")),
        "cmd-r again stops"
    );
    // The header's run button toggles like cmd-r (a stopped run keeps its port while it stops).
    for want in [true, false] {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("run", cx);
        })
        .unwrap();
        assert_eq!(
            cx.read(|cx| shark.read(cx).running("scripts")),
            want,
            "the run button toggles"
        );
    }

    on(cx, &|s, w, cx| s.archive_ws("scripts".into(), false, w, cx));
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.is_empty()
    })
    .await;
    let archived = fs::read_to_string(repos[0].path.join("archived.txt")).unwrap();
    assert_eq!(archived, "bye\n");
}
