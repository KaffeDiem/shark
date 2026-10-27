use super::*;

#[gpui_kit::test]
fn adopted_pr_base_persists_through_app_subscription(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let dir = home.join("shark/workspaces/adopted-base");
    let mut ws = git::create_workspace(
        &dir,
        "adopted-base",
        "feature",
        &git::on_branch(&repos, "feature"),
    )
    .unwrap();
    let old = "refs/shark/pull/7/base";
    let new = "origin/release";
    let wt = dir.join("api");
    git::git(&wt, &["update-ref", old, "HEAD"]).unwrap();
    git::git(&wt, &["update-ref", "refs/remotes/origin/release", "HEAD"]).unwrap();
    ws.repos[0].base = old.into();
    git::write_claude_md(&ws);
    let (window, shark) = boot(
        cx,
        State {
            repos,
            workspaces: vec![ws],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_workspace(0, w, cx);
        s.syncs["adopted-base"].update(cx, |sync, cx| sync.set_active(false, cx));
    });
    let (sync, generation) = cx.update(|cx| {
        let s = shark.read(cx);
        (s.syncs["adopted-base"].clone(), s.diff_gen)
    });
    sync.update(cx, |_, cx| {
        cx.emit(crate::sync::SyncEvent::PrBaseAdopted(vec![(
            "api".into(),
            old.into(),
            new.into(),
        )]));
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.ws("adopted-base").unwrap().repos[0].base, new);
        assert!(s.diff_gen > generation, "adoption requests a diff refresh");
    });
    assert_eq!(State::load().workspaces[0].repos[0].base, new);
    for file in ["AGENTS.md", "CLAUDE.md"] {
        let contents = fs::read_to_string(dir.join(file)).unwrap();
        assert!(contents.contains("base `origin/release`"), "{contents}");
        assert!(!contents.contains(old));
    }

    // A delayed adoption answer must not replace a base changed since its probe began.
    sync.update(cx, |_, cx| {
        cx.emit(crate::sync::SyncEvent::PrBaseAdopted(vec![(
            "api".into(),
            old.into(),
            "origin/stale".into(),
        )]));
    });
    cx.run_until_parked();
    assert_eq!(State::load().workspaces[0].repos[0].base, new);
    assert!(
        fs::read_to_string(dir.join("AGENTS.md"))
            .unwrap()
            .contains(new)
    );
}
