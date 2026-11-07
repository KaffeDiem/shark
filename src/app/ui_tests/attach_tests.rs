use super::super::attach::{Fit, setup_tail};
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
    let branch = format!("fix-the-login-page-{name}");
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

/// The dialog shows the branch and base before anything happens: cmd-b opens Change, cmd-b again
/// the bases, enter takes one and comes back, and enter attaches. alt-enter stays out of it.
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
    press(cx, &["cmd-b", "tab", "down"]);
    press(cx, &["alt-enter"]);
    assert!(
        shows(cx, window, "source-back"),
        "no shortcut to a base here"
    );
    press(cx, &["cmd-b"]);
    cx.update_window(window, |_, w, cx| w.input("develop", cx))
        .unwrap();
    press(cx, &["enter"]);
    assert_eq!(summary(cx, window), format!("{name} from develop"));
    press(cx, &["enter"]);
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
    press_in(window)(cx, &["cmd-b", "cmd-a"]);
    cx.update_window(window, |_, w, cx| w.input("taken", cx))
        .unwrap();
    press_in(window)(cx, &["enter", "enter"]);
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    assert!(ws_of(cx, &shark).attaching.is_empty());
}

/// A failed inclusion scan leaves an owned worktree that rollback cannot prove safe to remove.
/// Its intent must survive the failure and a restart so the user can recover it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_rollback_keeps_attachment_recoverable(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    fs::create_dir(repos[0].path.join(".worktreeinclude")).unwrap();
    let name = ws_of(cx, &shark).name;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx)
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert!(ws.repos.is_empty());
    assert_eq!(ws.attaching[0].stage, AttachStage::Uncertain);
    assert!(
        ws.attaching[0]
            .error
            .as_ref()
            .unwrap()
            .contains("Preserved resources")
    );
    assert!(ws.dir().join("api/.git").exists());
    assert!(git::has_ref(&repos[0].path, "refs/heads/b"));
    assert_eq!(State::load().workspaces[0].attaching, ws.attaching);
    let chat = chat_of(cx, &shark);
    cx.update(|cx| assert!(chat.read(cx).attach_hold));

    let (window2, shark2) = open(cx, WindowOptions::default());
    cx.run_until_parked();
    cx.update(|cx| {
        let ws = shark2.read(cx).ws(&name).unwrap();
        assert_eq!(ws.attaching[0].stage, AttachStage::Uncertain);
        assert!(ws.dir().join("api/.git").exists());
    });
    cx.update_window(window2, |_, w, _| w.remove_window())
        .unwrap();
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
    let asked = crate::agent::parse(
        r#"{"type":"control_request","request_id":"r","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"u","input":{"questions":[{"question":"Which?","options":[{"label":"A"}]}]}}}"#,
    ).pop().unwrap();
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
    let switched = interrupted("switched", api, Setup, true, true);
    git::git(
        &switched.dir().join("api"),
        &["checkout", "-q", "-b", "switched-other"],
    )
    .unwrap();
    let replaced = interrupted("replaced", api, Setup, true, true);
    let replaced_path = replaced.dir().join("api");
    git::git(
        &api.path,
        &[
            "worktree",
            "remove",
            "--force",
            &replaced_path.to_string_lossy(),
        ],
    )
    .unwrap();
    fs::create_dir(&replaced_path).unwrap();
    fs::write(replaced_path.join("mine.txt"), "keep").unwrap();
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
        switched,
        replaced,
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
    assert_eq!(attempt(cx, "switched"), (1, Some(Uncertain), false));
    assert_eq!(attempt(cx, "replaced"), (1, Some(Uncertain), false));
    assert_eq!(
        fs::read_to_string(replaced_path.join("mine.txt")).unwrap(),
        "keep"
    );
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

    // Stopping tracking a changed identity also drops its stale repo metadata, but no files.
    for name in ["switched", "replaced"] {
        on(cx, &|s, w, cx| {
            s.open_workspace_named(name, w, cx);
            s.attachment_menu(name.into(), "api".into(), w, cx);
        });
        press_in(window)(cx, &["down", "enter"]);
        assert_eq!(attempt(cx, name), (0, None, false));
    }
    assert_eq!(
        fs::read_to_string(replaced_path.join("mine.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        git::git(
            &workspaces_dir().join("switched/api"),
            &["branch", "--show-current"]
        )
        .unwrap(),
        "switched-other",
    );
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
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let repo = w.find("repo-chip-api");
        assert_eq!(repo.role(), Some(Role::Button));
        for id in ["repo-chip-api", "run", "header-attach"] {
            let bounds = w.find(id).bounds();
            assert_eq!(bounds.size.height, px(32.), "{id} is a toolbar button");
            assert_eq!(bounds.center().y, repo.bounds().center().y, "{id} aligns");
        }
    })
    .unwrap();
    assert!(
        !shown(cx, "branch"),
        "branch actions live in the repository chip"
    );
    // Hovering shows the card; the click's menu takes its place rather than opening under it.
    cx.update_window(window, |_, w, cx| w.hover("repo-chip-api", cx))
        .unwrap();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(shown(cx, "repo-chip-api-card"), "hover opens the card");
    cx.update_window(window, |_, w, cx| w.click("repo-chip-api", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(shark.read(cx).menu.is_some()));
    assert!(!shown(cx, "repo-chip-api-card"), "the menu replaces it");
    press_in(window)(cx, &["escape"]);
    // A real button stays reachable through focus traversal and Enter/Space.
    for key in ["enter", "space"] {
        cx.update_window(window, |_, w, cx| {
            for _ in 0..100 {
                w.render_frame(cx);
                if w.find("repo-chip-api").focused() == Some(true) {
                    break;
                }
                w.focus_next(cx);
            }
            assert_eq!(w.find("repo-chip-api").focused(), Some(true));
            w.press(key, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert!(shark.read(cx).menu.is_some()));
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let button = w.find("repo-chip-api").bounds();
            let menu = w.find("menu-parent").bounds();
            assert!(
                (menu.top() - button.bottom() - px(4.)).abs() <= px(0.5),
                "{key} anchor"
            );
            assert!(
                (menu.left() - button.left()).abs() <= px(1.),
                "{key} anchor"
            );
        })
        .unwrap();
        press_in(window)(cx, &["escape"]);
    }
    // A different checkout does not inherit the attached branch's base in its description.
    let saved = ws_of(cx, &shark).repos[0].clone();
    let plain = cx
        .update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.find("repo-chip-api").bounds().size.width
        })
        .unwrap();
    on(cx, &|s, _, cx| {
        s.ws_mut(&name).unwrap().repos[0].branch = Some("attached-branch".into());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let chip = w.find("repo-chip-api").bounds();
        assert!(chip.size.width > plain, "only a drifted checkout names it");
        let label = w.find("repo-chip-api").label().unwrap().to_string();
        assert!(label.contains("Checked out: b"), "{label}");
        assert!(
            label.contains(&format!("Base: {} ← attached-branch", saved.base)),
            "{label}"
        );
        assert!(!label.contains(&format!("{} ← b", saved.base)), "{label}");
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.ws_mut(&name).unwrap().repos[0].branch = saved.branch.clone();
        cx.notify();
    });
    until(cx, window, |_, cx| {
        shark.read(cx).syncs[&name].read(cx).repos().is_some()
    })
    .await;
    on(cx, &|s, w, cx| {
        assert!(s.git_features.contains_key(&name), "the view it offered");
        let sync = s.syncs[&name].read(cx);
        assert_eq!(sync.repos().unwrap()[0].name, "api");
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

/// Draws until a measured header has settled: each change shows on the next frame.
fn header_settled(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
}

fn shows(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    cx.update_window(window, move |_, w, cx| {
        w.render_frame(cx);
        w.try_find(id).is_some_and(|e| e.visible())
    })
    .unwrap()
}

/// Cancelling a chip takes back that repo alone; the others stay queued, and what has started
/// stays out of reach. Cancel all queued is a row of its own.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cancelling_one_keeps_the_rest_queued(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web", "docs", "cli"], &[]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let chat = chat_of(cx, &shark);
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| {
        let picks = repos.iter().map(|r| fresh(r, "b")).collect();
        s.queue_attachment(name.clone(), picks, w, cx)
    });
    let notices = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| w.notifications(cx).len())
            .unwrap()
    };
    assert_eq!(notices(cx), 1, "told it is queued");
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "web".into(), w, cx)
    });
    press_in(window)(cx, &["enter"]);
    let left = |cx: &mut TestAppContext| -> Vec<String> {
        (ws_of(cx, &shark).attaching.iter())
            .map(|a| a.repo.clone())
            .collect()
    };
    assert_eq!(left(cx), ["api", "docs", "cli"]);
    assert_eq!(State::load().workspaces[0].attaching.len(), 3);
    cx.update(|cx| assert!(chat.read(cx).attach_hold, "the rest still hold"));

    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["down", "enter"]);
    assert!(left(cx).is_empty(), "the second row takes them all");
    cx.update(|cx| assert!(!chat.read(cx).attach_hold));
    cx.update(|cx| chat.update(cx, |c, _| c.busy = false));

    // Creating: the panel has nothing to cancel, and a call from a stale row changes nothing.
    on(cx, &|s, w, cx| {
        let picks = vec![fresh(&repos[0], "b"), fresh(&repos[1], "b")];
        s.queue_attachment(name.clone(), picks, w, cx);
        s.cancel_queued(&name, Some("api"), w, cx);
        assert_eq!(s.ws(&name).unwrap().attaching.len(), 2);
        assert_eq!(s.attach_actions(&name, "api")[0].1, "Close");
        s.cancel_queued(&name, Some("web"), w, cx);
    });
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.repos.len(), 1);
    assert!(ws.dir().join("api").exists() && !ws.dir().join("web").exists());
}

