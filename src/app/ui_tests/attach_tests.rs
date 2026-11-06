use super::*;
use crate::agent::{Ev, Task};
use crate::attention::Attn;
use crate::chat::Chat;
use crate::state::{AttachStage, Attachment, ChatMeta, RepoSettings};

/// Wrapped so a test can't forget what the guards need: the lock and the quiet claude.
macro_rules! guards {
    ($quiet:ident, $turn:ident, $claude:ident) => {
        let $quiet = quiet_claude();
        let $turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let $claude = ClaudeGuard::set(&$quiet);
    };
}

pub(super) fn ws_of(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Workspace {
    cx.update(|cx| shark.read(cx).active_ws().unwrap().clone())
}

pub(super) fn chat_of(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Entity<Chat> {
    cx.update(|cx| shark.read(cx).active_chat().unwrap().clone())
}

/// The app with `repos` registered and a fresh agent open.
pub(super) fn agent(
    cx: &mut TestAppContext,
    home: &std::path::Path,
    names: &[&str],
    settings: &[(&str, &str)],
) -> (AnyWindowHandle, Entity<Shark>, Vec<Repo>) {
    let repos = repos(home, names);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            repo_settings: (settings.iter())
                .map(|(r, s)| {
                    let setup = vec![s.to_string()];
                    (
                        r.to_string(),
                        RepoSettings {
                            setup,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    press_in(window)(cx, &["cmd-n"]);
    (window, shark, repos)
}

fn fresh(r: &Repo, branch: &str) -> Attachment {
    Attachment::fresh(r, branch, "")
}

async fn settled(cx: &mut TestAppContext, window: AnyWindowHandle, shark: &Entity<Shark>) {
    let shark = shark.clone();
    until(cx, window, move |_, cx| {
        let s = shark.read(cx);
        s.adding_repos.is_empty()
            && s.setting_up.is_empty()
            && (s.active_ws().unwrap().attaching.iter())
                .all(|a| a.stuck() || a.stage == AttachStage::Queued)
    })
    .await;
    cx.run_until_parked();
}

pub(super) fn result() -> Ev {
    Ev::Result {
        is_error: false,
        text: "ok".into(),
        secs: 1,
    }
}

fn stage(cx: &mut TestAppContext, shark: &Entity<Shark>) -> AttachStage {
    ws_of(cx, shark).attaching[0].stage
}

/// A first repo takes the branch from the title; a later one inherits it, unless overridden, and
/// a base can be picked. The intent is on disk before Git starts.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branches_and_bases_of_first_and_later_repos(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web", "docs"], &[]);
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    on(cx, &|s, _, cx| {
        s.set_title(&name, "Fix: the Login page!".into(), cx)
    });
    let suffix = name.strip_prefix("agent-").unwrap();
    let branch = format!("fix-the-login-page-{suffix}");
    assert_eq!(ws_of(cx, &shark).default_branch(), branch);

    let first = fresh(&repos[0], &branch);
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![first.clone()], w, cx);
        // One update: idle, so it started, and it is saved, before Git has run.
        let saved = State::load();
        assert_eq!(
            saved.workspaces[0].attaching[0].stage,
            AttachStage::Creating
        );
        assert!(s.adding_repos.contains(&name));
        assert!(!ws.dir().join("api").exists());
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.branch, branch);
    assert_eq!(ws.repos[0].branch, None);
    assert_eq!(ws.repos[0].base, "main");
    assert!(ws.ready());
    assert_eq!(ws.default_branch(), branch, "later repos inherit it");

    // Overridden, on another base.
    let web = &repos[1].path;
    git::git(web, &["checkout", "-q", "-b", "develop"]).unwrap();
    let tip = commit_staged(web, "develop");
    git::git(web, &["checkout", "-q", "main"]).unwrap();
    let second = Attachment::fresh(&repos[1], "my/web", "develop");
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![second.clone()], w, cx)
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.branch, branch, "the agent's stays");
    assert_eq!(ws.repos[1].branch.as_deref(), Some("my/web"));
    assert_eq!(ws.repos[1].base, "develop");
    let wt = ws.dir().join("web");
    assert_eq!(git::git(&wt, &["rev-parse", "HEAD"]).unwrap(), tip);
    let docs = fs::read_to_string(ws.dir().join("AGENTS.md")).unwrap();
    assert!(
        docs.contains("on `my/web`") && docs.contains("base `develop`"),
        "{docs}"
    );
    let saved = State::load();
    assert_eq!(saved.workspaces[0].repos.len(), 2);
    assert!(saved.workspaces[0].attaching.is_empty());
    let docs_repo = fresh(&repos[2], &ws.default_branch());
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![docs_repo.clone()], w, cx)
    });
    settled(cx, window, &shark).await;
    assert_eq!(ws_of(cx, &shark).repos[2].branch, None, "the inherited one");
}

