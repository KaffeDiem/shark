//! A branch renamed outside Shark (`git branch -m` by an agent or in a terminal) is adopted by
//! the regular status check. A checkout of another branch is not a rename.
use super::*;

/// A task `name` on branch `old` over `repos`, booted and opened, its polling stopped so each
/// test runs checks by hand.
fn task(
    cx: &mut TestAppContext,
    name: &str,
    repos: &[&str],
) -> (PathBuf, Entity<Shark>, Entity<crate::sync::GitSync>) {
    let home = test_home();
    let repos = super::repos(&home, repos);
    let dir = home.join("shark/workspaces").join(name);
    let ws = git::create_task(&dir, name, "old", &git::on_branch(&repos, "old")).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos,
            tasks: vec![ws],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_task(0, w, cx);
        s.syncs[name].update(cx, |sync, cx| sync.set_active(false, cx));
    });
    let sync = cx.update(|cx| shark.read(cx).syncs[name].clone());
    (dir, shark, sync)
}

fn check(cx: &mut TestAppContext, sync: &Entity<crate::sync::GitSync>) -> String {
    sync.update(cx, |s, cx| s.check_now(cx));
    cx.run_until_parked();
    sync.update(cx, |s, cx| s.check_now(cx));
    cx.run_until_parked();
    cx.update(|cx| sync.read(cx).action.as_ref().unwrap().label.clone())
}

fn saved_branch() -> String {
    State::load().tasks[0].branch.clone()
}

#[gpui_kit::test]
fn renamed_branch_is_adopted(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, shark, sync) = task(cx, "rename", &["api", "web"]);
    for repo in ["api", "web"] {
        git::git(&dir.join(repo), &["branch", "-m", "new"]).unwrap();
    }

    let label = check(cx, &sync);

    assert!(!label.starts_with("Switch to branch"), "{label}");
    assert_eq!(saved_branch(), "new");
    cx.update(|cx| assert_eq!(shark.read(cx).task("rename").unwrap().branch, "new"));
    for file in ["AGENTS.md", "CLAUDE.md"] {
        let contents = fs::read_to_string(dir.join(file)).unwrap();
        assert!(contents.contains("branch `new`"), "{contents}");
        assert!(!contents.contains("branch `old`"), "{contents}");
    }
}

#[gpui_kit::test]
fn checkout_of_another_branch_is_not_a_rename(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, _, sync) = task(cx, "checkout", &["api"]);
    git::git(&dir.join("api"), &["switch", "-q", "-c", "other"]).unwrap();

    assert_eq!(check(cx, &sync), "Switch to branch");
    assert_eq!(saved_branch(), "old");
}

#[gpui_kit::test]
fn repos_that_disagree_are_not_a_rename(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (dir, _, sync) = task(cx, "partial", &["api", "web"]);
    git::git(&dir.join("api"), &["branch", "-m", "new"]).unwrap();

    assert_eq!(check(cx, &sync), "Switch to branch · api");
    assert_eq!(saved_branch(), "old");

    // Both renamed, to different names: still no single new name to adopt.
    git::git(&dir.join("web"), &["branch", "-m", "other"]).unwrap();
    assert!(check(cx, &sync).starts_with("Switch to branch"));
    assert_eq!(saved_branch(), "old");
}