/// Failures come first and what doesn't fit is behind `+N` and cmd-k, so every failure can be
/// reached and dealt with, not only the first.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failures_stay_reachable_under_overflow(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, _) = agent(cx, &home, &["api"], &[]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let failed: Vec<String> = (0..40).map(|i| format!("repository-{i:02}")).collect();
    on(cx, &|s, _, _| {
        let results = s.attach_results.entry(name.clone()).or_default();
        results.failed = (failed.iter())
            .map(|r| (r.clone(), "branch exists".into()))
            .collect();
        s.ws_mut(&name).unwrap().repos.push(crate::state::WsRepo {
            name: "settled".into(),
            src: "/src/settled".into(),
            base: "main".into(),
            branch: None,
        });
    });
    cx.update_window(window, |_, w, _| w.resize(size(px(900.), px(600.))))
        .unwrap();
    header_settled(cx, window);
    assert!(shows(cx, window, "attach-chip-repository-00"), "first");
    assert!(!shows(cx, window, "attach-chip-repository-39"), "clipped");
    assert!(
        !shows(cx, window, "repo-chip-settled"),
        "behind the failures"
    );
    assert!(shows(cx, window, "attach-more"));
    for key in ["enter", "space"] {
        cx.update_window(window, |_, w, cx| {
            for id in ["attach-chip-repository-00", "attach-more"] {
                let button = w.find(id);
                assert_eq!(button.role(), Some(Role::Button));
                assert_eq!(button.bounds().size.height, px(32.));
            }
            for _ in 0..100 {
                w.render_frame(cx);
                if w.find("attach-chip-repository-00").focused() == Some(true) {
                    break;
                }
                w.focus_next(cx);
            }
            assert_eq!(w.find("attach-chip-repository-00").focused(), Some(true));
            w.press(key, cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            shows(cx, window, "attach-panel"),
            "{key} opens failure actions"
        );
        press_in(window)(cx, &["escape"]);
    }
    // `+N`: the hidden ones, the last of them a failure the header can't show.
    cx.update_window(window, |_, w, cx| w.click("attach-more", cx))
        .unwrap();
    cx.run_until_parked();
    let on_row = |cx: &mut TestAppContext, row: usize| {
        for _ in 0..row {
            press_in(window)(cx, &["down"]);
        }
        press_in(window)(cx, &["enter"]);
    };
    let hidden = 41
        - (0..40)
            .take_while(|i| {
                let id = SharedString::from(format!("attach-chip-repository-{i:02}"));
                shows(cx, window, id)
            })
            .count();
    on_row(cx, hidden - 2); // the settled repo is the last row
    assert!(shows(cx, window, "attach-panel"));
    press_in(window)(cx, &["enter"]);
    assert_eq!(
        cx.update(|cx| shark.read(cx).attach_results[&name].failed.len()),
        39,
        "dismissed from the list"
    );
    let gone = cx.update(|cx| {
        let failed = &shark.read(cx).attach_results[&name].failed;
        !failed.iter().any(|(r, _)| r == "repository-39")
    });
    assert!(gone, "the clipped one");

    // A hidden ready repository opens its branch actions, not a completed setup panel.
    on(cx, &|s, w, cx| s.attachment_list(name.clone(), 0, w, cx));
    cx.update_window(window, |_, w, cx| w.input("settled", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| assert!(shark.read(cx).menu.is_some()));
    assert!(!shows(cx, window, "attach-panel"));
    press_in(window)(cx, &["c", "enter"]);
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text())),
        Some(name.clone())
    );

    // cmd-k's Attach repository: everything, from the keyboard, then the dialog as its last row.
    on(cx, &|s, _, cx| {
        let results = s.attach_results.get_mut(&name).unwrap();
        results.failed.truncate(2);
        cx.notify();
    });
    let form = |cx: &mut TestAppContext| shows(cx, window, ("repo-pick", 0usize));
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert!(!form(cx), "not the dialog: failures come first");
    on_row(cx, 1);
    press_in(window)(cx, &["enter"]);
    assert_eq!(
        cx.update(|cx| shark.read(cx).attach_needs(&name)).unwrap(),
        "Couldn't attach repository-00"
    );
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    on_row(cx, 0);
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).attach_needs(&name), None));
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert!(form(cx), "nothing left in the way");
}