/// The dialog shows the branch and each repo's base before anything happens: tab reaches the
/// list, cmd-b a repo's branches, alt-enter takes one as base, and cmd-enter confirms.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_dialog_picks_branch_and_base_from_the_keyboard(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    git::git(&repos[0].path, &["branch", "develop"]).unwrap();
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let press = press_in(window);
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    assert!(ws_of(cx, &shark).attaching.is_empty(), "nothing yet");
    press(cx, &["tab", "cmd-b"]);
    cx.run_until_parked();
    press(cx, &["down"]);
    // develop comes after the agent's New branch row and main, in commit-date order.
    let list = git::branches(&repos[0].path).unwrap();
    let pick = list.iter().position(|b| b.name == "develop").unwrap();
    for _ in 0..pick {
        press(cx, &["down"]);
    }
    press(cx, &["alt-enter"]);
    press(cx, &["cmd-enter"]);
    settled(cx, window, &shark).await;
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.repos[0].base, "develop");
    assert_eq!(ws.branch, ws.name, "New agent's default: its identity");
}

/// A taken branch name stops before anything is made: told in the dialog, and again by the
/// operation, as one can appear while it waits. Only the name is refused, nothing is touched.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn collisions_leave_everything_alone(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let src = repos[0].path.clone();
    git::git(&src, &["branch", "taken"]).unwrap();
    let tip = git::git(&src, &["rev-parse", "taken"]).unwrap();
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "taken")], w, cx)
    });
    settled(cx, window, &shark).await;
    let after = ws_of(cx, &shark);
    assert!(after.repos.is_empty() && after.attaching.is_empty());
    assert!(!ws.dir().join("api").exists());
    assert_eq!(git::git(&src, &["rev-parse", "taken"]).unwrap(), tip);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.attach_needs(&name).unwrap(), "Couldn't attach api");
        assert!(matches!(s.attention_of(&name, cx), Attn::Needs(_)));
    });
    let chat = chat_of(cx, &shark);
    cx.update(|cx| assert!(!chat.read(cx).attach_hold, "a failure holds nothing"));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("attach-chip-api").is_some(),
            "shown in the header"
        );
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).attach_needs(&name), None, "dismissed"));

    // The dialog says so first, and stays open.
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    press_in(window)(cx, &["cmd-a"]);
    cx.update_window(window, |_, w, cx| w.input("taken", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    assert!(ws_of(cx, &shark).attaching.is_empty());
}

/// Three repos run in order; the one that can't attach is reported, the others attach, and the
/// hold ends with the operation.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn several_repos_run_in_turn_and_report_partial_success(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web", "docs"], &[]);
    git::git(&repos[1].path, &["branch", "both"]).unwrap();
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let chat = chat_of(cx, &shark);
    on(cx, &|s, w, cx| {
        let picks = repos.iter().map(|r| fresh(r, "both")).collect();
        s.queue_attachment(name.clone(), picks, w, cx);
        let stages: Vec<_> = (s.ws(&name).unwrap().attaching.iter())
            .map(|a| a.stage)
            .collect();
        use AttachStage::*;
        assert_eq!(stages, [Creating, Queued, Queued], "one at a time");
        assert!(chat.read(cx).attach_hold);
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    let attached: Vec<&str> = ws.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(attached, ["api", "docs"]);
    assert!(ws.ready());
    cx.update(|cx| {
        assert!(!chat.read(cx).attach_hold);
        assert_eq!(
            shark.read(cx).attach_needs(&name).unwrap(),
            "Couldn't attach web"
        );
    });
    // Refused while one is pending.
    on(cx, &|s, w, cx| {
        s.ws_mut(&name).unwrap().attaching = vec![fresh(&repos[1], "x")];
        s.queue_attachment(name.clone(), vec![fresh(&repos[1], "y")], w, cx);
        assert_eq!(s.ws(&name).unwrap().attaching[0].branch, "x");
        s.ws_mut(&name).unwrap().attaching.clear();
    });
}

#[test]
fn the_report_says_what_attached_and_what_did_not() {
    use super::super::attach::report;
    let failed = [("web".to_string(), "taken".to_string())];
    assert_eq!(
        report("T", "api, docs", &failed, &[]).unwrap(),
        (false, "Attached api, docs to T.\nweb failed: taken".into())
    );
    assert_eq!(
        report("T", "api", &[], &[]).unwrap(),
        (true, "Attached api to T.".into())
    );
    let stuck = ["cli".to_string()];
    assert_eq!(
        report("T", "api", &[], &stuck).unwrap(),
        (false, "Attached api to T.\nWaiting on you: cli".into())
    );
    assert_eq!(report("T", "", &[], &stuck), None, "its setup failure told");
}

/// A failed setup keeps the worktree, runs only the new repo's scripts, and holds everything
/// until the user retries; a retry that works ends it and sends what waited.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn setup_failure_keeps_the_worktree_until_retry(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let marker = home.join("web-setup-ran");
    let web = format!("touch {}", marker.display());
    let (window, shark, repos) = agent(
        cx,
        &home,
        &["api", "web"],
        &[("api", "echo broken; exit 3"), ("web", &web)],
    );
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    let chat = chat_of(cx, &shark);
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    settled(cx, window, &shark).await;
    let failed = ws_of(cx, &shark);
    assert_eq!(failed.repos.len(), 1, "metadata saved");
    assert!(!failed.ready(), "not ready");
    assert_eq!(failed.attaching[0].stage, AttachStage::SetupFailed);
    assert!(
        failed.attaching[0]
            .error
            .as_ref()
            .unwrap()
            .contains("failed")
    );
    assert!(failed.dir().join("api").exists(), "the worktree stays");
    assert!(!marker.exists(), "the other repo's scripts didn't run");
    let saved = State::load();
    assert_eq!(
        saved.workspaces[0].attaching[0].stage,
        AttachStage::SetupFailed
    );
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.attention_of(&name, cx),
            Attn::Needs("Setup failed: api".into())
        );
        assert!(chat.read(cx).attach_hold);
        assert!(
            s.syncs[&name].read(cx).blocked(false, cx).is_some(),
            "git waits too"
        );
    });
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            assert!(c.enqueue("after setup", vec![], cx));
            assert_eq!(c.queued(), 1);
        })
    });

    // Fixed, retried: only now is it ready, and what waited goes out.
    on(cx, &|s, w, cx| {
        s.state.repo_settings.get_mut("api").unwrap().setup = vec!["true".into()];
        s.attachment_menu(name.clone(), "api".into(), w, cx);
    });
    press_in(window)(cx, &["down", "enter"]);
    settled(cx, window, &shark).await;
    assert!(ws_of(cx, &shark).ready());
    cx.update(|cx| {
        let c = chat.read(cx);
        assert!(!c.attach_hold);
        assert_eq!(c.sent_texts(), ["after setup"]);
        assert_eq!(shark.read(cx).attention_of(&name, cx), Attn::None);
    });
}

