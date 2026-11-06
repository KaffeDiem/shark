use super::*;
use std::os::unix::fs::PermissionsExt;

fn invoke(entries: Vec<Entry>, label: &str, window: &mut Window, cx: &mut App) {
    let action = entries
        .into_iter()
        .find_map(|e| match e {
            Entry::Item {
                label: text,
                on_click,
                ..
            } if text == label => Some(on_click),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing menu action {label}"));
    action(window, cx);
}

fn has(entries: &[Entry], text: &str) -> bool {
    entries.iter().any(|e| match e {
        Entry::Item { label, .. } | Entry::Sub { label, .. } => label == text,
        Entry::Sep => false,
    })
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repository_menu_tracks_checkout_and_scopes_links(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let fake_gh = home.join("fake-gh");
    fs::write(&fake_gh, "#!/bin/sh\nprintf '[]\\n'\n").unwrap();
    fs::set_permissions(&fake_gh, fs::Permissions::from_mode(0o755)).unwrap();
    let _gh = GhGuard::set(&fake_gh);
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
            "menu-scope".into(),
            git::on_branch(&repos, "menu-scope"),
            vec![],
            None,
            w,
            cx,
        )
    });
    until(cx, window, |_, cx| shark.read(cx).active.is_some()).await;
    on(cx, &|s, _, cx| {
        let sync = s.syncs.remove("menu-scope").unwrap();
        let ws = s.ws("menu-scope").unwrap();
        assert_eq!(
            menus::repo_branch(s, ws, &ws.repos[0], cx).as_deref(),
            Some("menu-scope"),
            "saved branch before a probe"
        );
        s.syncs.insert("menu-scope".into(), sync);
    });
    until(cx, window, |_, cx| {
        shark.read(cx).syncs["menu-scope"]
            .read(cx)
            .repos()
            .is_some()
    })
    .await;
    on(cx, &|s, w, cx| {
        s.syncs["menu-scope"].update(cx, |g, _| {
            g.set_pr("api", 11, true);
            g.set_pr("web", 22, true);
        });
        let ws = s.ws("menu-scope").unwrap();
        let entries = menus::branch_entries(s, ws, Some("api"), cx);
        assert!(!has(&entries, "New agent on top…"));
        assert!(!has(&entries, "Insert agent below…"));
        invoke(entries, "Open pull request", w, cx);
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/o/api/pull/11")
    );
    on(cx, &|s, w, cx| {
        let ws = s.ws("menu-scope").unwrap();
        invoke(
            menus::branch_entries(s, ws, Some("web"), cx),
            "Open pull request",
            w,
            cx,
        );
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/o/web/pull/22")
    );

    let api = home.join("shark/workspaces/menu-scope/api");
    git::git(
        &api,
        &["remote", "add", "origin", "https://github.com/o/api.git"],
    )
    .unwrap();
    // Keep the GitHub URL for links, but forbid network transport in this fixture.
    git::git(&api, &["config", "protocol.https.allow", "never"]).unwrap();
    git::git(
        &api,
        &["update-ref", "refs/remotes/origin/menu-scope", "HEAD"],
    )
    .unwrap();
    git::git(&api, &["checkout", "-qb", "actual-api"]).unwrap();
    let refresh = |cx: &mut TestAppContext| {
        on(cx, &|s, _, cx| {
            let ws = s.ws("menu-scope").unwrap().clone();
            s.syncs["menu-scope"].update(cx, |g, cx| g.set_workspace(ws, cx));
        })
    };
    refresh(cx);
    until(cx, window, |_, cx| {
        let s = shark.read(cx);
        let ws = s.ws("menu-scope").unwrap();
        menus::repo_branch(s, ws, &ws.repos[0], cx).as_deref() == Some("actual-api")
    })
    .await;
    on(cx, &|s, w, cx| {
        let ws = s.ws("menu-scope").unwrap();
        let entries = menus::branch_entries(s, ws, Some("api"), cx);
        assert!(entries.iter().any(|e| matches!(e,
            Entry::Item { label, hint: Some(hint), .. }
                if label == "Change base…" && hint.as_ref() == format!("{} ← menu-scope", ws.repos[0].base)
        )));
        invoke(entries, "Copy branch name", w, cx);
    });
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text())),
        Some("actual-api".into())
    );

    on(cx, &|s, w, cx| {
        let ws = s.ws("menu-scope").unwrap();
        invoke(
            menus::branch_entries(s, ws, Some("api"), cx),
            "Open attached branch “menu-scope” on GitHub",
            w,
            cx,
        );
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/o/api/tree/menu-scope")
    );

    git::git(&api, &["checkout", "--detach", "HEAD"]).unwrap();
    refresh(cx);
    until(cx, window, |_, cx| {
        let s = shark.read(cx);
        let ws = s.ws("menu-scope").unwrap();
        menus::repo_branch(s, ws, &ws.repos[0], cx).is_none()
    })
    .await;
    on(cx, &|s, _, cx| {
        let ws = s.ws("menu-scope").unwrap();
        assert!(!has(
            &menus::branch_entries(s, ws, Some("api"), cx),
            "Copy branch name"
        ));
        assert!(menus::branch_entries(s, ws, Some("gone"), cx).is_empty());
        assert!(has(
            &menus::branch_entries(s, ws, None, cx),
            "New agent on top…"
        ));
    });

    fs::rename(&api, api.with_file_name("api-away")).unwrap();
    refresh(cx);
    until(cx, window, |_, cx| {
        let s = shark.read(cx);
        let ws = s.ws("menu-scope").unwrap();
        menus::repo_branch(s, ws, &ws.repos[0], cx).as_deref() == Some("menu-scope")
    })
    .await;
    fs::remove_dir_all(&home).ok();
}