/// The Attach buttons give up their labels before chips get hidden, and take them back only with
/// room to spare: nothing flips from frame to frame.
#[test]
fn chips_get_the_room_first() {
    let wide = px(1000.);
    let fit = Fit::default().next(0, 3, wide);
    assert_eq!(fit, Fit::default());
    let tight = fit.next(2, 3, wide);
    assert_eq!(tight.hidden, 0, "labels go first");
    assert_eq!(
        tight.next(0, 3, wide),
        tight,
        "stays, though it all fits now"
    );
    let hidden = tight.next(1, 3, wide);
    assert_eq!(hidden.hidden, 1);
    assert_eq!(hidden.next(1, 3, wide), hidden);
    assert_eq!(hidden.next(0, 3, wide + px(10.)).hidden, 0, "room again");
    assert_eq!(hidden.next(0, 2, wide), Fit::default(), "a chip went");
    assert_eq!(hidden.next(0, 3, wide + px(400.)), Fit::default());
}

/// Each stage marks its steps: what's done, what's on, what's left, and only what failed is red.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_panel_marks_each_step(cx: &mut TestAppContext) {
    use super::super::attach::Step::*;
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    let name = ws_of(cx, &shark).name;
    let chat = chat_of(cx, &shark);
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    let on = on_app(window, shark.clone());
    for (stage, error, marks, detail) in [
        (
            Some(AttachStage::Queued),
            None,
            [Current, Pending, Pending],
            [Some("1 chat running"), None, None],
        ),
        (
            Some(AttachStage::Creating),
            None,
            [Done, Current, Pending],
            [None; 3],
        ),
        (
            Some(AttachStage::Setup),
            None,
            [Done, Done, Current],
            [None; 3],
        ),
        (
            Some(AttachStage::SetupFailed),
            Some("api: `x` failed"),
            [Done, Done, Failed],
            [None, None, Some("api: `x` failed")],
        ),
        (
            Some(AttachStage::Uncertain),
            Some("it isn't ours"),
            [Done, Failed, Pending],
            [None, Some("it isn't ours"), None],
        ),
        (
            None,
            Some("branch exists"),
            [Done, Failed, Pending],
            [None, Some("branch exists"), None],
        ),
        (None, None, [Done, Done, Done], [None; 3]),
    ] {
        on(cx, &|s, _, _| {
            let ws = s.ws_mut(&name).unwrap();
            ws.attaching = (stage.iter())
                .map(|st| Attachment {
                    stage: *st,
                    error: error.map(String::from),
                    ..fresh(&repos[0], "b")
                })
                .collect();
            let results = s.attach_results.entry(name.clone()).or_default();
            results.failed = (error.iter())
                .filter(|_| stage.is_none())
                .map(|e| ("api".to_string(), e.to_string()))
                .collect();
        });
        let steps = cx.update(|cx| shark.read(cx).attach_steps(&name, "api", cx));
        assert_eq!(steps.each_ref().map(|s| s.1), marks, "{stage:?}");
        assert_eq!(
            steps.each_ref().map(|s| s.2.as_deref()),
            detail,
            "{stage:?}"
        );
        assert_eq!(
            steps.map(|s| s.0),
            ["Waiting for idle", "Creating worktree", "Running setup"]
        );
    }
}

