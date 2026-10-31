//! Git is an ordinary workspace artifact: one tab, persisted ordering, independent drafts.
use super::*;
use crate::features::git::{
    GitFeature,
    commits::{Commits, FileView},
};
use gpui_kit::test::TestEventExt;
use serde_json::{Value, json};

fn pr_fixture(threads: usize) -> Value {
    let connection = |nodes: Vec<Value>| json!({"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}});
    let comment = |id: &str, body: &str| json!({"id":id,"body":body,"author":{"login":"reviewer"},"url":"https://github.com/acme/api/pull/7#discussion_r1","createdAt":"2026-10-27T10:00:00Z"});
    json!({
        "id":"PR_7","number":7,"url":"https://github.com/acme/api/pull/7",
        "title":"Make sign-in more reliable","body":"## Why\nKeep the session across reconnects.",
        "state":"OPEN","isDraft":false,"author":{"login":"author"},
        "headRefName":"main","baseRefName":"master","viewerCanUpdate":true,"locked":false,
        "comments":connection(vec![comment("C_1", "Please explain the retry policy.")]),
        "reviews":connection(vec![]),
        "reviewThreads":connection((0..threads).map(|i| json!({
            "id":format!("T_{i}"),"path":"src/login.rs","line":i+1,
            "isResolved":false,"isOutdated":i==1,"viewerCanReply":true,
            "viewerCanResolve":true,"viewerCanUnresolve":true,
            "comments":connection(vec![comment(&format!("TC_{i}"), "Does this retry when the connection drops?")]),
        })).collect()),
    })
}

/// Real subprocess boundary, local JSON state: every write stays inside the test workspace.
fn fake_github(home: &Path, state: &State, threads: usize) -> (GhGuard, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let script = home.join("fake-gh");
    fs::write(&script, r#"#!/usr/bin/env python3
import fcntl, json, pathlib, sys, time
p = pathlib.Path('git-pr.json')
if not p.exists():
    print('[]')
    sys.exit(0)
pr = json.loads(p.read_text())
args = sys.argv[1:]
if args[:2] == ['pr', 'view']:
    print(json.dumps(pr))
    sys.exit(0)
if args[:2] == ['pr', 'list']:
    print(json.dumps([dict(pr, isCrossRepository=False)]))
    sys.exit(0)
fields = dict(a.split('=', 1) for a in args if '=' in a)
query = fields.get('query', '')
lock = open(pathlib.Path(__file__).parent / 'git-active.lock', 'w')
try:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    (pathlib.Path(__file__).parent / 'git-overlap').write_text('overlapping requests')
    fcntl.flock(lock, fcntl.LOCK_EX)
if pathlib.Path('git-delay').exists():
    time.sleep(float(pathlib.Path('git-delay').read_text()))
if pathlib.Path('git-fail').exists():
    sys.stderr.write('HTTP 503: GitHub is temporarily unavailable\n')
    sys.exit(1)
if query.startswith('mutation'):
    with open('git-writes.jsonl', 'a') as f:
        f.write(json.dumps(fields) + '\n')
    if 'updatePullRequest' in query:
        pr.update(title=fields['title'], body=fields['body'])
        key = 'updatePullRequest'
    elif 'addComment' in query:
        key = 'addComment'
        pr['comments']['nodes'].append(dict(pr['comments']['nodes'][0], id='posted', body=fields['body']))
    elif 'addPullRequestReviewThreadReply' in query:
        key = 'addPullRequestReviewThreadReply'
        thread = next(t for t in pr['reviewThreads']['nodes'] if t['id'] == fields['id'])
        thread['comments']['nodes'].append(dict(thread['comments']['nodes'][0], id='reply', body=fields['body']))
    else:
        key = 'unresolveReviewThread' if 'unresolveReviewThread' in query else 'resolveReviewThread'
        thread = next(t for t in pr['reviewThreads']['nodes'] if t['id'] == fields['id'])
        thread['isResolved'] = key == 'resolveReviewThread'
    p.write_text(json.dumps(pr))
    result = {'updatePullRequest':{'pullRequest':{'id':pr['id']}}, 'addComment':{'commentEdge':{'node':{'id':'posted'}}}, 'addPullRequestReviewThreadReply':{'comment':{'id':'reply'}}}.get(key, {'thread':{'id':fields['id'], 'isResolved':key == 'resolveReviewThread'}})
    print(json.dumps({'data':{key:result}}))
elif 'node(id:' in query:
    thread = next(t for t in pr['reviewThreads']['nodes'] if t['id'] == fields['id'])
    print(json.dumps({'data':{'node':dict(thread, pullRequest={'id':pr['id']})}}))
else:
    print(json.dumps({'data':{'repository':{'pullRequest':pr}}}))
"#).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    for ws in &state.workspaces {
        for repo in &ws.repos {
            let dir = ws.dir().join(&repo.name);
            fs::write(dir.join("git-pr.json"), pr_fixture(threads).to_string()).unwrap();
            git::git(
                &dir,
                &[
                    "config",
                    "branch.main.shark-pr-url",
                    "https://github.com/acme/api/pull/7",
                ],
            )
            .unwrap();
        }
    }
    let dir = state.workspaces[0].dir().join("api");
    (GhGuard::set(&script), dir)
}

fn fixture(home: &Path) -> State {
    let repos = repos(home, &["api"]);
    let mut state = State {
        repos: repos.clone(),
        ..Default::default()
    };
    for name in ["git-review", "git-other"] {
        let mut ws = Workspace {
            name: name.into(),
            branch: "main".into(),
            repos: vec![crate::state::WsRepo {
                name: "api".into(),
                src: repos[0].path.clone(),
                base: "main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        ws.migrate();
        init_repo(&ws.dir().join("api"));
        state.workspaces.push(ws);
    }
    state.last_open = Some(("git-review".into(), "chat".into()));
    state
}

#[test]
fn git_artifact_state_migrates_and_normalizes() {
    let legacy = r#"{"name":"legacy","branch":"main","repos":[],"diff_closed":false}"#;
    let mut ws: Workspace = serde_json::from_str(legacy).unwrap();
    ws.migrate();
    assert!(ws.git_closed);
    assert!(!ws.tab_order.contains(&TabId::Git));
    ws.git_closed = false;
    ws.open_tab(TabId::Git);
    ws.open_tab(TabId::Git);
    assert_eq!(ws.tab_order.iter().filter(|t| **t == TabId::Git).count(), 1);
    assert!(ws.move_tab(&TabId::Git, &TabId::Chat("legacy".into())));
    let saved = serde_json::to_string(&ws).unwrap();
    let mut restored: Workspace = serde_json::from_str(&saved).unwrap();
    restored.migrate();
    assert_eq!(restored.tab_order, ws.tab_order);
    assert!(!restored.git_closed);
    restored.tab_order.push(TabId::Git);
    restored.normalize_tabs();
    assert_eq!(restored.tab_order, ws.tab_order);
    restored.git_closed = true;
    restored.normalize_tabs();
    assert!(!restored.tab_order.contains(&TabId::Git));
    assert!(restored.tab_order.contains(&TabId::Diff));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_tab_launch_close_reorder_and_workspace_restore(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("feature-git").visible());
        assert!(w.try_find("git-tab").is_none());
        assert!(shark.read(cx).active_ws().unwrap().git_closed);
        w.click("feature-git", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    for _ in 0..2 {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("feature-git", cx);
            w.render_frame(cx);
            let s = shark.read(cx);
            assert_eq!(s.tab, Tab::Feature(Feature::Git));
            assert_eq!(s.active_git_feature(), Some(&feature));
            assert_eq!(w.find_all("git-tab").len(), 1);
            assert!(!s.active_ws().unwrap().git_closed);
        })
        .unwrap();
    }
    assert!(!State::load().workspaces[0].git_closed);
    on(cx, &|s, _, cx| {
        s.move_tab(
            "git-review",
            &TabId::Git,
            &TabId::Chat("git-review".into()),
            cx,
        )
    });
    assert_eq!(State::load().workspaces[0].tab_order[0], TabId::Git);
    press(cx, &["ctrl-2"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    press(cx, &["ctrl-shift-tab"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Feature(Feature::Git)));
    press(cx, &["cmd-w"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    assert!(State::load().workspaces[0].git_closed);
    press(cx, &["cmd-shift-g"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).tab, Tab::Feature(Feature::Git));
        assert_eq!(shark.read(cx).active_git_feature(), Some(&feature));
    });
    on(cx, &|s, w, cx| s.open_workspace_named("git-other", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(shark.read(cx).active_ws().unwrap().git_closed);
        assert!(w.try_find("git-tab").is_none());
    })
    .unwrap();
    press(cx, &["cmd-shift-g"]);
    cx.update(|cx| assert_ne!(shark.read(cx).active_git_feature(), Some(&feature)));
    on(cx, &|s, w, cx| s.open_workspace_named("git-review", w, cx));
    press(cx, &["cmd-shift-g"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active_git_feature(), Some(&feature)));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_move(w.find("git-tab").bounds().center(), None, cx);
        w.render_frame(cx);
        w.click("close-git", cx);
        assert_eq!(shark.read(cx).tab, Tab::Chat);
    })
    .unwrap();
    assert!(State::load().workspaces[0].git_closed);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_description_and_comments_cross_the_cli_boundary(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 2);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.as_ref().is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-description-edit", cx);
        w.render_frame(cx);
        w.click("git-editor-body", cx);
        w.press("cmd-a", cx);
        w.input(
            "## Updated\nKeep markdown, café, and `literal $HOME` intact.",
            cx,
        );
        w.click("git-editor-save", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.body.starts_with("## Updated"))
    })
    .await;
    let data: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("git-pr.json")).unwrap()).unwrap();
    assert_eq!(
        data["body"],
        "## Updated\nKeep markdown, café, and `literal $HOME` intact."
    );

    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-comment-new", cx);
        w.render_frame(cx);
        w.click("git-editor-body", cx);
        w.input("Retries use exponential backoff.", cx);
    })
    .unwrap();
    fs::write(dir.join("git-fail"), "offline").unwrap();
    cx.update_window(window, |_, w, cx| w.click("git-editor-save", cx))
        .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).busy()).await;
    cx.update(|cx| {
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Retries use exponential backoff.")
        )
    });
    let writes = fs::read_to_string(dir.join("git-writes.jsonl")).unwrap();
    assert_eq!(
        writes.lines().count(),
        1,
        "failed preflight never posts a comment"
    );
    fs::remove_file(dir.join("git-fail")).unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-editor-save", cx);
        // A repeated click while busy cannot post the same comment twice.
        w.click("git-editor-save", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.comments.len() == 2)
    })
    .await;
    let writes = fs::read_to_string(dir.join("git-writes.jsonl")).unwrap();
    assert_eq!(writes.lines().count(), 2);
    cx.update(|cx| assert!(feature.read(cx).draft_text(cx).is_none()));

    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.start_reply("T_0", w, cx));
        w.render_frame(cx);
        w.click("git-editor-body", cx);
        w.input("Covered by the reconnect test.", cx);
        w.click("git-editor-save", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.threads[0].comments.len() == 2)
    })
    .await;
    let writes = fs::read_to_string(dir.join("git-writes.jsonl")).unwrap();
    let last: Value = serde_json::from_str(writes.lines().last().unwrap()).unwrap();
    assert_eq!(last["id"], "T_0");
    assert_eq!(last["body"], "Covered by the reconnect test.");
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.resolve("T_0", true, w, cx));
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.threads[0].resolved)
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-show-resolved", cx);
        feature.update(cx, |f, cx| f.resolve("T_0", false, w, cx));
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| !d.threads[0].resolved)
    })
    .await;
    let writes = fs::read_to_string(dir.join("git-writes.jsonl")).unwrap();
    assert_eq!(
        writes.lines().count(),
        5,
        "save, comment, reply, resolve and reopen each happen once"
    );
    cx.update(|cx| feature.update(cx, |f, cx| f.retry_save(cx)));
    until(cx, window, |_, cx| !feature.read(cx).saving_drafts()).await;
    let drafts =
        fs::read_to_string(crate::state::support_dir().join("git-drafts/git-review.json")).unwrap();
    assert!(!drafts.contains("Retries use exponential backoff."));
    assert!(!drafts.contains("Covered by the reconnect test."));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_description_conflict_and_workspace_changes_keep_drafts(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    let press = press_in(window);
    press(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.as_ref().is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-description-edit", cx);
        w.render_frame(cx);
        w.click("git-editor-body", cx);
        w.press("cmd-a", cx);
        w.input("My unsaved description", cx);
    })
    .unwrap();
    let mut changed = pr_fixture(1);
    changed["body"] = json!("Another reviewer updated this description");
    fs::write(dir.join("git-pr.json"), changed.to_string()).unwrap();
    cx.update_window(window, |_, w, cx| w.click("git-editor-save", cx))
        .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).busy()).await;
    cx.update(|cx| {
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("My unsaved description")
        )
    });
    assert!(!dir.join("git-writes.jsonl").exists());
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_workspace_named("git-other", w, cx));
    press(cx, &["cmd-shift-g"]);
    let other = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        other.read(cx).inspect().details.as_ref().is_some()
    })
    .await;
    cx.update(|cx| assert!(other.read(cx).draft_text(cx).is_none()));
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_workspace_named("git-review", w, cx));
    press(cx, &["cmd-shift-g"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active_git_feature(), Some(&feature));
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("My unsaved description")
        );
    });
    press(cx, &["cmd-w", "cmd-shift-g"]);
    cx.update(|cx| {
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("My unsaved description")
        )
    });
    cx.update(|cx| feature.update(cx, |f, cx| f.retry_save(cx)));
    until(cx, window, |_, cx| !feature.read(cx).saving_drafts()).await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.git_features.remove("git-review");
        s.show(Tab::Feature(Feature::Git), w, cx);
    });
    let restored = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        restored.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        restored.update(cx, |f, cx| f.start_description(w, cx));
        assert_eq!(
            restored.read(cx).draft_text(cx).as_deref(),
            Some("My unsaved description")
        );
        w.render_frame(cx);
        w.click("git-editor-discard", cx);
        assert!(w.has_active_dialog(cx));
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| restored.update(cx, |f, cx| f.retry_save(cx)));
    until(cx, window, |_, cx| !restored.read(cx).saving_drafts()).await;
    let saved =
        fs::read_to_string(crate::state::support_dir().join("git-drafts/git-review.json")).unwrap();
    assert!(
        !saved.contains("My unsaved description"),
        "discard cannot be overwritten by an older queued draft save"
    );
    assert!(
        !dir.join("git-writes.jsonl").exists(),
        "discard is local only"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_large_review_virtualizes_and_releases(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1000);
    let target =
        crate::pr::git_feature::Target::from_url("https://github.com/acme/api/pull/7").unwrap();
    let details = crate::pr::git_feature::load(&dir, &target).unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    for count in [10, 100, 1000] {
        let mut sample = details.clone();
        sample.threads.truncate(count);
        let started = Instant::now();
        cx.update_window(window, |_, w, cx| {
            feature.update(cx, |f, cx| f.inject_details(sample, cx));
            w.render_frame(cx);
        })
        .unwrap();
        until(cx, window, |_, cx| {
            feature.read(cx).markdown_text_len("description", cx) > 0
        })
        .await;
        let open = started.elapsed();
        cx.update_window(window, |_, w, cx| {
            let started = Instant::now();
            for _ in 0..10 {
                feature.update(cx, |_, cx| cx.notify());
                w.render_frame(cx);
            }
            let frame = started.elapsed() / 10;
            eprintln!("Git review threads={count}: build+parsed first frame={open:?}; mean frame={frame:?}");
            assert!(w.try_find(format!("git-thread-reply-T_{}", count-1)).is_none(), "offscreen threads must not build their controls");
            assert!(frame < Duration::from_millis(100), "cached render has excessive work: {frame:?}");
        }).unwrap();
    }
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.scroll_row(204, cx));
        w.render_frame(cx);
        let offset = feature.read(cx).scroll_offset();
        assert!(offset.item_ix > 100);
        feature.update(cx, |f, cx| f.inject_details(details.clone(), cx));
        assert_eq!(
            feature.read(cx).scroll_offset().item_ix,
            offset.item_ix,
            "refreshing the same PR preserves the reading position"
        );
        feature.update(cx, |f, cx| f.scroll_row(0, cx));
        w.render_frame(cx);
    })
    .unwrap();
    let mut large = details;
    large.body = "A **review detail** with `inline code`, cafe, and a [link](https://github.com). "
        .repeat(900);
    large.body.truncate(65_000);
    large.comments[0].body = large.body.clone();
    let started = Instant::now();
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.inject_details(large, cx));
        w.render_frame(cx);
    })
    .unwrap();
    let (worst_frame, redraw) = settle_markdown(cx, window, &feature, "description").await;
    eprintln!(
        "Git 65k description: worst pending/parsed frame={worst_frame:?}; forced settled redraw={redraw:?}"
    );
    assert!(worst_frame < Duration::from_millis(100) && redraw < Duration::from_millis(100));
    eprintln!(
        "Git 65k Markdown description + 1000 threads: fully parsed+rendered={:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "large description parse/layout stalls the UI"
    );
    cx.update_window(window, |_, w, cx| {
        assert!(feature.read(cx).markdown_text_len("description", cx) < 4096);
        assert_eq!(
            feature
                .read(cx)
                .inspect()
                .details
                .as_ref()
                .unwrap()
                .body
                .len(),
            65_000
        );
        check_full_text_view(w, cx, &feature, "git-long-description");
    })
    .unwrap();
    let started = Instant::now();
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.scroll_row(2, cx));
        w.render_frame(cx);
    })
    .unwrap();
    let (worst_frame, redraw) = settle_markdown(cx, window, &feature, "C_1").await;
    eprintln!(
        "Git 65k C_1: worst pending/parsed frame={worst_frame:?}; forced settled redraw={redraw:?}"
    );
    assert!(worst_frame < Duration::from_millis(100) && redraw < Duration::from_millis(100));
    eprintln!(
        "Git 65k Markdown comment: scroll+fully parsed+rendered={:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "large comment parse/layout stalls the UI"
    );
    cx.update_window(window, |_, w, cx| {
        assert!(feature.read(cx).markdown_text_len("C_1", cx) < 4096);
        assert_eq!(
            feature
                .read(cx)
                .inspect()
                .details
                .as_ref()
                .unwrap()
                .comments[0]
                .body
                .len(),
            65_000
        );
        check_full_text_view(w, cx, &feature, "git-long-C_1");
    })
    .unwrap();
    let weak = feature.downgrade();
    drop(feature);
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.tab = Tab::Chat;
            s.git_features.clear();
            cx.notify();
        });
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        weak.upgrade().is_none(),
        "Git feature, markdown cache and subscriptions release together"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_loading_empty_and_retry_states(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let mut empty_workspace = state.workspaces[0].clone();
    empty_workspace.repos.clear();
    let (_gh, dir) = fake_github(&home, &state, 1);
    fs::write(dir.join("git-fail"), "offline").unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    let feature = cx
        .update_window(window, |_, w, cx| {
            w.press("cmd-shift-g", cx);
            let feature = shark.read(cx).active_git_feature().unwrap().clone();
            w.render_frame(cx);
            assert!(feature.read(cx).inspect().loading);
            assert!(w.find("git-loading").visible());
            assert!(w.try_find("git-empty-pr").is_none());
            feature
        })
        .unwrap();
    until(cx, window, |_, cx| {
        !feature.read(cx).inspect().loading && feature.read(cx).inspect().error.is_some()
    })
    .await;
    fs::remove_file(dir.join("git-fail")).unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("git-load-error").visible());
        assert_eq!(
            w.find_all("git-error").len(),
            1,
            "one readable error, not duplicate banners"
        );
        w.click("git-state-retry", cx);
        w.render_frame(cx);
        assert!(feature.read(cx).inspect().loading);
        assert!(w.find("git-loading").visible());
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some() && !feature.read(cx).inspect().loading
    })
    .await;
    let details = cx.update(|cx| feature.read(cx).inspect().details.as_ref().unwrap().clone());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("git-overview").visible());
        assert!(w.try_find("git-load-error").is_none());
        feature.update(cx, |f, cx| {
            f.inject_load_state(false, true, None, cx);
            cx.notify();
        });
        w.render_frame(cx);
        assert!(w.find("git-empty-pr").visible());
        assert!(w.find("git-state-retry").visible());
        let mut empty = details.clone();
        empty.body = " \n ".into();
        empty.comments.clear();
        empty.reviews.clear();
        empty.threads.clear();
        feature.update(cx, |f, cx| f.inject_details(empty.clone(), cx));
        w.render_frame(cx);
        assert!(w.find("git-empty-description").visible());
        assert!(w.find("git-empty-activity").visible());
        empty.threads = details.threads.clone();
        for thread in &mut empty.threads {
            thread.resolved = true;
        }
        feature.update(cx, |f, cx| f.inject_details(empty, cx));
        w.render_frame(cx);
        assert!(w.find("git-resolved-threads").visible());
        assert!(w.try_find("git-empty-activity").is_none());
        w.click("git-resolved-show", cx);
        w.render_frame(cx);
        assert!(w.try_find("git-resolved-threads").is_none());
        assert!(w.find(("git-thread-path", 0usize)).visible());
        feature.update(cx, |f, cx| f.set_workspace(empty_workspace, w, cx));
        w.render_frame(cx);
        assert!(w.find("git-empty-repositories").visible());
        assert!(w.try_find("git-state-retry").is_none());
    })
    .unwrap();
    assert!(!dir.join("git-writes.jsonl").exists());
}