/// Dismissing a failure that was already reported doesn't upset the next report.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn finishing_after_a_dismissed_failure_reports_cleanly(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[("api", "exit 3")]);
    git::git(&repos[1].path, &["branch", "b"]).unwrap();
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    on(cx, &|s, w, cx| {
        let picks = vec![fresh(&repos[0], "b"), fresh(&repos[1], "b")];
        s.queue_attachment(name.clone(), picks, w, cx)
    });
    settled(cx, window, &shark).await;
    assert_eq!(stage(cx, &shark), AttachStage::SetupFailed);
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "web".into(), w, cx)
    });
    press_in(window)(cx, &["enter"]);
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["down", "down", "enter"]);
    assert!(ws_of(cx, &shark).ready());
}

/// The chip's Retry stays out of a worktree being deleted.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn retry_refuses_during_deletion(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let marker = home.join("setup-ran");
    let setup = format!("touch {}; exit 3", marker.display());
    let (window, shark, repos) = agent(cx, &home, &["api"], &[("api", &setup)]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    settled(cx, window, &shark).await;
    fs::remove_file(&marker).unwrap();
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "api".into(), w, cx);
        s.archiving.insert(name.clone());
    });
    press_in(window)(cx, &["down", "enter"]);
    cx.run_until_parked();
    assert!(!marker.exists(), "no script started");
    assert_eq!(stage(cx, &shark), AttachStage::SetupFailed);
    cx.update(|cx| assert!(shark.read(cx).setting_up.is_empty()));
}

/// Confirming a delete asks again: a message queued behind a Git step goes out while the dialog
/// is open, and the worktree must not go from under its turn. Uncommitted work still stops a
/// delete on this path, and an unpushed branch outlives one.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn delete_rechecks_after_the_dialog(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    settled(cx, window, &shark).await;
    let chat = chat_of(cx, &shark);
    let wt = ws.dir().join("api");
    let step = |cx: &mut TestAppContext, label: Option<&'static str>| {
        on(cx, &|s, _, cx| {
            s.syncs[&name].update(cx, |g, cx| g.play_step(label, cx));
        })
    };
    let deleting = |cx: &mut TestAppContext| {
        on(cx, &|s, w, cx| s.archive_ws(name.clone(), false, w, cx));
    };
    step(cx, Some("Pull"));
    cx.update(|cx| chat.update(cx, |c, cx| assert!(c.enqueue("after pull", vec![], cx))));
    deleting(cx);
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    step(cx, None);
    cx.update(|cx| assert!(chat.read(cx).busy, "it went out behind the dialog"));
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        assert!(shark.read(cx).archiving.is_empty(), "refused");
        assert!(!chat.read(cx).archiving);
    });
    assert!(wt.exists());
    cx.update(|cx| chat.update(cx, |c, cx| c.play(result(), cx)));

    // A Git step started behind the dialog counts too.
    deleting(cx);
    step(cx, Some("Pull"));
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| assert!(shark.read(cx).archiving.is_empty(), "refused"));
    step(cx, None);

    fs::write(wt.join("dirty.txt"), "unsaved").unwrap();
    deleting(cx);
    press_in(window)(cx, &["enter"]);
    let waiting = shark.clone();
    until(cx, window, move |_, cx| {
        waiting.read(cx).archiving.is_empty()
    })
    .await;
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(wt.join("dirty.txt")).unwrap(), "unsaved");
    cx.update(|cx| assert_eq!(shark.read(cx).state.workspaces.len(), 1, "kept"));

    let tip = commit_all(&wt, "unpushed");
    deleting(cx);
    press_in(window)(cx, &["enter"]);
    let gone = shark.clone();
    until(cx, window, move |_, cx| {
        gone.read(cx).state.workspaces.is_empty()
    })
    .await;
    assert!(!wt.exists());
    assert_eq!(git::git(&repos[0].path, &["rev-parse", "b"]).unwrap(), tip);
}