/// The log is shared between repos and setup goes on after a failure: the panel shows the failed
/// repo's own last lines.
#[test]
fn the_tail_is_the_failed_repos() {
    let log = "\n$ api: one\nold\n\n$ api: two\nline a\nline b\n\n$ web: three\nweb says\n";
    assert_eq!(setup_tail(log, "api"), "$ api: two\nline a\nline b");
    assert_eq!(setup_tail(log, "web"), "$ web: three\nweb says");
    assert_eq!(setup_tail(log, "my-api"), "");
    let long = format!("$ api: one\n{}", "x\n".repeat(20));
    assert_eq!(setup_tail(&long, "api").lines().count(), 8);
}

/// A panel left open follows the attachment: opened while it starts, it offers Retry once setup
/// has failed, and the retry works.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn an_open_panel_follows_the_failure(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[("api", "echo broken; exit 3")]);
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    on(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![fresh(&repos[0], "b")], w, cx);
        s.attachment_menu(name.clone(), "api".into(), w, cx);
    });
    settled(cx, window, &shark).await;
    assert_eq!(stage(cx, &shark), AttachStage::SetupFailed);
    assert!(shows(cx, window, ("attach-step", 2usize)));
    on(cx, &|s, _, _| {
        s.state.repo_settings.get_mut("api").unwrap().setup = vec!["true".into()];
    });
    press_in(window)(cx, &["down", "enter"]);
    settled(cx, window, &shark).await;
    assert!(ws_of(cx, &shark).ready(), "Retry was on the open panel");
    on(cx, &|s, w, cx| {
        s.attachment_menu(name.clone(), "api".into(), w, cx)
    });
    press_in(window)(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
}

/// The hold says what it waits on and what to do, naming the repo: a failure waits on the user,
/// not on an attachment that is under way.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_hold_says_what_it_waits_on(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (_, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    let chat = chat_of(cx, &shark);
    let mut ws = ws_of(cx, &shark);
    let held = |cx: &mut TestAppContext, ws: &Workspace| {
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.workspace_changed(ws, cx);
                c.held()
            })
        })
    };
    assert_eq!(held(cx, &ws), None);
    for (stage, words) in [
        (AttachStage::Queued, "api is queued"),
        (AttachStage::Creating, "api is being attached"),
        (AttachStage::Setup, "api is being attached"),
        (AttachStage::SetupFailed, "setup of api failed"),
        (AttachStage::Uncertain, "can't tell how api ended up"),
    ] {
        let a = Attachment {
            stage,
            ..fresh(&repos[0], "b")
        };
        ws.attaching = vec![a];
        let why = held(cx, &ws).unwrap();
        assert!(why.contains(words) && why.contains("chip"), "{why}");
    }
    // Who needs the user is named over who is merely waiting its turn.
    ws.attaching.insert(0, fresh(&repos[1], "b"));
    ws.attaching[1].stage = AttachStage::SetupFailed;
    assert!(held(cx, &ws).unwrap().contains("setup of api failed"));
    // A git step still comes first; it clears by itself.
    cx.update(|cx| chat.update(cx, |c, _| c.hold = true));
    assert_eq!(held(cx, &ws).unwrap(), "a git step is running");
}

/// The dialog's base button lists bases: enter takes one as the base, not as a branch to check
/// out, the default branch is marked, and the row says what will be made.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn the_base_button_picks_a_base(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let src = &repos[0].path;
    git::git(src, &["branch", "zz-base"]).unwrap();
    git::git(src, &["update-ref", "refs/remotes/origin/main", "main"]).unwrap();
    let on = on_app(window, shark.clone());
    let name = ws_of(cx, &shark).name;
    let press = press_in(window);
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert_eq!(summary(cx, window), format!("{name} from main"), "visible");
    assert!(!shows(cx, window, "base-default"));
    let click = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(window, |_, w, cx| w.click(id, cx))
            .unwrap();
        cx.run_until_parked();
    };
    click(cx, "attach-change");
    click(cx, "source-other");
    assert!(shows(cx, window, "base-default"), "marked Default");
    let list = git::branches(src).unwrap();
    let main = list.iter().position(|b| b.name == "main").unwrap();
    let custom = list.iter().position(|b| b.name == "zz-base").unwrap();
    // It starts on the current base, the default.
    for _ in 0..custom.saturating_sub(main) {
        press(cx, &["down"]);
    }
    for _ in 0..main.saturating_sub(custom) {
        press(cx, &["up"]);
    }
    press(cx, &["enter"]);
    assert!(shows(cx, window, "attach-summary"), "back on the repos");
    // Reopening the base list keeps our choice, even though the default sorts before it.
    assert!(main < custom);
    click(cx, "attach-change");
    click(cx, "source-other");
    press(cx, &["enter"]);
    press(cx, &["enter"]);
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.repos[0].base, "zz-base");
    assert_eq!(ws.branch, ws.name, "still the new branch, not zz-base");
}