/// Polls real background work (git, merman, image decoding) in the native harness.
fn settle(
    cx: &mut gpui_kit::HeadlessAppContext,
    window: AnyWindowHandle,
    mut ready: impl FnMut(&mut Window, &mut App) -> bool,
) {
    for _ in 0..500 {
        cx.run_until_parked();
        if cx
            .update_window(window, |_, w, cx| {
                w.render_frame(cx);
                ready(w, cx)
            })
            .unwrap()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("native Git render did not settle");
}

fn render_commits_pixels(
    cx: &mut gpui_kit::HeadlessAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    label: &str,
    width: f32,
) {
    use crate::features::git::commits::FileView;
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    let commits = cx.update(|cx| feature.read(cx).commits().clone());
    cx.update_window(window, |_, w, cx| {
        w.click("git-full-close", cx);
        w.render_frame(cx);
        w.click("git-view-commits", cx);
    })
    .unwrap();
    settle(cx, window, |_, cx| commits.read(cx).summaries().len() == 3);
    // Every named element is shown and stays inside the Git pane.
    let fits = |cx: &mut gpui_kit::HeadlessAppContext, name: &str, ids: Vec<ElementId>| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let panel = w.find("git-feature").bounds();
            for id in ids {
                let e = w.find(id.clone());
                let b = e.bounds();
                assert!(
                    e.visible() && b.left() >= panel.left() && b.right() <= panel.right(),
                    "{label}/{width}: {name}: {id}"
                );
            }
        })
        .unwrap();
        let shot = cx
            .capture_screenshot(window)
            .expect("Git commits Metal render");
        if let Some(output) = std::env::var_os("SHARK_RENDER_DIR") {
            shot.save(PathBuf::from(output).join(format!("{name}-{label}-{}.png", width as usize)))
                .unwrap();
        }
    };
    fits(
        cx,
        "git-commits",
        vec![("git-commit", 0usize).into(), "git-commits-refresh".into()],
    );
    cx.update_window(window, |_, w, cx| w.click(("git-commit", 1usize), cx))
        .unwrap();
    settle(cx, window, |w, cx| {
        commits.read(cx).commit().is_some() && w.try_find(("git-commit-file", 0usize)).is_some()
    });
    fits(
        cx,
        "git-commit",
        ["git-commit-subject", "git-commit-older", "git-commit-newer"]
            .map(ElementId::from)
            .to_vec(),
    );
    let readme = cx.update(|cx| {
        let c = commits.read(cx).commit().unwrap();
        (c.files.iter())
            .position(|f| f.path == "docs/readme.md")
            .unwrap()
    });
    cx.update(|cx| {
        commits.update(cx, |c, cx| {
            c.open_file_for_test(readme, FileView::Before, cx)
        })
    });
    settle(cx, window, |_, cx| {
        commits.read(cx).doc().is_some_and(|d| {
            let d = d.read(cx);
            d.diagram_state(2).is_some() && d.media_view(1).is_some_and(|m| m.read(cx).ready())
        })
    });
    fits(
        cx,
        "git-history-preview",
        vec![("git-mermaid", 2usize).into()],
    );
    cx.update_window(window, |_, w, cx| {
        assert!(w.find(("git-mermaid", 2usize)).bounds().size.height > px(20.));
        w.click("git-view-conversation", cx);
        w.render_frame(cx);
    })
    .unwrap();
}