/// Continuing past a failed setup is the user's call, and ends the hold; the worktree stays.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn continuing_without_setup_resolves_it(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[("api", "exit 3")]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    on(cx, &|s, w, cx| {
        let picks = vec![fresh(&repos[0], "b"), fresh(&repos[1], "b")];
        s.queue_attachment(name.clone(), picks, w, cx)
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.repos.len(), 2, "the next repo doesn't wait on it");
    assert_eq!(ws.attaching.len(), 1);
    assert!(!ws.ready());
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    press_in(window)(cx, &["down", "down", "enter"]);
    let ws = ws_of(cx, &shark);
    assert!(ws.ready() && ws.dir().join("api").exists());
    assert!(State::load().workspaces[0].attaching.is_empty());
}

/// Cancelling while queued takes it back with nothing made; once creation starts it can't.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cancel_works_only_while_queued(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    let chat = chat_of(cx, &shark);
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    assert_eq!(stage(cx, &shark), AttachStage::Queued, "a turn runs");
    assert_eq!(State::load().workspaces[0].attaching.len(), 1, "saved");
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["enter"]);
    assert!(ws_of(cx, &shark).attaching.is_empty());
    assert!(State::load().workspaces[0].attaching.is_empty());
    assert!(!ws.dir().join("api").exists());
    assert!(!git::has_ref(&repos[0].path, "refs/heads/b"));
    cx.update(|cx| {
        assert!(!chat.read(cx).attach_hold);
        chat.update(cx, |c, cx| {
            c.busy = false;
            cx.notify();
        })
    });
    cx.run_until_parked();
    assert!(!ws.dir().join("api").exists(), "and it stays cancelled");

    // Started: nothing to cancel, and the worktree comes.
    on(cx, &|s, w, cx| {
        let picks = vec![fresh(&repos[0], "b"), fresh(&repos[1], "b")];
        s.queue_attachment(name.clone(), picks, w, cx);
        s.cancel_attachment(&name, w, cx);
        let left = &s.ws(&name).unwrap().attaching;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].stage, AttachStage::Creating);
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.repos.len(), 1);
    assert!(ws.ready() && ws.dir().join("api").exists());
    assert!(!ws.dir().join("web").exists(), "the queued one went");
}

/// A save that fails as the queued repo's turn comes aborts it before Git starts.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_failed_save_stops_creation_before_it_starts(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    let chat = chat_of(cx, &shark);
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    let state = crate::state::support_dir().join("state.json");
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.busy = false;
            cx.notify();
        })
    });
    cx.run_until_parked();
    fs::remove_dir(&state).unwrap();
    assert!(ws_of(cx, &shark).attaching.is_empty());
    assert!(!ws.dir().join("api").exists());
    assert!(!git::has_ref(&repos[0].path, "refs/heads/b"));
    cx.update(|cx| assert!(!chat.read(cx).attach_hold));
}

/// Attachment waits for each thing that may still touch the agent, in any chat, and starts on the
/// update that frees it, with no timer: a turn, a question, background tasks, an unread steer, a
/// git step.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn attachment_starts_when_the_last_work_ends(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web", "docs", "cli", "ops"], &[]);
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    let chat = chat_of(cx, &shark);
    on(cx, &|s, w, cx| s.new_chat(w, cx));
    let other = chat_of(cx, &shark);
    let task = Task {
        id: "t".into(),
        kind: "shell".into(),
        description: "x".into(),
    };
    let queue = |cx: &mut TestAppContext, r: &Repo| {
        on(cx, &|s, w, cx| {
            s.queue_attachment(name.clone(), vec![fresh(r, "b")], w, cx)
        });
        assert_eq!(stage(cx, &shark), AttachStage::Queued);
    };
    let wait = |cx: &mut TestAppContext, n: usize| {
        cx.run_until_parked();
        let ws = ws_of(cx, &shark);
        assert_eq!(ws.repos.len(), n, "{:?}", ws.attaching);
    };
    // A running turn, in the chat that isn't shown.
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    queue(cx, &repos[0]);
    // A second chat going idle isn't enough.
    cx.update(|cx| other.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
    assert_eq!(stage(cx, &shark), AttachStage::Queued);
    cx.update(|cx| chat.update(cx, |c, cx| c.play(result(), cx)));
    settled(cx, window, &shark).await;
    wait(cx, 1);

    // A pending question: the turn holds, and answering it finishes the turn.
    let asked = Ev::Permission {
        request_id: "r".into(),
        tool: "AskUserQuestion".into(),
        tool_use_id: "u".into(),
        input: serde_json::json!({"questions": [{"question": "Which?", "options": [{"label": "A"}]}]}),
    };
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.busy = true;
            c.play(asked, cx);
            assert!(c.asking());
        })
    });
    queue(cx, &repos[1]);
    cx.update(|cx| chat.update(cx, |c, cx| c.play(result(), cx)));
    settled(cx, window, &shark).await;
    wait(cx, 2);

    // Background tasks outlive the turn.
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.play(Ev::Background(vec![task.clone()]), cx);
            assert!(!c.busy);
        })
    });
    queue(cx, &repos[2]);
    cx.update(|cx| chat.update(cx, |c, cx| c.play(Ev::Background(vec![]), cx)));
    settled(cx, window, &shark).await;
    wait(cx, 3);

    // A message the ended turn never read starts a turn of its own.
    cx.update(|cx| chat.update(cx, |c, _| c.steered = true));
    queue(cx, &repos[3]);
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.steered = false;
            cx.notify();
        })
    });
    settled(cx, window, &shark).await;
    wait(cx, 4);

    // A git step finishes first; the attachment hears it end.
    on(cx, &|s, _, cx| {
        s.syncs[&name].update(cx, |g, cx| g.play_step(Some("Pull"), cx));
    });
    queue(cx, &repos[4]);
    on(cx, &|s, _, cx| {
        s.syncs[&name].update(cx, |g, cx| g.play_step(None, cx));
    });
    settled(cx, window, &shark).await;
    wait(cx, 5);
    assert!(ws.dir().join("ops").exists());
}