fn summary(cx: &mut TestAppContext, window: AnyWindowHandle) -> String {
    label(cx, window, "attach-summary")
}

fn label(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) -> String {
    let id = id.into();
    cx.update_window(window, move |_, w, cx| {
        w.render_frame(cx);
        w.find(id).label().unwrap_or_default().to_string()
    })
    .unwrap()
}

fn is_open(cx: &mut TestAppContext, window: AnyWindowHandle) -> bool {
    cx.update_window(window, |_, w, cx| w.has_active_dialog(cx))
        .unwrap()
}

/// `name` attached already, so the agent has a branch for later repositories to follow.
async fn with_branch(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    repo: &Repo,
) {
    let name = ws_of(cx, shark).name;
    let first = fresh(repo, &ws_of(cx, shark).default_branch());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.queue_attachment(name.clone(), vec![first.clone()], w, cx)
    });
    settled(cx, window, shark).await;
}

/// The common case is one key: nothing to tick, nothing to confirm.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn one_enter_attaches_the_highlighted_repository(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, _) = agent(cx, &home, &["api", "web"], &[]);
    let name = ws_of(cx, &shark).name;
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert_eq!(label(cx, window, "create"), "Attach 1 repository");
    press_in(window)(cx, &["down", "enter"]);
    settled(cx, window, &shark).await;
    assert!(!is_open(cx, window));
    let ws = ws_of(cx, &shark);
    let names: Vec<&str> = ws.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["web"]);
}

/// cmd-enter marks and stays; the button counts what enter would take: the marks, or the highlight
/// with none.
/// Space isn't a mark: the field takes typing.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn marks_count_on_the_button(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, _) = agent(cx, &home, &["api", "web", "docs"], &[]);
    let name = ws_of(cx, &shark).name;
    let press = press_in(window);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    press(cx, &["space"]);
    assert_eq!(label(cx, window, "create"), "Attach 1 repository");
    press(cx, &["cmd-enter"]);
    assert!(is_open(cx, window), "marking stays open");
    assert_eq!(label(cx, window, "create"), "Attach 1 repository");
    press(cx, &["down"]);
    assert_eq!(
        label(cx, window, "create"),
        "Attach 1 repository",
        "marks win over the highlight"
    );
    press(cx, &["cmd-enter", "down", "down"]);
    assert_eq!(
        label(cx, window, "create"),
        "Attach 2 repositories",
        "Add a local repository is no target"
    );
    cx.update_window(window, |_, w, cx| w.click("create", cx))
        .unwrap();
    settled(cx, window, &shark).await;
    assert!(!is_open(cx, window));
    let ws = ws_of(cx, &shark);
    let names: Vec<&str> = ws.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api", "web"]);
}

/// A later repository shows the agent's branch and can't rename it; its base stays its own.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_later_repository_follows_the_agents_branch(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    with_branch(cx, window, &shark, &repos[0]).await;
    let name = ws_of(cx, &shark).name;
    let press = press_in(window);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert!(shows(cx, window, "attach-follows"));
    press(cx, &["cmd-b"]);
    assert!(shows(cx, window, "attach-follows"), "read-only in Change");
    press(cx, &["tab", "escape"]);
    assert!(is_open(cx, window), "esc goes back, the dialog stays");
    press(cx, &["enter"]);
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.repos[1].name, "web");
    assert_eq!(ws.repos[1].branch, None, "the inherited one");
}

/// A taken name changes the row and offers another: tab takes it, nothing reuses the branch.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_collision_says_so_and_suggests_a_name(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    let ws = ws_of(cx, &shark);
    let (name, branch) = (ws.name.clone(), ws.default_branch());
    let src = &repos[0].path;
    git::git(src, &["branch", &branch]).unwrap();
    git::git(src, &["branch", &format!("{branch}-2")]).unwrap();
    let tip = git::git(src, &["rev-parse", &branch]).unwrap();
    let press = press_in(window);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert_eq!(
        label(cx, window, ("attach-row", 0usize)),
        format!("api, branch {branch} already exists")
    );
    assert_eq!(
        label(cx, window, ("attach-rename", 0usize)),
        format!("Use {branch}-3"),
        "the next free one"
    );
    press(cx, &["enter"]);
    assert!(is_open(cx, window), "refused, and told");
    assert!(ws_of(cx, &shark).attaching.is_empty());
    press(cx, &["tab"]);
    assert_eq!(label(cx, window, ("attach-row", 0usize)), "api");
    assert_eq!(summary(cx, window), format!("{branch}-3 from main"));
    press(cx, &["enter"]);
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    assert_eq!(ws.branch, format!("{branch}-3"));
    assert_eq!(git::git(src, &["rev-parse", &branch]).unwrap(), tip);
}