/// Real Metal rendering of the complete Git artifact, including a draft composer.
/// Polls real background work (git, merman, image decoding) in the native harness.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (fixture, _, _) = history_fixture(&home);
    let (_gh, dir) = fake_github(&home, &fixture, 2);
    let target =
        crate::pr::git_feature::Target::from_url("https://github.com/acme/api/pull/7").unwrap();
    let mut details = crate::pr::git_feature::load(&dir, &target).unwrap();
    details.title =
        "Keep sign-in sessions reliable when reconnecting after a network interruption".into();
    details.reviews.push(crate::pr::git_feature::Review {
        comment: crate::pr::git_feature::Comment {
            id: "review-1".into(),
            body: "The reconnect flow reads well. Please keep the retry policy close to the session code.\n\n- Preserve the existing session\n- Back off after a failed reconnect".into(),
            ..details.comments[0].clone()
        },
        state: "CHANGES_REQUESTED".into(),
    });
    for width in [800., 1440.] {
        for (mode, label) in [
            (crate::state::Mode::Dark, "dark"),
            (crate::state::Mode::Light, "light"),
        ] {
            State {
                appearance: crate::state::Appearance {
                    mode,
                    ..Default::default()
                },
                ..Default::default()
            }
            .save()
            .unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(800.)),
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
                    let mut ws = fixture.workspaces[0].clone();
                    ws.git_closed = false;
                    ws.open_tab(TabId::Git);
                    s.active = Some(ws.name.clone());
                    s.state.workspaces = vec![ws];
                    s.home.on = false;
                    s.tab = Tab::Feature(Feature::Git);
                    s.sel = SideRow::Ws(0);
                    let feature = s.ensure_git_feature(w, cx).unwrap();
                    feature.update(cx, |f, cx| f.inject_details(details.clone(), cx));
                    cx.notify();
                });
                w.render_frame(cx);
            })
            .unwrap();
            for editing in [false, true] {
                cx.update_window(window, |_, w, cx| {
                    if editing {
                        shark
                            .read(cx)
                            .active_git_feature()
                            .unwrap()
                            .clone()
                            .update(cx, |f, cx| f.start_description(w, cx));
                    }
                    w.render_frame(cx);
                    let island = w.find("content-island").bounds();
                    let feature = w.find("git-feature").bounds();
                    assert!(w.find("feature-git").visible());
                    assert!(w.find("git-tab").visible());
                    assert_eq!(w.find("features-rail").bounds().size.width, px(72.));
                    assert!(feature.left() >= island.left() && feature.right() <= island.right());
                    for id in ["git-refresh", "git-description-edit", "git-comment-new"] {
                        let element = w.find(id);
                        assert!(element.visible(), "{label}/{width}: {id}");
                        assert!(
                            element.bounds().left() >= feature.left()
                                && element.bounds().right() <= feature.right()
                        );
                    }
                    let overview = w.find("git-overview").bounds();
                    assert!(overview.size.width <= px(680.));
                    assert!((overview.center().x - feature.center().x).abs() <= px(1.));
                    let title = w.find("git-pr-title").bounds();
                    assert!(
                        title.size.height > px(30.),
                        "long title must wrap, not truncate"
                    );
                    assert!(title.right() <= overview.right());
                    if editing {
                        let composer = w.find("git-composer").bounds();
                        assert_eq!(composer.left(), overview.left());
                        assert_eq!(composer.size.width, overview.size.width);
                        for id in [
                            "git-editor-title",
                            "git-editor-body",
                            "git-editor-save",
                            "git-editor-discard",
                        ] {
                            let element = w.find(id);
                            assert!(element.visible(), "{label}/{width}: {id}");
                            assert!(
                                element.bounds().bottom() <= feature.bottom(),
                                "{id} clipped below artifact"
                            );
                        }
                    }
                })
                .unwrap();
                let screenshot = cx.capture_screenshot(window).expect("Git Metal render");
                if let Some(output) = std::env::var_os("SHARK_RENDER_DIR") {
                    let output = PathBuf::from(output);
                    fs::create_dir_all(&output).unwrap();
                    screenshot
                        .save(output.join(format!(
                            "git-{label}-{}-editing-{editing}.png",
                            width as usize
                        )))
                        .unwrap();
                }
            }
            cx.update_window(window, |_, w, cx| {
                w.click("git-editor-cancel", cx);
                let feature = shark.read(cx).active_git_feature().unwrap().clone();
                feature.update(cx, |f, cx| f.scroll_row(3, cx));
                w.render_frame(cx);
                assert!(
                    w.try_find("git-pr-title")
                        .is_none_or(|title| !title.visible()),
                    "overview scrolls with the conversation"
                );
                assert!(
                    w.find("git-comment-new").visible(),
                    "comment action remains available while reading"
                );
            })
            .unwrap();
            let activity = cx
                .capture_screenshot(window)
                .expect("Git activity Metal render");
            if let Some(output) = std::env::var_os("SHARK_RENDER_DIR") {
                activity
                    .save(
                        PathBuf::from(output)
                            .join(format!("git-activity-{label}-{}.png", width as usize)),
                    )
                    .unwrap();
            }
            cx.update_window(window, |_, w, cx| {
                let feature = shark.read(cx).active_git_feature().unwrap().clone();
                feature.update(cx, |f, cx| f.start_reply("T_0", w, cx));
                w.render_frame(cx);
                w.input("Preserve the existing session after reconnecting.", cx);
                w.press("escape", cx);
                w.render_frame(cx);
                w.click("git-local-drafts", cx);
                w.render_frame(cx);
                let panel = w.find("git-feature").bounds();
                assert!(w.find("git-local-drafts-list").visible());
                for i in 0..2usize {
                    let draft = w.find(("git-local-draft", i));
                    assert!(draft.visible());
                    assert!(
                        draft.bounds().left() >= panel.left()
                            && draft.bounds().right() <= panel.right()
                    );
                }
            })
            .unwrap();
            let screenshot = cx
                .capture_screenshot(window)
                .expect("local Git drafts Metal render");
            if let Some(output) = std::env::var_os("SHARK_RENDER_DIR") {
                screenshot
                    .save(
                        PathBuf::from(output)
                            .join(format!("git-local-drafts-{label}-{}.png", width as usize)),
                    )
                    .unwrap();
            }
            cx.update_window(window, |_, w, cx| {
                w.click("git-local-drafts", cx);
                let feature = shark.read(cx).active_git_feature().unwrap().clone();
                feature.update(cx, |f, cx| f.scroll_row(0, cx));
                let mut long = details.clone();
                long.body = "Full Markdown source remains selectable and searchable. ".repeat(1300);
                long.body.truncate(65_000);
                feature.update(cx, |f, cx| f.inject_details(long, cx));
                w.render_frame(cx);
                w.click("git-long-description", cx);
                w.render_frame(cx);
                assert!(w.find("git-full-reader").visible());
                w.click("git-full-source", cx);
                w.render_frame(cx);
                assert_eq!(feature.read(cx).full_text_len(cx), 65_000);
                let panel = w.find("git-feature").bounds();
                let source = w.find("git-full-text");
                assert!(source.visible());
                assert!(
                    source.bounds().left() >= panel.left()
                        && source.bounds().right() <= panel.right()
                );
                assert!(source.bounds().bottom() <= panel.bottom());
                assert!(w.find("git-full-close").visible());
            })
            .unwrap();
            let screenshot = cx
                .capture_screenshot(window)
                .expect("full Git Markdown Metal render");
            if let Some(output) = std::env::var_os("SHARK_RENDER_DIR") {
                screenshot
                    .save(
                        PathBuf::from(output)
                            .join(format!("git-full-{label}-{}.png", width as usize)),
                    )
                    .unwrap();
            }
            render_commits_pixels(cx, window, &shark, label, width);
            for (state, expected) in [
                ("loading", "git-loading"),
                ("no-pr", "git-empty-pr"),
                ("error", "git-load-error"),
                ("refreshing", "git-overview"),
                ("stale", "git-error"),
                ("empty", "git-empty-activity"),
                ("resolved", "git-resolved-threads"),
                ("no-repositories", "git-empty-repositories"),
            ] {
                cx.update_window(window, |_, w, cx| {
                    let feature = shark.read(cx).active_git_feature().unwrap().clone();
                    feature.update(cx, |f, cx| {
                        let mut sample = details.clone();
                        if matches!(state, "empty" | "resolved") {
                            sample.body.clear();
                            sample.comments.clear();
                            sample.reviews.clear();
                            if state == "empty" {
                                sample.threads.clear();
                            } else {
                                for thread in &mut sample.threads {
                                    thread.resolved = true;
                                }
                            }
                        }
                        f.inject_details(sample, cx);
                        f.scroll_row(0, cx);
                        f.inject_load_state(matches!(state, "loading" | "refreshing"),
                            matches!(state, "loading" | "no-pr" | "error"),
                            matches!(state, "error" | "stale").then(|| "GitHub could not load this pull request because the request timed out. Check your connection and GitHub authentication, then retry. Your saved drafts are still available in this workspace.".into()), cx);
                        if state == "no-repositories" {
                            let mut workspace = fixture.workspaces[0].clone();
                            workspace.repos.clear();
                            f.set_workspace(workspace, w, cx);
                        }
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let panel = w.find("git-feature").bounds();
                    let content = w.find(expected);
                    assert!(content.visible(), "{label}/{width}/{state}: missing {expected}");
                    let bounds = content.bounds();
                    assert!(bounds.left() >= panel.left() && bounds.right() <= panel.right(), "{state}: content escapes the pane horizontally");
                    assert!(bounds.bottom() <= panel.bottom(), "{state}: state content is clipped");
                    if matches!(state, "no-pr" | "error") {
                        let retry = w.find("git-state-retry");
                        assert!(retry.visible());
                        assert!(retry.bounds().bottom() <= panel.bottom());
                    }
                    if matches!(state, "refreshing" | "stale") {
                        assert!(w.find("git-overview").visible());
                        assert!(w.try_find("git-loading").is_none());
                    }
                    if state == "empty" {
                        assert!(w.find("git-empty-description").visible());
                    }
                }).unwrap();
                let screenshot = cx
                    .capture_screenshot(window)
                    .expect("Git state Metal render");
                if let Some(output) = std::env::var_os("SHARK_RENDER_DIR") {
                    screenshot
                        .save(
                            PathBuf::from(output)
                                .join(format!("git-state-{state}-{label}-{}.png", width as usize)),
                        )
                        .unwrap();
                }
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    println!(
        "Git artifact: real Metal, dark/light, 800/1440px, composer, full Markdown and loading/empty/error states passed"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_thread_context_opens_diff_and_preserves_missing_context(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    fs::write(state.workspaces[0].dir().join("api/login.rs"), "retry();\n").unwrap();
    fs::write(
        state.workspaces[0].dir().join("api/other.rs"),
        "original();\n",
    )
    .unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    until(cx, window, |_, cx| {
        shark
            .read(cx)
            .differ
            .read(cx)
            .diff_files()
            .iter()
            .any(|f| f.path() == "api/login.rs")
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.differ
                .read(cx)
                .diff()
                .clone()
                .update(cx, |d, cx| d.set_file_collapsed("api/login.rs", true, cx));
            s.open_git_file("api/login.rs", w, cx);
            assert_eq!(s.tab, Tab::Feature(Feature::Diff));
            assert_eq!(
                s.differ.read(cx).diff_files()[s.differ.read(cx).diff_file()].path(),
                "api/login.rs"
            );
            assert!(
                !s.differ
                    .read(cx)
                    .diff()
                    .read(cx)
                    .is_file_collapsed("api/login.rs")
            );
            s.show(Tab::Feature(Feature::Git), w, cx);
            s.open_git_file("api/deleted.rs", w, cx);
            assert_eq!(
                s.tab,
                Tab::Feature(Feature::Git),
                "missing review context must not open an unrelated diff"
            );
        });
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/other.rs".into()), w, cx)
    });
    until(cx, window, |_, cx| {
        shark.read(cx).differ.read(cx).editing().is_some()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.differ
            .update(cx, |d, _| d.editing_mut().as_mut().unwrap().dirty = true);
        s.show(Tab::Feature(Feature::Git), w, cx);
        s.open_git_file("api/login.rs", w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx));
        w.press("escape", cx);
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Git));
        assert!(s.differ.read(cx).editing().as_ref().unwrap().dirty);
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_git_file("api/login.rs", w, cx));
    press_in(window)(cx, &["enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Diff));
        assert!(s.differ.read(cx).editing().is_none());
        assert_eq!(
            s.differ.read(cx).diff_files()[s.differ.read(cx).diff_file()].path(),
            "api/login.rs"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_repository_drafts_and_offline_refresh_are_isolated(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = fixture(&home);
    let ws = &mut state.workspaces[0];
    init_repo(&ws.dir().join("web"));
    ws.repos.push(crate::state::WsRepo {
        name: "web".into(),
        src: home.join("src/web"),
        base: "main".into(),
        branch: None,
    });
    let (_gh, dir) = fake_github(&home, &state, 1);
    let web = state.workspaces[0].dir().join("web");
    let mut second = pr_fixture(1);
    second["url"] = json!("https://github.com/acme/web/pull/9");
    second["number"] = json!(9);
    second["id"] = json!("PR_9");
    fs::write(web.join("git-pr.json"), second.to_string()).unwrap();
    git::git(
        &web,
        &[
            "config",
            "branch.main.shark-pr-url",
            "https://github.com/acme/web/pull/9",
        ],
    )
    .unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-comment-new", cx);
        w.render_frame(cx);
        w.input("API draft", cx);
        w.press("escape", cx);
        assert!(
            feature.read(cx).draft_text(cx).is_none(),
            "Escape keeps and closes draft"
        );
        w.render_frame(cx);
        w.click("git-repo-1", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.target.number == 9)
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-comment-new", cx);
        w.render_frame(cx);
        assert_eq!(feature.read(cx).draft_text(cx).as_deref(), Some(""));
        w.input("Web draft", cx);
        w.click("git-repo-0", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.target.number == 7)
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-comment-new", cx);
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("API draft")
        );
    })
    .unwrap();
    fs::write(dir.join("git-fail"), "offline").unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-refresh", cx);
        w.render_frame(cx);
        assert!(
            w.find("git-overview").visible(),
            "refresh retains the reading view"
        );
        assert!(
            w.try_find("git-loading").is_none(),
            "cached refresh does not replace content with a loading panel"
        );
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).inspect().loading).await;
    cx.update(|cx| {
        let f = feature.read(cx);
        assert!(f.inspect().error.as_ref().unwrap().contains("503"));
        assert_eq!(f.inspect().details.as_ref().unwrap().target.number, 7);
        assert_eq!(f.draft_text(cx).as_deref(), Some("API draft"));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.find("git-error").visible());
        assert!(w.find("git-overview").visible());
        assert!(w.try_find("git-load-error").is_none());
    })
    .unwrap();
    assert!(!dir.join("git-writes.jsonl").exists());
    assert!(!web.join("git-writes.jsonl").exists());
    fs::remove_file(dir.join("git-fail")).unwrap();
    for path in [&dir, &web] {
        fs::write(path.join("git-delay"), "0.15").unwrap();
    }
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| {
            for index in [1, 0, 1, 0] {
                f.select_repo(index, w, cx);
            }
            assert!(f.inspect().loading);
            assert_eq!(f.inspect().selected, 0);
        })
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).inspect().loading).await;
    cx.update(|cx| {
        let f = feature.read(cx);
        assert_eq!(f.inspect().selected, 0);
        assert_eq!(
            f.inspect().details.as_ref().unwrap().target.number,
            7,
            "delayed stale result cannot replace latest selected PR"
        );
        assert!(f.inspect().error.is_none());
    });
    assert!(
        !home.join("git-overlap").exists(),
        "rapid switches must coalesce behind one in-flight request"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_corrupt_drafts_are_protected_and_reported(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, _) = fake_github(&home, &state, 1);
    let path = crate::state::support_dir().join("git-drafts/git-review.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{my original damaged draft").unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.start_comment(w, cx));
        w.render_frame(cx);
        w.input("new draft must not destroy the old file", cx);
        feature.update(cx, |f, cx| f.retry_save(cx));
        assert!(feature.read(cx).inspect().save_error.is_some());
        assert!(!shark.read(cx).unsaved_data(None, cx).is_empty());
    })
    .unwrap();
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "{my original damaged draft"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_stale_description_requires_reviewed_replacement(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.start_description(w, cx));
        w.render_frame(cx);
        w.press("cmd-a", cx);
        w.input("My reviewed replacement", cx);
    })
    .unwrap();
    let mut changed = pr_fixture(1);
    changed["body"] = json!("Latest remote description");
    fs::write(dir.join("git-pr.json"), changed.to_string()).unwrap();
    cx.update_window(window, |_, w, cx| {
        w.press("cmd-s", cx);
        assert!(
            feature.read(cx).busy(),
            "save shortcut dispatches from textarea"
        );
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).busy()).await;
    assert!(!dir.join("git-writes.jsonl").exists());
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |f, cx| f.refresh(w, cx))
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).inspect().loading).await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-description-replace", cx);
        assert!(w.has_active_dialog(cx));
        w.press("escape", cx);
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("My reviewed replacement")
        );
    })
    .unwrap();
    assert!(!dir.join("git-writes.jsonl").exists());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-description-replace", cx);
        w.press("enter", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.body == "My reviewed replacement")
    })
    .await;
    let writes = fs::read_to_string(dir.join("git-writes.jsonl")).unwrap();
    assert_eq!(
        writes.lines().count(),
        1,
        "only the confirmed reviewed replacement writes to GitHub"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_first_open_during_archive_cannot_load_or_post(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.archiving.insert("git-review".into());
        s.show(Tab::Feature(Feature::Git), w, cx);
        let feature = s.active_git_feature().unwrap().clone();
        assert!(feature.read(cx).inspect().archiving);
        assert!(!feature.read(cx).inspect().loading);
        feature.update(cx, |f, cx| {
            f.start_comment(w, cx);
            f.submit(w, cx);
            assert!(!f.busy());
            assert!(f.draft_text(cx).is_none());
        });
    });
    cx.update_window(window, |_, w, cx| {
        let feature = shark.read(cx).active_git_feature().unwrap().clone();
        w.render_frame(cx);
        assert!(w.find("git-unloaded").visible());
        w.click("git-state-retry", cx);
        assert!(
            !feature.read(cx).inspect().loading,
            "archiving keeps the load action disabled"
        );
    })
    .unwrap();
    assert!(!dir.join("git-writes.jsonl").exists());
}