/// A Git hold and an attachment hold release independently, whichever ends first; the queue
/// goes out only when both have.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_and_attachment_holds_end_separately(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let chat = chat_of(cx, &shark);
    let step = |cx: &mut TestAppContext, label: Option<&'static str>| {
        on(cx, &|s, _, cx| {
            s.syncs[&name].update(cx, |g, cx| g.play_step(label, cx));
        })
    };
    let sent = |cx: &mut TestAppContext| cx.update(|cx| chat.read(cx).sent_texts());

    // The Git hold ends first, while the attachment is the one that's left.
    step(cx, Some("Pull"));
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            assert!(c.hold && c.attach_hold);
            assert!(c.enqueue("first", vec![], cx));
        })
    });
    // Blocking the next: the attachment waits on the step, then stays queued on this.
    on(cx, &|s, _, cx| {
        let c = s.chats[&name].clone();
        c.update(cx, |c, _| c.busy = true);
    });
    step(cx, None);
    cx.update(|cx| {
        let c = chat.read(cx);
        assert!(
            !c.hold && c.attach_hold,
            "git ending leaves the attachment's"
        );
        assert_eq!(c.queued(), 1);
    });
    assert_eq!(stage(cx, &shark), AttachStage::Queued);

    // The attachment ends first: cancelled under a new Git step, which keeps its own hold.
    step(cx, Some("Pull"));
    cx.update(|cx| chat.update(cx, |c, _| c.busy = false));
    on(cx, &|s, w, cx| s.cancel_attachment(&name, w, cx));
    cx.update(|cx| {
        let c = chat.read(cx);
        assert!(
            c.hold && !c.attach_hold,
            "the attachment ending leaves git's"
        );
        assert_eq!(c.queued(), 1);
    });
    assert!(sent(cx).is_empty());
    step(cx, None);
    assert_eq!(sent(cx), ["first"]);
}

/// While an attachment is pending: a new chat and a reopened one inherit its hold, a chat's Plan
/// mode stays as it was, and it all lifts with the operation.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn chats_opened_during_attachment_inherit_the_hold(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    settled(cx, window, &shark).await;
    let chat = chat_of(cx, &shark);
    // A second chat, out of Plan mode, closed with a session to come back to.
    on(cx, &|s, w, cx| {
        s.new_chat(w, cx);
        let ws = s.active_ws_mut().unwrap();
        ws.chats[1].session_id = Some("s2".into());
        s.close_chat(w, cx);
    });
    assert_eq!(ws_of(cx, &shark).closed.len(), 1);
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[1], "b")], w, cx);
        s.reopen_chat(None, w, cx);
    });
    let reopened = chat_of(cx, &shark);
    cx.update(|cx| {
        let c = reopened.read(cx);
        assert!(c.attach_hold, "reopened");
        assert!(!c.meta.plan, "a later attachment leaves its mode alone");
    });
    on(cx, &|s, w, cx| s.new_chat(w, cx));
    let new = chat_of(cx, &shark);
    cx.update(|cx| assert!(new.read(cx).attach_hold, "new"));
    cx.update_window(window, |_, w, cx| w.input("typed meanwhile", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        let c = new.read(cx);
        assert_eq!(c.queued(), 1);
        assert!(c.sent_texts().is_empty());
        let s = shark.read(cx);
        assert!(s.syncs[&name].read(cx).blocked(false, cx).is_some());
    });
    cx.update(|cx| chat.update(cx, |c, cx| c.play(result(), cx)));
    settled(cx, window, &shark).await;
    cx.update(|cx| {
        for c in [&chat, &reopened, &new] {
            assert!(!c.read(cx).attach_hold);
        }
        assert_eq!(new.read(cx).sent_texts(), ["typed meanwhile"]);
    });
}