/// In a later repository only the one that collides leaves the agent's branch, and it can name it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn a_later_collision_is_that_repositorys_own(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web", "docs"], &[]);
    with_branch(cx, window, &shark, &repos[0]).await;
    let ws = ws_of(cx, &shark);
    let (name, branch) = (ws.name.clone(), ws.branch.clone());
    git::git(&repos[1].path, &["branch", &branch]).unwrap();
    let press = press_in(window);
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert!(!shows(cx, window, "attach-follows"), "it collides");
    press(cx, &["cmd-enter", "cmd-b", "cmd-a"]);
    cx.update_window(window, |_, w, cx| w.input("my/web", cx))
        .unwrap();
    press(cx, &["enter", "down"]);
    assert!(shows(cx, window, "attach-follows"), "docs still follows");
    press(cx, &["cmd-enter", "enter"]);
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    let branch_of = |repo: &str| {
        let r = ws.repos.iter().find(|r| r.name == repo).unwrap();
        r.branch.clone()
    };
    assert_eq!(branch_of("web").as_deref(), Some("my/web"));
    assert_eq!(branch_of("docs"), None);
}

/// Attached repositories stay in the list, last and out of reach; recently used come first.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn attached_rows_are_listed_and_disabled(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web", "docs"], &[]);
    with_branch(cx, window, &shark, &repos[0]).await;
    let name = ws_of(cx, &shark).name;
    let on = on_app(window, shark.clone());
    on(cx, &|s, _, _| {
        let used = |r: &str| crate::state::WsRepo {
            name: r.into(),
            src: "/nowhere".into(),
            base: "main".into(),
            branch: None,
        };
        s.state.workspaces.push(Workspace {
            name: "older".into(),
            repos: vec![used("docs")],
            last_interacted_at: Some(1),
            ..Default::default()
        });
    });
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    // docs was used, web never, and api is attached.
    assert_eq!(label(cx, window, ("attach-row", 0usize)), "docs");
    assert_eq!(label(cx, window, ("attach-row", 1usize)), "web");
    assert_eq!(label(cx, window, ("attach-row", 2usize)), "api, attached");
    assert!(!shows(cx, window, ("repo-pick", 2usize)));
    cx.update_window(window, |_, w, cx| w.click(("attach-row", 2usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(is_open(cx, window), "a click does nothing");
    let press = press_in(window);
    press(cx, &["down", "down", "enter"]);
    assert!(cx.did_prompt_for_paths(), "down skips it, to the last row");
    cx.simulate_path_prompt_response(|_| None);
}

/// With nothing registered there's no list to land on: the panel, and the dialog once there's a
/// repository. A bad folder is told and goes nowhere, and cancelling leaves nothing open.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn no_repositories_go_straight_to_the_folder_panel(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, _) = agent(cx, &home, &[], &[]);
    let name = ws_of(cx, &shark).name;
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert!(cx.did_prompt_for_paths() && !is_open(cx, window));
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_paths() && !is_open(cx, window));

    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    let bad = home.join("plain");
    fs::create_dir_all(&bad).unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec![bad.clone()]));
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_paths() && !is_open(cx, window));

    on(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    let good = home.join("src/api");
    init_repo(&good);
    cx.simulate_path_prompt_response(|_| Some(vec![good.clone()]));
    cx.run_until_parked();
    assert!(is_open(cx, window));
    assert_eq!(label(cx, window, "create"), "Attach 1 repository");
    press_in(window)(cx, &["enter"]);
    settled(cx, window, &shark).await;
    assert_eq!(ws_of(cx, &shark).repos[0].name, "api");
}

/// Everything attached: a title, a line and the one thing to do, then the list once it's done.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn all_attached_offers_a_local_repository(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api"], &[]);
    with_branch(cx, window, &shark, &repos[0]).await;
    let name = ws_of(cx, &shark).name;
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    assert!(shows(cx, window, "attach-none"));
    assert!(!shows(cx, window, ("attach-row", 0usize)), "no list");
    let press = press_in(window);
    press(cx, &["enter"]);
    assert!(cx.did_prompt_for_paths(), "enter does it");
    assert!(is_open(cx, window));
    let web = home.join("src/web");
    init_repo(&web);
    cx.simulate_path_prompt_response(|_| Some(vec![web.clone()]));
    cx.run_until_parked();
    assert!(!shows(cx, window, "attach-none"));
    assert_eq!(label(cx, window, "create"), "Attach 1 repository");
    press(cx, &["enter"]);
    settled(cx, window, &shark).await;
    assert_eq!(ws_of(cx, &shark).repos[1].name, "web");
}