async fn settle_markdown(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    feature: &Entity<crate::features::git::GitFeature>,
    key: &str,
) -> (Duration, Duration) {
    let mut worst = Duration::ZERO;
    for _ in 0..1000 {
        let ready = cx
            .update_window(window, |_, w, cx| {
                let frame = Instant::now();
                w.render_frame(cx);
                worst = worst.max(frame.elapsed());
                feature.read(cx).markdown_text_len(key, cx) > 1000
            })
            .unwrap();
        if ready {
            let redraw = cx
                .update_window(window, |_, w, cx| {
                    feature.update(cx, |_, cx| cx.notify());
                    let frame = Instant::now();
                    w.render_frame(cx);
                    frame.elapsed()
                })
                .unwrap();
            return (worst, redraw);
        }
        cx.executor().timer(Duration::from_millis(10)).await;
    }
    panic!("Git Markdown did not finish parsing");
}

fn check_full_text_view(
    w: &mut Window,
    cx: &mut App,
    feature: &Entity<crate::features::git::GitFeature>,
    button: &'static str,
) {
    let source = feature
        .read(cx)
        .inspect()
        .details
        .as_ref()
        .unwrap()
        .body
        .clone();
    let offset = feature.read(cx).scroll_offset().item_ix;
    let started = Instant::now();
    w.click(button, cx);
    w.render_frame(cx);
    let read = started.elapsed();
    let reader = feature
        .read(cx)
        .reader()
        .expect("the rich reader opens")
        .clone();
    assert_eq!(
        reader.read(cx).source().len(),
        65_000,
        "nothing beyond 4 KB is cut"
    );
    assert!(
        read < Duration::from_millis(300),
        "the virtualized reader lays out only visible blocks: {read:?}"
    );
    let started = Instant::now();
    w.click("git-full-source", cx);
    w.render_frame(cx);
    let opened = started.elapsed();
    assert_eq!(feature.read(cx).full_text_len(cx), 65_000);
    w.input("must stay read-only", cx);
    assert_eq!(feature.read(cx).full_text_len(cx), 65_000);
    w.press("cmd-a", cx);
    w.press("cmd-c", cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some(source)
    );
    feature.update(cx, |_, cx| cx.notify());
    let started = Instant::now();
    w.render_frame(cx);
    let redraw = started.elapsed();
    eprintln!("Git65k full readonly source {button}: open={opened:?}; redraw={redraw:?}");
    assert!(
        opened < Duration::from_millis(100) && redraw < Duration::from_millis(100),
        "full Markdown source must remain responsive"
    );
    w.click("git-full-close", cx);
    w.render_frame(cx);
    assert_eq!(
        feature.read(cx).scroll_offset().item_ix,
        offset,
        "returning from full text preserves the conversation position"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn feature_rail_keys_clamp_and_focus_walks_feature_columns(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, fixture(&home), WindowOptions::default());
    let press = press_in(window);
    press(cx, &["cmd-shift-d", "cmd-alt-right"]);
    cx.update_window(window, |_, w, cx| {
        assert_eq!(
            shark.read(cx).focused_pane(w, cx),
            Some(Pane::Feature(Feature::Diff, FeatureFocus::Files))
        );
    })
    .unwrap();
    press(cx, &["cmd-alt-right", "up", "up"]);
    cx.update_window(window, |_, w, cx| {
        assert_eq!(shark.read(cx).focused_pane(w, cx), Some(Pane::Features));
        assert_eq!(shark.read(cx).feature_sel, 0);
    })
    .unwrap();
    press(cx, &["down", "down", "enter"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Git));
        assert_eq!(s.feature_sel, 1);
        assert_eq!(
            s.focused_pane(w, cx),
            Some(Pane::Feature(Feature::Git, FeatureFocus::Main))
        );
    })
    .unwrap();
    press(cx, &["cmd-alt-right", "cmd-alt-right", "up", "enter"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Feature(Feature::Diff));
        assert_eq!(
            s.focused_pane(w, cx),
            Some(Pane::Feature(Feature::Diff, FeatureFocus::Main))
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn inactive_git_write_blocks_close_and_archive(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    fs::write(dir.join("git-delay"), "0.15").unwrap();
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        w.render_frame(cx);
        w.input("Pending write survives switching workspaces", cx);
        feature.update(cx, |g, cx| g.submit(w, cx));
        assert!(feature.read(cx).busy());
        shark.update(cx, |s, cx| {
            s.open_workspace_now(1, w, cx);
            assert!(!s.storage_guard(true, w, cx));
            assert!(!s.close_window(w, cx));
            s.remove_workspace("git-review", false, w, cx);
            assert!(!s.archiving.contains("git-review"));
            assert!(s.ws("git-review").is_some());
        });
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).busy()).await;
    assert_eq!(
        fs::read_to_string(dir.join("git-writes.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_home_save_keeps_hidden_draft_local(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        w.render_frame(cx);
        w.input("Keep this comment local", cx);
        shark.update(cx, |s, cx| s.enter_home(w, cx));
        w.render_frame(cx);
        w.dispatch_action(SaveFile.boxed_clone(), cx);
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        assert!(s.home_shown());
        let command = palette::COMMANDS
            .iter()
            .find(|row| row.0 == "Save file")
            .unwrap();
        command.run(s, w, cx);
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(!feature.read(cx).busy());
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Keep this comment local")
        );
    });
    assert!(!dir.join("git-writes.jsonl").exists());
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_keyboard_reaches_offscreen_threads_without_stealing_input(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, _) = fake_github(&home, &state, 100);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    let press = press_in(window);
    press(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    on_app(window, shark.clone())(cx, &|s, _, cx| s.set_vim(false, cx));
    let offset = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let at = feature.read(cx).scroll_offset();
            (at.item_ix, at.offset_in_item)
        })
    };
    press(cx, &["home", "down"]);
    assert!(offset(cx) != (0, px(0.)));
    press(cx, &["up"]);
    assert_eq!(offset(cx), (0, px(0.)));
    press(cx, &["pagedown"]);
    assert!(offset(cx) != (0, px(0.)));
    press(cx, &["pageup", "home"]);
    assert_eq!(offset(cx), (0, px(0.)));
    press(cx, &["end"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("git-thread-reply-T_99").is_some());
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| s.set_vim(true, cx));
    press(cx, &["g", "g", "j"]);
    assert!(offset(cx) != (0, px(0.)));
    press(cx, &["k"]);
    assert_eq!(offset(cx), (0, px(0.)));
    press(cx, &["ctrl-d"]);
    assert!(offset(cx) != (0, px(0.)));
    press(cx, &["ctrl-u", "g", "g"]);
    assert_eq!(offset(cx), (0, px(0.)));
    press(cx, &["shift-g"]);
    assert!(offset(cx).0 > 90);
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        w.render_frame(cx);
    })
    .unwrap();
    let before = offset(cx);
    press(cx, &["j", "k", "g", "g", "down", "up", "home", "end"]);
    assert_eq!(
        offset(cx),
        before,
        "conversation shortcuts must not run in an input"
    );
    cx.update(|cx| assert_eq!(feature.read(cx).draft_text(cx).as_deref(), Some("jkgg")));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_slow_save_keeps_input_responsive_and_discard_ordered(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, _) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    let press = press_in(window);
    press(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| {
            g.delay_next_save(Duration::from_secs(60));
            g.start_comment(w, cx);
        });
        w.render_frame(cx);
        w.input("older snapshot", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| feature.read(cx).save_started() > 0).await;
    cx.update_window(window, |_, w, cx| {
        assert!(feature.read(cx).saving_drafts());
        w.input(" plus newer input", cx);
        w.render_frame(cx);
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("older snapshot plus newer input")
        );
        w.click("git-editor-discard", cx);
        w.press("enter", cx);
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.open_workspace_now(1, w, cx);
        assert!(!s.storage_guard(true, w, cx));
        s.remove_workspace("git-review", false, w, cx);
        assert!(!s.archiving.contains("git-review"));
    });
    cx.executor().advance_clock(Duration::from_secs(61));
    until(cx, window, |_, cx| !feature.read(cx).saving_drafts()).await;
    let saved =
        fs::read_to_string(crate::state::support_dir().join("git-drafts/git-review.json")).unwrap();
    assert!(
        !saved.contains("older snapshot"),
        "a completed old write cannot resurrect a discarded draft"
    );
    on_app(window, shark)(cx, &|s, w, cx| assert!(s.storage_guard(true, w, cx)));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_failed_draft_save_retries_without_losing_text(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, _) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    let directory = crate::state::support_dir().join("git-drafts");
    fs::write(&directory, "blocks directory creation").unwrap();
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        w.render_frame(cx);
        w.input("Recover this draft", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature.read(cx).storage_error().is_some()
    })
    .await;
    cx.update(|cx| {
        assert!(!feature.read(cx).saving_drafts());
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Recover this draft")
        );
        assert!(!shark.read(cx).unsaved_data(None, cx).is_empty());
    });
    fs::remove_file(&directory).unwrap();
    cx.update(|cx| feature.update(cx, |g, cx| g.retry_save(cx)));
    until(cx, window, |_, cx| {
        !feature.read(cx).saving_drafts() && feature.read(cx).storage_error().is_none()
    })
    .await;
    assert!(
        fs::read_to_string(directory.join("git-review.json"))
            .unwrap()
            .contains("Recover this draft")
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn qa_git_post_restores_conversation_keyboard_navigation(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, _) = fake_github(&home, &state, 100);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        w.render_frame(cx);
        w.input("Continue reading after posting", cx);
        w.press("cmd-s", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature
            .read(cx)
            .inspect()
            .details
            .as_ref()
            .is_some_and(|d| d.comments.len() == 2)
            && !feature.read(cx).inspect().loading
    })
    .await;
    press_in(window)(cx, &["end"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            feature.read(cx).scroll_offset().item_ix > 90,
            "after posting removes the focused composer, End must still navigate the conversation"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn qa_git_saved_draft_accessible_after_permissions_revoked(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 1);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        w.render_frame(cx);
        w.input("Recover my saved local comment", cx);
        w.press("escape", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).saving_drafts()).await;
    assert!(
        fs::read_to_string(crate::state::support_dir().join("git-drafts/git-review.json"))
            .unwrap()
            .contains("Recover my saved local comment")
    );
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.start_description(w, cx));
        w.render_frame(cx);
        w.press("cmd-a", cx);
        w.input("Local description after permission loss", cx);
        w.press("escape", cx);
        feature.update(cx, |g, cx| g.start_reply("T_0", w, cx));
        w.render_frame(cx);
        w.input("Local reply after permission loss", cx);
        w.press("escape", cx);
    })
    .unwrap();
    let mut locked = pr_fixture(1);
    locked["locked"] = json!(true);
    locked["viewerCanUpdate"] = json!(false);
    locked["reviewThreads"]["nodes"][0]["viewerCanReply"] = json!(false);
    fs::write(dir.join("git-pr.json"), locked.to_string()).unwrap();
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.refresh(w, cx));
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).inspect().loading).await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        feature.update(cx, |g, cx| g.start_comment(w, cx));
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Recover my saved local comment"),
            "remote permissions must not prevent reading or copying a saved local draft"
        );
        w.render_frame(cx);
        w.press("cmd-s", cx);
        assert!(!feature.read(cx).busy());
        feature.update(cx, |g, cx| g.start_description(w, cx));
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Local description after permission loss")
        );
        w.render_frame(cx);
        w.press("cmd-s", cx);
        assert!(!feature.read(cx).busy());
        feature.update(cx, |g, cx| g.start_reply("T_0", w, cx));
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Local reply after permission loss")
        );
        w.render_frame(cx);
        w.press("cmd-s", cx);
        assert!(!feature.read(cx).busy());
    })
    .unwrap();
    assert!(!dir.join("git-writes.jsonl").exists());
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_feature_local_drafts_recover_offline_and_keep_remote_targets(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let state = fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 0);
    let target = "https://github.com/acme/api/pull/7";
    let other = "https://github.com/acme/api/pull/8";
    let path = crate::state::support_dir().join("git-drafts/git-review.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, json!({
        target: {
            "description": {"original_title":"Original title", "original_body":"Original body", "title":"Local title", "body":"Local description"},
            "note":"Local comment", "replies":{"deleted-thread":"Local reply"}
        },
        other: {"description":null, "note":"Other PR comment", "replies":{}}
    }).to_string()).unwrap();
    fs::write(dir.join("git-fail"), "offline").unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().error.is_some() && !feature.read(cx).inspect().loading
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-local-drafts", cx);
    })
    .unwrap();
    for (index, text) in [
        "Local description",
        "Local comment",
        "Local reply",
        "Other PR comment",
    ]
    .into_iter()
    .enumerate()
    {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click(("git-local-draft", index), cx);
            w.render_frame(cx);
            assert_eq!(feature.read(cx).draft_text(cx).as_deref(), Some(text));
            w.press("cmd-a", cx);
            w.press("cmd-c", cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                Some(text)
            );
            w.press("cmd-s", cx);
            assert!(!feature.read(cx).busy(), "offline drafts cannot post");
            w.press("escape", cx);
        })
        .unwrap();
    }
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(("git-local-draft", 1usize), cx);
        w.render_frame(cx);
        w.press("cmd-a", cx);
        w.input("Edited offline", cx);
        w.press("escape", cx);
        w.render_frame(cx);
        w.click(("git-local-draft", 2usize), cx);
        w.render_frame(cx);
        w.click("git-editor-discard", cx);
        w.press("enter", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| !feature.read(cx).saving_drafts()).await;
    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved[target]["note"], "Edited offline");
    assert_eq!(
        saved[target]["description"]["original_body"],
        "Original body"
    );
    assert!(saved[target]["replies"].as_object().unwrap().is_empty());
    assert_eq!(saved[other]["note"], "Other PR comment");

    fs::remove_file(dir.join("git-fail")).unwrap();
    cx.update_window(window, |_, w, cx| {
        feature.update(cx, |g, cx| g.refresh(w, cx))
    })
    .unwrap();
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some() && !feature.read(cx).inspect().loading
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(("git-local-draft", 2usize), cx);
        w.render_frame(cx);
        w.press("cmd-s", cx);
        assert!(
            !feature.read(cx).busy(),
            "another PR's draft cannot post to the loaded PR"
        );
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("Other PR comment")
        );
    })
    .unwrap();
    assert!(!dir.join("git-writes.jsonl").exists());
}

