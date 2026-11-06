use super::*;
use crate::state::RepoSettings;

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

    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(("repo", 0usize), cx);
        assert!(w.has_active_dialog(cx), "a repo row opens its settings");
        w.close_dialog(cx);
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
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.is_empty()
    })
    .await;
    let archived = fs::read_to_string(repos[0].path.join("archived.txt")).unwrap();
    assert_eq!(archived, "bye\n");
}