/// The dialog stays through the folder panel: marks, the branch, a repository's base and the
/// filter survive a cancel and a bad folder, and the new repository arrives marked.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn add_local_comes_back_to_the_dialog_as_it_was(cx: &mut TestAppContext) {
    guards!(quiet, _turn, _claude);
    let home = test_home();
    let (window, shark, repos) = agent(cx, &home, &["api", "web"], &[]);
    git::git(&repos[1].path, &["branch", "develop"]).unwrap();
    let name = ws_of(cx, &shark).name;
    let press = press_in(window);
    let input = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        cx.run_until_parked();
    };
    on_app(window, shark.clone())(cx, &|s, w, cx| s.attach_repository(name.clone(), w, cx));
    // web on develop, the branch renamed, api marked, a filter on the last row.
    press(cx, &["down", "cmd-b", "cmd-a"]);
    input(cx, "my/branch");
    press(cx, &["cmd-b"]);
    input(cx, "develop");
    press(cx, &["enter"]);
    assert_eq!(summary(cx, window), "my/branch from develop");
    press(cx, &["up"]);
    assert_eq!(summary(cx, window), "my/branch from main");
    press(cx, &["cmd-enter"]);
    input(cx, "local");
    press(cx, &["enter"]);
    assert!(cx.did_prompt_for_paths());
    assert!(is_open(cx, window), "open behind the panel");
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    let state = |cx: &mut TestAppContext| {
        assert!(is_open(cx, window));
        assert_eq!(label(cx, window, "create"), "Attach 2 repositories");
        assert!(!shows(cx, window, ("attach-row", 0usize)), "filtered still");
    };
    state(cx);

    // A bad folder and a duplicate: told, and back here.
    press(cx, &["enter"]);
    let bad = home.join("plain");
    fs::create_dir_all(&bad).unwrap();
    cx.simulate_path_prompt_response(|_| Some(vec![bad.clone()]));
    cx.run_until_parked();
    state(cx);
    press(cx, &["enter"]);
    let same = repos[0].path.clone();
    cx.simulate_path_prompt_response(|_| Some(vec![same.clone()]));
    cx.run_until_parked();
    state(cx);

    press(cx, &["enter"]);
    let docs = home.join("src/docs");
    init_repo(&docs);
    cx.simulate_path_prompt_response(|_| Some(vec![docs.clone()]));
    cx.run_until_parked();
    assert_eq!(label(cx, window, "create"), "Attach 3 repositories");
    cx.update_window(window, |_, w, cx| w.click("create", cx))
        .unwrap();
    settled(cx, window, &shark).await;
    let ws = ws_of(cx, &shark);
    let names: Vec<&str> = ws.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api", "web", "docs"]);
    assert_eq!(ws.branch, "my/branch");
    assert_eq!(ws.repos[1].base, "develop");
}

/// New workspace from branch shares the form and keeps its shell: its heading and count, ticking
/// with space and enter, and cmd-enter to create.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_agent_from_branch_keeps_its_keys(cx: &mut TestAppContext) {
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
    let press = press_in(window);
    branch_dialog(cx, window);
    assert!(shows(cx, window, ("repo-prs", 0usize)));
    assert!(!shows(cx, window, "attach-summary"));
    cx.update_window(window, |_, w, cx| w.input("kept", cx))
        .unwrap();
    press(cx, &["tab", "space", "down", "enter"]);
    assert!(is_open(cx, window), "enter ticks, in the list");
    press(cx, &["cmd-enter"]);
    until(cx, window, {
        let shark = shark.clone();
        move |_, cx| shark.read(cx).ws("kept").is_some()
    })
    .await;
    let ws = cx.update(|cx| shark.read(cx).ws("kept").cloned().unwrap());
    assert_eq!(ws.repos.len(), 2);
}