fn png(w: u32, h: u32) -> Vec<u8> {
    resvg::tiny_skia::Pixmap::new(w, h)
        .unwrap()
        .encode_png()
        .unwrap()
}

fn history_fixture(home: &Path) -> (State, PathBuf, [String; 3]) {
    let mut state = fixture(home);
    let wt = state.workspaces[0].dir().join("api");
    git::git(&wt, &["branch", "start"]).unwrap();
    state.workspaces[0].repos[0].base = "start".into();
    fs::create_dir_all(wt.join("docs")).unwrap();
    fs::write(
        wt.join("docs/readme.md"),
        "# Doc\n\n![logo](logo.png)\n\n```mermaid\nflowchart TD\n  A --> B\n```\n",
    )
    .unwrap();
    fs::write(wt.join("docs/logo.png"), png(1, 1)).unwrap();
    fs::write(wt.join("gone.txt"), "bye\n").unwrap();
    let first = commit_all(&wt, "Add docs");
    fs::write(wt.join("docs/readme.md"), "# Doc v2\n\n![logo](logo.png)\n").unwrap();
    fs::write(wt.join("docs/logo.png"), png(2, 3)).unwrap();
    fs::remove_file(wt.join("gone.txt")).unwrap();
    let second = commit_all(&wt, "Update docs\n\nWith a body.");
    let third = commit_all(&wt, "Empty");
    // Later local work must never leak into history views.
    fs::write(wt.join("docs/readme.md"), "UNSAVED WORKING COPY\n").unwrap();
    fs::write(wt.join("docs/logo.png"), b"not a png").unwrap();
    fs::write(wt.join("new.txt"), "untracked\n").unwrap();
    git::git(&wt, &["add", "new.txt"]).unwrap();
    (state, wt, [first, second, third])
}