/// `name`'s directory and an `attaching` record at `stage`; with `made`, its worktree on `branch`
/// exists too, and with `saved`, `Workspace::repos` knows it.
fn interrupted(name: &str, repo: &Repo, stage: AttachStage, made: bool, saved: bool) -> Workspace {
    let dir = workspaces_dir().join(name);
    let empty = git::create_empty_workspace(&dir, name, name).unwrap();
    let branch = format!("{name}-branch");
    let mut ws = match made {
        true => git::add_to_workspace(&empty, &dir, repo, &branch, Some("")).unwrap(),
        false => empty.clone(),
    };
    if !saved {
        (ws.repos, ws.branch) = (vec![], empty.branch);
    }
    ws.attaching = vec![Attachment {
        stage,
        ..fresh(repo, &branch)
    }];
    ws
}

/// A restart reconciles each recorded state against repo, branch and path: finished work is
/// adopted, untouched intent cleared, the rest kept and shown. No script reruns by itself and
/// nothing uncertain goes, not even with the agent's deletion.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn restart_reconciles_every_stage(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let (api, web) = (&repos[0], &repos[1]);
    let marker = home.join("setup-ran");
    use AttachStage::*;
    let mut gone = interrupted("gone", api, Setup, true, true);
    fs::remove_dir_all(gone.dir().join("api")).unwrap();
    gone.attaching[0].stage = Setup;
    let branch_only = interrupted("branch-only", api, Creating, false, false);
    git::git(&api.path, &["branch", "branch-only-branch"]).unwrap();
    let foreign = interrupted("foreign", api, Creating, false, false);
    fs::create_dir(foreign.dir().join("api")).unwrap();
    fs::write(foreign.dir().join("api/mine.txt"), "keep").unwrap();
    let mut failed = interrupted("failed", web, SetupFailed, true, true);
    failed.attaching[0].error = Some("exit 3".into());
    let workspaces = vec![
        interrupted("queued", api, Queued, false, false),
        interrupted("untouched", api, Creating, false, false),
        interrupted("finished", api, Creating, true, false),
        interrupted("finished-scripts", web, Creating, true, false),
        interrupted("mid-setup", web, Setup, true, true),
        interrupted("mid-no-setup", api, Setup, true, true),
        failed,
        gone,
        branch_only,
        foreign,
    ];
    let setup = vec![format!("touch {}", marker.display())];
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            repo_settings: [(
                "web".to_string(),
                RepoSettings {
                    setup,
                    ..Default::default()
                },
            )]
            .into(),
            workspaces,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let on = on_app(window, shark.clone());
    let attempt = |cx: &mut TestAppContext, n: &str| {
        cx.update(|cx| {
            let ws = shark.read(cx).ws(n).unwrap();
            let a = ws.attaching.first();
            (ws.repos.len(), a.map(|a| a.stage), ws.ready())
        })
    };
    let needs =
        |cx: &mut TestAppContext, n: &str| cx.update(|cx| shark.read(cx).attention_of(n, cx));
    assert_eq!(attempt(cx, "queued"), (0, None, false), "cleared");
    assert_eq!(attempt(cx, "untouched"), (0, None, false), "cleared");
    assert_eq!(attempt(cx, "finished"), (1, None, true), "adopted");
    let adopted = cx.update(|cx| shark.read(cx).ws("finished").unwrap().clone());
    assert_eq!(adopted.branch, "finished-branch");
    assert_eq!(adopted.repos[0].base, "main");
    assert_eq!(
        attempt(cx, "mid-no-setup"),
        (1, None, true),
        "nothing left to run"
    );
    for n in ["finished-scripts", "mid-setup"] {
        assert_eq!(attempt(cx, n), (1, Some(SetupFailed), false), "{n}");
        assert_eq!(needs(cx, n), Attn::Needs("Setup failed: web".into()));
    }
    assert!(!marker.exists(), "no script reran");
    assert_eq!(attempt(cx, "failed"), (1, Some(SetupFailed), false));
    let kept = cx.update(|cx| shark.read(cx).ws("failed").unwrap().clone());
    assert_eq!(kept.attaching[0].error.as_deref(), Some("exit 3"));
    assert_eq!(attempt(cx, "gone"), (1, Some(Uncertain), false));
    assert_eq!(attempt(cx, "branch-only"), (0, Some(Uncertain), false));
    assert_eq!(attempt(cx, "foreign"), (0, Some(Uncertain), false));
    assert_eq!(needs(cx, "foreign"), Attn::Needs("Recover api".into()));
    assert!(git::has_ref(&api.path, "refs/heads/branch-only-branch"));
    let saved = State::load();
    let saved = saved
        .workspaces
        .iter()
        .find(|w| w.name == "finished")
        .unwrap();
    assert!(
        saved.attaching.is_empty() && saved.repos.len() == 1,
        "saved"
    );

    // Shown in the header, holding the chats; deletion leaves it alone.
    on(cx, &|s, w, cx| s.open_workspace_named("foreign", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("attach-chip-api").is_some());
    })
    .unwrap();
    let chat = chat_of(cx, &shark);
    cx.update(|cx| assert!(chat.read(cx).attach_hold));
    on(cx, &|s, w, cx| {
        s.remove_workspace("foreign", true, w, cx);
        assert!(s.archiving.is_empty(), "refused");
    });
    // Stopping tracking deletes nothing and lets go of the chats.
    on(cx, &|s, w, cx| {
        s.attachment_menu("foreign".into(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["down", "enter"]);
    assert_eq!(attempt(cx, "foreign"), (0, None, false));
    let dir = workspaces_dir().join("foreign");
    assert_eq!(
        fs::read_to_string(dir.join("api/mine.txt")).unwrap(),
        "keep"
    );
    cx.update(|cx| assert!(!chat.read(cx).attach_hold));

    // Sorted out by hand, Check again clears it.
    git::git(&api.path, &["branch", "-D", "branch-only-branch"]).unwrap();
    on(cx, &|s, w, cx| s.open_workspace_named("branch-only", w, cx));
    on(cx, &|s, w, cx| {
        s.attachment_menu("branch-only".into(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["enter"]);
    assert_eq!(attempt(cx, "branch-only"), (0, None, false));

    // Its worktree gone, stopping tracking drops the repo too: `ready` can't pass on nothing.
    on(cx, &|s, w, cx| s.open_workspace_named("gone", w, cx));
    on(cx, &|s, w, cx| {
        s.attachment_menu("gone".into(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["down", "enter"]);
    assert_eq!(attempt(cx, "gone"), (0, None, false));
    let dir = workspaces_dir().join("gone");
    let docs = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert!(docs.contains("No repositories"), "{docs}");
    let saved = State::load();
    let saved = saved.workspaces.iter().find(|w| w.name == "gone").unwrap();
    assert!(saved.repos.is_empty());
    let chat = chat_of(cx, &shark);
    cx.update(|cx| assert!(!chat.read(cx).ws.ready()));
}

/// A claude that keeps what it is sent, one JSON line per message, next to itself.
pub(super) fn recording_claude(quiet: &std::path::Path) -> PathBuf {
    let path = quiet.with_file_name("recording-claude");
    let _ = fs::remove_file(path.with_extension("in"));
    fs::write(&path, "#!/bin/sh\nexec cat >> \"${0%-claude}-claude.in\"\n").unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&path)
        .status()
        .unwrap();
    path
}

/// Message `n` as it reached the provider; the script runs in its own process, so it is waited
/// for in real time.
pub(super) fn wire(claude: &std::path::Path, n: usize) -> String {
    let path = claude.with_extension("in");
    for _ in 0..100 {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let messages = text.lines().filter(|l| l.starts_with(r#"{"type":"user""#));
        if let Some(line) = messages.into_iter().nth(n) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            return v["message"]["content"].as_str().unwrap().to_string();
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("no message {n}: {:?}", fs::read_to_string(path));
}

/// Context reaches the provider with the next normal turn: not at attach time, not on a
/// `/command`, not in the user's bubble; a failed start keeps it pending, an echo records it,
/// and a new session doesn't need it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn context_goes_with_the_next_normal_turn(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let claude = recording_claude(&quiet);
    let guard = ClaudeGuard::set(&claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let chat = chat_of(cx, &shark);
    let ask = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| {
            chat.update(cx, |c, cx| c.ask(text, w, cx))
        })
        .unwrap()
    };
    let play = |cx: &mut TestAppContext, ev: Ev| {
        cx.update(|cx| chat.update(cx, |c, cx| c.play(ev, cx)));
        cx.run_until_parked();
    };
    let pending = |cx: &mut TestAppContext| cx.update(|cx| chat.read(cx).pending_context().len());
    let init = || Ev::Init {
        session_id: "s1".into(),
        model: "m".into(),
        permission_mode: "plan".into(),
        mcp: vec![],
    };
    // A conversation from before any repo.
    assert!(ask(cx, "plan it"));
    assert_eq!(wire(&claude, 0), "plan it");
    play(cx, init());
    play(cx, Ev::User("plan it".into()));
    play(cx, result());

    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    settled(cx, window, &shark).await;
    assert_eq!(pending(cx), 1, "for the next turn");

    // A command stays a command, and the context waits.
    assert!(ask(cx, "/compact"));
    assert_eq!(wire(&claude, 1), "/compact");
    play(cx, Ev::User("/compact".into()));
    play(cx, result());
    assert_eq!(pending(cx), 1);

    assert!(ask(cx, "carry on"));
    let sent = wire(&claude, 2);
    assert!(sent.starts_with("[shark] Repo `api` is attached"), "{sent}");
    assert!(sent.ends_with("carry on"), "{sent}");
    cx.update(|cx| {
        assert_eq!(
            chat.read(cx).sent_texts().last().unwrap(),
            "carry on",
            "the user's bubble shows what they wrote"
        );
    });
    assert_eq!(pending(cx), 1, "unread until the agent echoes it");
    play(cx, init());
    play(cx, Ev::User(sent.clone()));
    assert_eq!(pending(cx), 0);
    let saved = State::load();
    assert_eq!(
        saved.workspaces[0].chats[0].delivered.len(),
        1,
        "survives a restart"
    );
    play(cx, result());
    assert!(ask(cx, "next"));
    assert_eq!(wire(&claude, 3), "next", "told once");
    play(cx, Ev::User("next".into()));
    play(cx, result());

    // A chat that can't start keeps it pending. (Its process dying is `chat::ui_tests`': a real
    // exit would trip the deterministic scheduler.)
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[1], "b")], w, cx);
        let id = format!("{name}~9");
        let ws = s.active_ws_mut().unwrap();
        ws.push_chat(ChatMeta {
            id,
            session_id: Some("s9".into()),
            ..Default::default()
        });
    });
    settled(cx, window, &shark).await;
    on(cx, &|s, w, cx| s.select_chat(1, w, cx));
    let resumed = chat_of(cx, &shark);
    let ask_resumed = |cx: &mut TestAppContext, text: &str| {
        let chat = resumed.clone();
        cx.update_window(window, |_, w, cx| {
            chat.update(cx, |c, cx| c.ask(text, w, cx))
        })
        .unwrap()
    };
    let dead = ClaudeGuard::set(std::path::Path::new("/nonexistent/claude"));
    assert!(!ask_resumed(cx, "failed"));
    cx.update(|cx| assert_eq!(resumed.read(cx).pending_context().len(), 2));
    drop(dead);
    assert!(ask_resumed(cx, "retried"));
    let sent = wire(&claude, 4);
    assert!(
        sent.contains("Repo `api`") && sent.contains("Repo `web`"),
        "{sent}"
    );
    assert!(sent.ends_with("retried"), "{sent}");

    // A new session reads the repos from the root docs.
    on(cx, &|s, w, cx| s.new_chat(w, cx));
    let new = chat_of(cx, &shark);
    cx.update_window(window, |_, w, cx| {
        new.update(cx, |c, cx| c.ask("hello", w, cx))
    })
    .unwrap();
    assert_eq!(wire(&claude, 5), "hello");
    cx.update(|cx| assert!(new.read(cx).pending_context().is_empty()));
    drop(guard);
}

/// Attaching reaches every chat of the agent without loading any: one with a stored session,
/// open or closed, hears of it when it next talks, also after a restart.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn unloaded_chats_catch_up_after_a_restart(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let (open_id, closed_id) = (format!("{name}~2"), format!("{name}~3"));
    on(cx, &|s, w, cx| {
        let ws = s.active_ws_mut().unwrap();
        let meta = |id: &String, s: &str| ChatMeta {
            id: id.clone(),
            session_id: Some(s.into()),
            ..Default::default()
        };
        ws.push_chat(meta(&open_id, "s2"));
        ws.closed.push(meta(&closed_id, "s3"));
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx);
    });
    settled(cx, window, &shark).await;
    cx.update(|cx| assert!(!shark.read(cx).chats.contains_key(&open_id), "not loaded"));

    let (window2, shark2) = open(cx, WindowOptions::default());
    cx.run_until_parked();
    let on2 = on_app(window2, shark2.clone());
    on2(cx, &|s, w, cx| {
        s.open_workspace_named(&name, w, cx);
        s.select_chat(1, w, cx);
    });
    let chat = chat_of(cx, &shark2);
    cx.update(|cx| {
        let c = chat.read(cx);
        assert_eq!(c.meta.id, open_id);
        assert_eq!(c.pending_context().len(), 1);
    });
    on2(cx, &|s, w, cx| s.reopen_chat(None, w, cx));
    let chat = chat_of(cx, &shark2);
    cx.update(|cx| {
        let c = chat.read(cx);
        assert_eq!(c.meta.id, closed_id);
        assert_eq!(c.pending_context().len(), 1);
    });
    cx.update_window(window2, |_, w, _| w.remove_window())
        .unwrap();
}

/// An agent's Diff and Git offers give way to the real views when its first repo lands, tab
/// shown or not, and everything reading the repo list has it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn empty_views_go_live_once_a_repo_attaches(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let on = on_app(window, shark.clone());
    let ws = ws_of(cx, &shark);
    let name = ws.name.clone();
    on(cx, &|s, w, cx| {
        s.show(Tab::Feature(Feature::Diff), w, cx);
        s.show(Tab::Feature(Feature::Git), w, cx);
        assert!(s.git_features.is_empty(), "only the offer so far");
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx);
    });
    settled(cx, window, &shark).await;
    let shown = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.try_find(id).is_some()
        })
        .unwrap()
    };
    assert!(!shown(cx, "attach-repository"));
    assert!(shown(cx, "repo-chip-api"));
    assert!(shown(cx, "header-attach"), "more can join");
    assert!(shown(cx, "branch"));
    on(cx, &|s, w, cx| {
        assert!(s.git_features.contains_key(&name), "the view it offered");
        let sync = s.syncs[&name].read(cx);
        assert_eq!(sync.branches()[0].repo, "api");
        assert!(sync.blocked(false, cx).is_none());
        let ws = s.active_ws().unwrap().clone();
        assert_eq!(s.effective_workspace(ws.clone(), cx).repos.len(), 1);
        assert_eq!(s.active_chat().unwrap().read(cx).ws.repos.len(), 1);
        s.show(Tab::Feature(Feature::Diff), w, cx);
    });
    assert!(!shown(cx, "attach-repository"));
    let docs = fs::read_to_string(ws.dir().join("CLAUDE.md")).unwrap();
    assert!(
        docs.contains("`api/`") && !docs.contains("No repositories"),
        "{docs}"
    );
}