/// Real pixels, both themes: chips in each state, overflow, the confirm step and the panel.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web", "payments"]);
    let api = &repos[0].path;
    git::git(&repos[1].path, &["branch", "you/eng-123-taken"]).unwrap();
    git::git(api, &["branch", "develop"]).unwrap();
    git::git(api, &["branch", "release/2.4"]).unwrap();
    git::git(api, &["remote", "add", "origin", api.to_str().unwrap()]).unwrap();
    git::git(api, &["fetch", "-q", "origin"]).unwrap();
    let (src, name) = (&repos[0], "attach-preview");
    let stage = |repo: &str, stage, error: Option<&str>| Attachment {
        repo: repo.into(),
        stage,
        error: error.map(String::from),
        base: "develop".into(),
        ..Attachment::fresh(src, "you/eng-123-login", "")
    };
    let log = crate::scripts::log_path(name);
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(
        log,
        "\n$ payments: npm ci\nnpm warn deprecated inflight@1.0.6\nnpm error code ERESOLVE\n\
         npm error ERESOLVE could not resolve\nnpm error While resolving: react-dom@19.1.0\n\
         npm error Found: react@18.3.1\nnpm error A complete log of this run can be found in: \
         /Users/you/.npm/_logs/2026-11-06T10_12_44_121Z-debug-0.log\n\n$ search: make setup\nok\n",
    )
    .unwrap();
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for (case, width) in [
            ("header", 1280.),
            ("chips", 1280.),
            ("overflow", 860.),
            ("confirm", 1000.),
            ("first", 1000.),
            ("marks", 1000.),
            ("collision", 1000.),
            ("change", 1000.),
            ("change-later", 1000.),
            ("bases", 1000.),
            ("none", 1000.),
            ("queued", 1000.),
            ("running", 1000.),
            ("failed", 1000.),
        ] {
            let mut state = State {
                repos: repos.clone(),
                ..Default::default()
            };
            state.appearance.mode = mode;
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(640.)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| cx.new(|cx| Shark::new(w, cx)),
                    )
                })
                .unwrap();
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.home.on = false;
                    s.active = Some(name.into());
                    s.tab = Tab::Chat;
                    let settled = |r: &str| crate::state::WsRepo {
                        name: r.into(),
                        src: src.path.clone(),
                        base: "main".into(),
                        branch: None,
                    };
                    let mut ws = Workspace {
                        name: name.into(),
                        branch: "you/eng-123-login".into(),
                        chats: ["attach-preview-1", "attach-preview-2"]
                            .map(|id| ChatMeta {
                                id: id.into(),
                                ..Default::default()
                            })
                            .into(),
                        repos: ["payments", "ledger", "docs-site", "mobile-app"]
                            .map(settled)
                            .into(),
                        attaching: vec![
                            stage("search", AttachStage::Queued, None),
                            stage(
                                "payments",
                                AttachStage::SetupFailed,
                                Some("payments: `npm ci` failed (exit status: 1)"),
                            ),
                            stage("ledger", AttachStage::Setup, None),
                            stage("docs-site", AttachStage::Creating, None),
                            stage(
                                "mobile-app",
                                AttachStage::Uncertain,
                                Some("branch `b` exists and isn't Shark's"),
                            ),
                        ],
                        ..Default::default()
                    };
                    ws.repos.push(settled("notes"));
                    let id = |n: &str| crate::linear::Snapshot {
                        org: "acme".into(),
                        id: format!("uuid-{n}"),
                        identifier: n.into(),
                        title: "Sign-in loops on expired tokens".into(),
                        ..Default::default()
                    };
                    ws.issues = vec![id("ENG-123"), id("ENG-124")];
                    for s in &ws.issues {
                        ws.linear.attach(s.into());
                    }
                    ws.normalize_tabs();
                    s.state.workspaces = vec![ws.clone()];
                    if case == "queued" {
                        for meta in &ws.chats {
                            let chat = s.ensure_chat(&ws, meta, w, cx);
                            chat.update(cx, |c, _| c.busy = true);
                        }
                    }
                    let results = s.attach_results.entry(name.into()).or_default();
                    results.failed = vec![(
                        "billing".into(),
                        "billing: branch `you/eng-123-login` already exists; pick another name"
                            .into(),
                    )];
                    if case == "header" {
                        results.failed.clear();
                    }
                    let ws = s.ws_mut(name).unwrap();
                    match case {
                        "overflow" | "chips" => {}
                        "header" => {
                            ws.attaching = vec![];
                            ws.repos.truncate(2);
                            ws.repos[1].branch = Some("feature/ledger-reconciliation".into());
                            ws.repos[1].base = "origin/develop".into();
                            ws.linear.remove(&(&ws.issues[1]).into());
                        }
                        "queued" => ws.attaching.truncate(1),
                        _ => ws.attaching = vec![],
                    }
                    match case {
                        "running" => ws.attaching = vec![stage("ledger", AttachStage::Setup, None)],
                        "failed" => {
                            ws.attaching = vec![stage(
                                "payments",
                                AttachStage::SetupFailed,
                                Some("payments: `npm ci` failed (exit status: 1)"),
                            )]
                        }
                        _ => {}
                    }
                    let open = ["queued", "running", "failed"];
                    if let Some(at) = open.iter().position(|c| *c == case) {
                        let repo = ["search", "ledger", "payments"][at];
                        s.attachment_menu(name.into(), repo.into(), w, cx);
                    }
                    let dialog = [
                        "confirm",
                        "first",
                        "marks",
                        "collision",
                        "change",
                        "change-later",
                        "bases",
                        "none",
                    ];
                    if dialog.contains(&case) {
                        let ws = s.ws_mut(name).unwrap();
                        match case {
                            "first" | "marks" | "change" | "bases" => ws.repos.clear(),
                            "collision" => ws.branch = "you/eng-123-taken".into(),
                            _ => {}
                        }
                        // Agents that used the others lately.
                        let used = |name: &str, repo: &str, hours: u64| {
                            let repo = crate::state::WsRepo {
                                name: repo.into(),
                                src: src.path.clone(),
                                base: "main".into(),
                                branch: None,
                            };
                            let hours = hours * 3_600_000;
                            Workspace {
                                name: name.into(),
                                repos: vec![repo],
                                last_interacted_at: Some(
                                    super::super::interaction::now_ms() - hours,
                                ),
                                ..Default::default()
                            }
                        };
                        (s.state.workspaces).extend([used("a", "web", 2), used("b", "api", 30)]);
                        if case == "none" {
                            s.state.repos.retain(|r| r.name == "payments");
                        }
                        s.attach_results.clear();
                        // Marked from the start: a tick marked now is still animating in.
                        let marked = (case == "marks").then(|| "web".to_string());
                        s.attach_dialog(name.into(), marked.into_iter().collect(), w, cx);
                    }
                    cx.notify();
                })
            })
            .unwrap();
            cx.run_until_parked();
            // Capture settled dialogs even when boot reinitializes motion preferences.
            cx.update(|cx| cx.set_reduce_motion(true));
            // The branch lists load in the background; the base list is one click on.
            for frame in 0..6 {
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    match (case, frame) {
                        ("change" | "change-later" | "bases", 2) | ("bases", 3) => {
                            w.press("cmd-b", cx)
                        }
                        ("marks", 2) => w.press("down", cx),
                        _ => {}
                    }
                })
                .unwrap();
                cx.run_until_parked();
            }
            let screenshot = cx.capture_screenshot(window).expect("real attach renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!("attach-{case}-{label}.png")))
                    .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    println!("attachment previews: 30 rendered (both themes)");
}