fn repo_snapshot(wt: &Path) -> (String, String, String, Vec<u8>) {
    (
        git::git(wt, &["rev-parse", "HEAD"]).unwrap(),
        git::git(wt, &["status", "--porcelain"]).unwrap(),
        git::git(wt, &["ls-files", "-s"]).unwrap(),
        fs::read(wt.join("docs/readme.md")).unwrap(),
    )
}

/// The commit browser opened on [`history_fixture`], newest commit first in `shas`.
struct Browser {
    window: AnyWindowHandle,
    shark: Entity<Shark>,
    feature: Entity<GitFeature>,
    commits: Entity<Commits>,
    wt: PathBuf,
    /// [`repo_snapshot`] before the app touched the repository.
    before: (String, String, String, Vec<u8>),
    shas: [String; 3],
}

async fn open_commits(cx: &mut TestAppContext) -> Browser {
    let home = test_home();
    let (state, wt, [first, second, third]) = history_fixture(&home);
    let before = repo_snapshot(&wt);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-view-commits", cx);
    })
    .unwrap();
    let commits = cx.update(|cx| feature.read(cx).commits().clone());
    until(cx, window, |_, cx| commits.read(cx).summaries().len() == 3).await;
    Browser {
        window,
        shark,
        feature,
        commits,
        wt,
        before,
        shas: [third, second, first],
    }
}

