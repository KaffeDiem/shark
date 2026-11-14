//! `shark://open?url=…` from the browser extension (src/app/deep_link.rs).
use super::*;
use std::os::unix::fs::PermissionsExt;

const PR: &str = "https://github.com/acme/api/pull/7";

/// `api` on origin `github.com/acme/api`, served by a local bare repo with PR #7's head.
fn fixture(home: &std::path::Path) -> (Vec<Repo>, GhGuard) {
    let repos = repos(home, &["api"]);
    let src = &repos[0].path;
    let origin = home.join("api.git");
    git::git(home, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    git::git(
        src,
        &["remote", "add", "origin", "https://github.com/acme/api.git"],
    )
    .unwrap();
    let key = format!("url.{}.insteadOf", origin.display());
    git::git(src, &["config", &key, "https://github.com/acme/api.git"]).unwrap();
    git::git(src, &["push", "-q", "origin", "main"]).unwrap();
    git::git(src, &["checkout", "-qb", "feature"]).unwrap();
    commit_staged(src, "feature");
    git::git(
        src,
        &["push", "-q", "origin", "feature", "HEAD:refs/pull/7/head"],
    )
    .unwrap();
    git::git(src, &["checkout", "-q", "main"]).unwrap();
    let view = serde_json::json!({
        "number": 7, "title": "Add feature", "url": PR, "state": "OPEN",
        "headRefName": "feature", "baseRefName": "main", "isCrossRepository": false,
    });
    fs::write(home.join("view.json"), view.to_string()).unwrap();
    let fake = home.join("gh");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncase \"$*\" in *' view '*) cat '{}';; *) echo '[]';; esac\n",
            home.join("view.json").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let guard = GhGuard::set(&fake);
    (repos, guard)
}

fn open_link(cx: &mut TestAppContext, window: AnyWindowHandle, shark: &Entity<Shark>, link: &str) {
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.open_link(link, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

/// A link to a PR no task has starts a review task on it; the same link then opens that task.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn link_creates_then_reopens(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (repos, _gh) = fixture(&home);
    let (window, shark) = boot(
        cx,
        State {
            repos,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let link = format!("shark://open?url={PR}");
    open_link(cx, window, &shark, &link);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("pr-7")
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.tasks.len(), 1);
        assert_eq!(s.state.tasks[0].title, "Add feature");
        assert_eq!(s.state.tasks[0].repos[0].name, "api");
        assert_eq!(
            git::review_pr_url(&s.state.repos[0].path, "pr/7").as_deref(),
            Some(PR)
        );
    });
    // Away from it, the link comes back to it without creating another task.
    blank_task(cx, window, &shark);
    cx.update(|cx| assert_ne!(shark.read(cx).active.as_deref(), Some("pr-7")));
    open_link(cx, window, &shark, &link);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("pr-7")
    })
    .await;
    cx.update(|cx| assert_eq!(shark.read(cx).state.tasks.len(), 2));
    fs::remove_dir_all(&home).ok();
}

/// A task already working on the PR's own branch is the PR's task.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn link_opens_task_on_head_branch(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (repos, _gh) = fixture(&home);
    let task = Task {
        name: "own".into(),
        branch: "feature".into(),
        repos: vec![crate::state::TaskRepo {
            name: "api".into(),
            src: repos[0].path.clone(),
            base: "origin/main".into(),
            branch: None,
        }],
        ..Default::default()
    };
    let other = Task {
        name: "other".into(),
        branch: "other".into(),
        ..Default::default()
    };
    let state = State {
        repos,
        tasks: vec![other, task],
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.open_task_named("other", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    open_link(cx, window, &shark, &format!("shark://open?url={PR}"));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("own")
    })
    .await;
    cx.update(|cx| assert_eq!(shark.read(cx).state.tasks.len(), 2));
    fs::remove_dir_all(&home).ok();
}

/// Bad links and PRs of unregistered repositories say so and create nothing.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn link_errors_are_reported(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (repos, _gh) = fixture(&home);
    let (window, shark) = boot(
        cx,
        State {
            repos,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let toasts = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| w.notifications(cx).len())
            .unwrap()
    };
    for (i, link) in [
        "shark://open?url=https://github.com/acme/api",
        "shark://open?url=https://github.com/other/repo/pull/3",
    ]
    .iter()
    .enumerate()
    {
        open_link(cx, window, &shark, link);
        assert_eq!(toasts(cx), i + 1, "{link}");
    }
    cx.update(|cx| assert!(shark.read(cx).state.tasks.is_empty()));
    fs::remove_dir_all(&home).ok();
}