impl Browser {
    fn assert_untouched(&self) {
        assert_eq!(
            repo_snapshot(&self.wt),
            self.before,
            "HEAD, index and working files are untouched"
        );
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_commits_list_keyboard_and_selection(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let b = open_commits(cx).await;
    let (window, commits) = (b.window, b.commits.clone());
    let [third, second, first] = b.shas.clone();
    cx.update_window(window, |_, w, cx| {
        let shas: Vec<_> = (commits.read(cx).summaries().iter())
            .map(|c| c.sha.clone())
            .collect();
        assert_eq!(
            shas,
            [third.clone(), second.clone(), first.clone()],
            "newest first"
        );
        // Keyboard: down to the second commit, enter opens it.
        w.press("down", cx);
        w.press("enter", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| commits.read(cx).commit().is_some()).await;
    let files = cx
        .update_window(window, |_, w, cx| {
            let c = commits.read(cx).commit().unwrap();
            assert_eq!(c.sha, second);
            assert!(w.find("git-commit-message").visible());
            assert!(w.find("git-commit-comparison").visible());
            (c.files.iter())
                .map(|f| (f.path.clone(), f.status))
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(files.contains(&("gone.txt".into(), 'D')), "{files:?}");

    // Older reaches the root of the range; Escape walks out.
    cx.update(|cx| commits.update(cx, |c, cx| c.step_for_test(true, cx)));
    until(cx, window, |_, cx| {
        commits.read(cx).commit().is_some_and(|c| c.sha == first)
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-view-conversation", cx);
        w.render_frame(cx);
        w.click("git-view-commits", cx);
        w.render_frame(cx);
        assert_eq!(
            commits.read(cx).commit().unwrap().sha,
            first,
            "selection survives view switches"
        );
        w.press("escape", cx);
        w.render_frame(cx);
        assert!(commits.read(cx).commit().is_none());
        // Escape at the list root keeps the arrows on the list.
        w.press("escape", cx);
        w.render_frame(cx);
        let at = commits.read(cx).cursor();
        w.press(if at == 0 { "down" } else { "up" }, cx);
        assert_ne!(commits.read(cx).cursor(), at);
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| w.click(("git-commit", 0usize), cx))
        .unwrap();
    until(cx, window, |w, cx| {
        commits.read(cx).commit().is_some_and(|c| c.sha == third)
            && w.try_find("git-commit-empty").is_some()
    })
    .await;
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_commit_files_read_each_side_of_the_commit(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let b = open_commits(cx).await;
    let (window, commits) = (b.window, b.commits.clone());
    cx.update(|cx| commits.update(cx, |c, cx| c.open_commit(1, cx)));
    until(cx, window, |_, cx| commits.read(cx).commit().is_some()).await;
    let files: Vec<String> = cx.update(|cx| {
        let c = commits.read(cx).commit().unwrap();
        assert_eq!(c.sha, b.shas[1]);
        c.files.iter().map(|f| f.path.clone()).collect()
    });
    let ix = |p: &str| files.iter().position(|f| f == p).unwrap();
    let open = |cx: &mut TestAppContext, i: usize, view: FileView| {
        cx.update(|cx| commits.update(cx, |c, cx| c.open_file_for_test(i, view, cx)));
    };
    let shown = |cx: &mut TestAppContext| cx.update(|cx| commits.read(cx).shown(cx));

    open(cx, ix("docs/readme.md"), FileView::After);
    until(cx, window, |_, cx| {
        commits.read(cx).shown(cx).starts_with("text:")
    })
    .await;
    assert_eq!(shown(cx), "text:# Doc v2\n\n![logo](logo.png)\n");
    // The preview resolves its image from this commit's tree, not the working file.
    let doc = cx.update(|cx| commits.read(cx).doc().unwrap().clone());
    until(cx, window, |_, cx| {
        (doc.read(cx).media_view(1)).is_some_and(|m| m.read(cx).ready())
    })
    .await;
    cx.update(|cx| {
        let m = doc.read(cx).media_view(1).unwrap();
        assert_eq!(m.read(cx).shown(1).unwrap().1, Some((2, 3)));
    });

    open(cx, ix("docs/readme.md"), FileView::Before);
    until(cx, window, |_, cx| {
        commits.read(cx).shown(cx).starts_with("text:")
    })
    .await;
    assert!(shown(cx).starts_with("text:# Doc\n"), "{}", shown(cx));
    // Blocks of the parent's readme: 0 heading, 1 logo, 2 mermaid.
    let doc = cx.update(|cx| commits.read(cx).doc().unwrap().clone());
    cx.wait_for(window, Duration::from_secs(20), |_, cx| {
        let d = doc.read(cx);
        d.media_view(1).is_some_and(|m| m.read(cx).ready()) && d.diagram_state(2).is_some()
    })
    .await;
    cx.update(|cx| {
        let d = doc.read(cx);
        let m = d.media_view(1).unwrap();
        assert_eq!(
            m.read(cx).shown(1).unwrap().1,
            Some((1, 1)),
            "Before reads the parent"
        );
        assert!(
            d.diagram_state(2).unwrap().unwrap().exists(),
            "Mermaid rendered"
        );
    });
    cx.update(|cx| commits.update(cx, |c, cx| c.set_source(true, cx)));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("git-file-text").visible(),
            "Source opens the read-only editor"
        );
    })
    .unwrap();
    cx.update(|cx| commits.update(cx, |c, cx| c.set_source(false, cx)));

    open(cx, ix("docs/readme.md"), FileView::Diff);
    until(cx, window, |_, cx| {
        commits.read(cx).shown(cx).starts_with("diff:")
    })
    .await;

    for (view, size) in [(FileView::Before, (1, 1)), (FileView::After, (2, 3))] {
        open(cx, ix("docs/logo.png"), view);
        until(cx, window, |_, cx| {
            commits.read(cx).media().is_some_and(|m| m.read(cx).ready())
        })
        .await;
        cx.update(|cx| {
            let m = commits.read(cx).media().unwrap().read(cx);
            let side = if view == FileView::Before { 0 } else { 1 };
            assert_eq!(m.shown(side).unwrap().1, Some(size));
        });
    }
    open(cx, ix("docs/logo.png"), FileView::Diff);
    until(cx, window, |_, cx| {
        commits.read(cx).media().is_some_and(|m| m.read(cx).ready())
    })
    .await;

    open(cx, ix("gone.txt"), FileView::After);
    until(cx, window, |_, cx| {
        commits.read(cx).shown(cx).starts_with("text:")
    })
    .await;
    assert_eq!(
        shown(cx),
        "text:bye\n",
        "a deleted file exposes only its earlier side"
    );
    b.assert_untouched();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_commits_survive_rapid_switching_and_a_missing_base(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let b = open_commits(cx).await;
    let (window, commits) = (b.window, b.commits.clone());
    // Smoke check: both loads race, so which result lands last is not forced; the selection
    // must settle on the last request.
    cx.update(|cx| {
        commits.update(cx, |c, cx| {
            c.open_commit(1, cx);
            c.open_commit(2, cx);
        })
    });
    until(cx, window, |_, cx| commits.read(cx).commit().is_some()).await;
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(commits.read(cx).commit().unwrap().sha, b.shas[2]));

    // A missing base is an explicit error, never all of history.
    let ws = cx.update(|cx| {
        let mut ws = b.shark.read(cx).active_ws().unwrap().clone();
        ws.repos[0].base = "origin/nope".into();
        ws
    });
    cx.update_window(window, |_, w, cx| {
        b.feature.update(cx, |g, cx| g.set_workspace(ws, w, cx))
    })
    .unwrap();
    until(cx, window, |_, cx| {
        (commits.read(cx).error()).is_some_and(|e| e.contains("origin/nope"))
    })
    .await;
    cx.update(|cx| assert!(commits.read(cx).summaries().is_empty()));
    b.assert_untouched();
}

/// A PR whose description is the rich-content fixture, open on the Git tab.
struct RichPr {
    _gh: GhGuard,
    dir: PathBuf,
    window: AnyWindowHandle,
    feature: Entity<GitFeature>,
    head: String,
}

async fn open_rich_pr(cx: &mut TestAppContext) -> RichPr {
    let home = test_home();
    let (state, _, [_, second, _]) = history_fixture(&home);
    let (_gh, dir) = fake_github(&home, &state, 0);
    // The PR head is captured with the conversation; relative assets come from that commit.
    let mut pr: Value =
        serde_json::from_slice(&fs::read(dir.join("git-pr.json")).unwrap()).unwrap();
    pr["headRefOid"] = json!(second);
    // Blocks: 0 heading, 1 mermaid, 2 logo, 3 missing image, 4 insecure image, 5 the unclosed
    // fence plus long text (the tests below index into these).
    pr["body"] = json!(format!(
        "## Why\n\n```mermaid\nflowchart LR\n  A --> B\n```\n\n![logo](docs/logo.png)\n\n\
         ![missing](docs/none.png)\n\n![insecure](http://insecure.test/a.png)\n\n```mermaid\nflowchart\n\n{}tail-marker\n",
        "long paragraph text. ".repeat(400)
    ));
    fs::write(dir.join("git-pr.json"), serde_json::to_vec(&pr).unwrap()).unwrap();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    press_in(window)(cx, &["cmd-shift-g"]);
    let feature = cx.update(|cx| shark.read(cx).active_git_feature().unwrap().clone());
    until(cx, window, |_, cx| {
        feature.read(cx).inspect().details.is_some()
    })
    .await;
    RichPr {
        _gh,
        dir,
        window,
        feature,
        head: second,
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_conversation_renders_mermaid_and_images_from_the_pr_head(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pr = open_rich_pr(cx).await;
    let (window, feature) = (pr.window, pr.feature.clone());
    cx.update(|cx| {
        let details = feature.read(cx).inspect().details.as_ref().unwrap();
        assert_eq!(details.head_sha.as_deref(), Some(pr.head.as_str()));
    });
    let doc = |cx: &App| {
        feature
            .read(cx)
            .markdown_doc("description")
            .unwrap()
            .clone()
    };
    cx.wait_for(window, Duration::from_secs(20), |_, cx| {
        let Some(doc) = feature.read(cx).markdown_doc("description") else {
            return false;
        };
        let d = doc.read(cx);
        d.diagram_state(1).is_some()
            && d.media_view(2).is_some_and(|m| m.read(cx).ready())
            && d.media_error(3).is_some()
    })
    .await;
    cx.update(|cx| {
        let doc = doc(cx).read(cx);
        assert!(doc.diagram_state(1).unwrap().is_ok());
        let logo = doc.media_view(2).unwrap();
        assert_eq!(
            logo.read(cx).shown(1).unwrap().1,
            Some((2, 3)),
            "the PR head's blob, not the working file"
        );
        assert!(doc.media_error(3).unwrap().contains("not found"));
    });
    until(cx, window, |_, cx| {
        doc(cx).read(cx).media_error(4).is_some()
    })
    .await;
    cx.update(|cx| assert!(doc(cx).read(cx).media_error(4).unwrap().contains("HTTPS")));
    // The full reader keeps content past the 4 KB preview (the unclosed fence stays source).
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-long-description", cx);
        w.render_frame(cx);
        let reader = feature.read(cx).reader().unwrap().read(cx);
        assert!(reader.source().ends_with("tail-marker\n"));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn git_reader_and_view_switches_never_post_a_hidden_draft(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pr = open_rich_pr(cx).await;
    let (window, feature) = (pr.window, pr.feature.clone());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("git-comment-new", cx);
        w.render_frame(cx);
        w.input("draft survives", cx);
        w.render_frame(cx);
        w.click("git-long-description", cx);
        w.render_frame(cx);
        // cmd-s in the reader never posts the hidden comment.
        w.press("cmd-s", cx);
        w.press("escape", cx);
        w.render_frame(cx);
        w.click("git-view-commits", cx);
        w.render_frame(cx);
        w.press("cmd-s", cx);
        w.click("git-view-conversation", cx);
        w.render_frame(cx);
        assert_eq!(
            feature.read(cx).draft_text(cx).as_deref(),
            Some("draft survives")
        );
    })
    .unwrap();
    assert!(
        !pr.dir.join("git-writes.jsonl").exists(),
        "nothing was posted"
    );
}
