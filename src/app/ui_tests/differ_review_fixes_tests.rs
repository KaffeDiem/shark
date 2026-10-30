//! Review-comment fixes in the Diff tab: resolve menu, old-side revs, viewed marks, line
//! cursor, jumps, drafts across workspaces, edit-working-copy targets.
use super::*;
use crate::diff_view::{DiffLineRange, DiffSide};
use crate::features::differ::test_support::diff::Viewed;

fn commit_all(dir: &Path) {
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
            "c",
        ],
    )
    .unwrap();
}

/// A booted app with workspace `name` on repo `api`, whose main has `base` committed; returns
/// the workspace's repo dir.
async fn fixture(
    cx: &mut TestAppContext,
    name: &str,
    base: &[(&str, &str)],
) -> (PathBuf, AnyWindowHandle, Entity<Shark>, PathBuf) {
    let home = test_home();
    let repos = repos(&home, &["api"]);
    for (f, text) in base {
        fs::write(repos[0].path.join(f), text).unwrap();
    }
    if !base.is_empty() {
        commit_all(&repos[0].path);
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
    let branch = name.to_string();
    on(cx, &|s, w, cx| {
        s.create_workspace(
            branch.clone(),
            git::on_branch(&repos, &branch),
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
    let api = home.join("shark/workspaces").join(name).join("api");
    (home, window, shark, api)
}

async fn show_diff(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    shark: &Entity<Shark>,
    files: usize,
) {
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.show(Tab::Feature(Feature::Diff), w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_files().len() == files
    })
    .await;
}

fn thread(id: u64, path: &str, old: bool, line: usize, status: &str, rev: &str) -> review::Comment {
    serde_json::from_str(&format!(
        r#"{{"id":{id},"path":"{path}","start":[{old},{line}],"end":[{old},{line}],"text":"x","status":{status},"rev":"{rev}"}}"#
    ))
    .unwrap()
}

fn add(
    cx: &mut TestAppContext,
    shark: &Entity<Shark>,
    window: AnyWindowHandle,
    c: Vec<review::Comment>,
) {
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.differ
            .update(cx, |d, cx| d.edit_comments(cx, |l| l.extend(c.clone())))
    });
}

fn selected(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Option<String> {
    cx.update(|cx| {
        let d = shark.read(cx).differ.read(cx);
        (d.diff().read(cx).selected_lines()).map(|r| r.path().to_string())
    })
}

/// Resolve hides a thread for good, so only a sent one offers it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn thread_menu_resolves_only_sent_threads(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "rm", &[]).await;
    fs::write(api.join("f"), "a\nb\n").unwrap();
    show_diff(cx, window, &shark, 1).await;
    add(
        cx,
        &shark,
        window,
        vec![
            thread(1, "api/f", false, 1, r#""Pending""#, ""),
            thread(2, "api/f", false, 1, r#""Queued""#, ""),
            thread(3, "api/f", false, 1, r#"{"Sent":{"turn":1}}"#, ""),
        ],
    );
    let resolve = |cx: &mut TestAppContext, id: u64| {
        shark.update(cx, |s, cx| {
            (s.differ.update(cx, |d, cx| d.thread_menu(id, cx)).iter())
                .any(|e| matches!(e, Entry::Item { label, .. } if label == "Resolve"))
        })
    };
    assert!(!resolve(cx, 1), "a pending thread would vanish unsent");
    assert!(!resolve(cx, 2), "so would a queued one");
    assert!(resolve(cx, 3));
    fs::remove_dir_all(&home).ok();
}

/// An old-side thread counts lines in the commit it was made against; another compare commit
/// (Uncommitted mode compares against HEAD) leaves it off its line.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn old_side_thread_follows_its_compare_commit(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "rv", &[("f", "a\nb\n")]).await;
    fs::write(api.join("f"), "a\nB\n").unwrap();
    commit_all(&api);
    fs::write(api.join("f"), "a\nC\n").unwrap();
    let rev = git::git(&api, &["merge-base", "main", "HEAD"]).unwrap();
    show_diff(cx, window, &shark, 1).await;
    add(
        cx,
        &shark,
        window,
        vec![thread(1, "api/f", true, 2, r#""Pending""#, &rev)],
    );
    let on_line = |cx: &App| {
        let d = shark.read(cx).differ.read(cx);
        d.on_its_line(d.comment(1).unwrap())
    };
    cx.update(|cx| assert!(on_line(cx)));
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.toggle_uncommitted(w, cx))
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| !on_line(cx))
        .await;
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.toggle_uncommitted(w, cx))
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| on_line(cx))
        .await;
    fs::remove_dir_all(&home).ok();
}

/// Switching to Uncommitted and back neither drops marks of files outside it nor marks a
/// viewed file changed.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn viewed_marks_survive_the_uncommitted_toggle(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "vw", &[("f", "a\n")]).await;
    fs::write(api.join("f"), "b\n").unwrap();
    fs::write(api.join("h"), "h\n").unwrap();
    commit_all(&api);
    fs::write(api.join("f"), "c\n").unwrap();
    show_diff(cx, window, &shark, 2).await;
    let on = on_app(window, shark.clone());
    let viewed = |cx: &mut TestAppContext, p: &str| {
        cx.update(|cx| shark.read(cx).differ.read(cx).viewed_path(p))
    };
    on(cx, &|s, _, cx| {
        s.differ.update(cx, |d, cx| {
            d.toggle_viewed_path("api/f", cx);
            d.toggle_viewed_path("api/h", cx);
        })
    });
    assert_eq!(viewed(cx, "api/f"), Some(Viewed::Yes));
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.toggle_uncommitted(w, cx))
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_files().len() == 1
    })
    .await;
    assert_eq!(
        viewed(cx, "api/f"),
        Some(Viewed::Yes),
        "another old side only"
    );
    // h is outside this mode; marking f again must not forget it.
    on(cx, &|s, _, cx| {
        s.differ.update(cx, |d, cx| {
            d.toggle_viewed_path("api/f", cx);
            d.toggle_viewed_path("api/f", cx);
        })
    });
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.toggle_uncommitted(w, cx))
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).diff_files().len() == 2
    })
    .await;
    assert_eq!(viewed(cx, "api/h"), Some(Viewed::Yes));
    assert_eq!(viewed(cx, "api/f"), Some(Viewed::Yes));
    fs::remove_dir_all(&home).ok();
}

/// Moving to another file (tree, open_path) drops the line cursor left in the previous one.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn moving_to_a_file_clears_the_line_cursor(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "sel", &[]).await;
    fs::write(api.join("f"), "a\n").unwrap();
    fs::write(api.join("g"), "a\n").unwrap();
    show_diff(cx, window, &shark, 2).await;
    let on = on_app(window, shark.clone());
    let select = |cx: &mut TestAppContext| {
        on(cx, &|s, _, cx| {
            s.differ.update(cx, |d, cx| {
                let range = DiffLineRange::new("api/f".to_string(), DiffSide::Modified, 1, 1);
                d.diff()
                    .update(cx, |d, cx| d.set_selected_lines(Some(range), cx))
            })
        });
        assert_eq!(selected(cx, &shark).as_deref(), Some("api/f"));
    };
    select(cx);
    on(cx, &|s, _, cx| {
        s.differ.update(cx, |d, cx| {
            let row = d
                .tree_rows()
                .iter()
                .position(|r| r.file == Some(1))
                .unwrap();
            d.tree_select(row, cx)
        })
    });
    assert_eq!(selected(cx, &shark), None, "tree_select");
    select(cx);
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.open_path("api/g", w, cx));
    });
    assert_eq!(selected(cx, &shark), None, "open_path");
    fs::remove_dir_all(&home).ok();
}

/// A jump to a thread unfolds its viewed file; one on a file left the diff (an orphan) goes
/// nowhere, and `]c` skips it instead of scrolling to the top.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn jumping_to_threads_unfolds_files_and_skips_orphans(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "jp", &[]).await;
    fs::write(api.join("f"), "a\nb\n").unwrap();
    show_diff(cx, window, &shark, 1).await;
    add(
        cx,
        &shark,
        window,
        vec![
            thread(1, "api/f", false, 1, r#""Pending""#, ""),
            thread(2, "api/gone", false, 1, r#""Pending""#, ""),
            thread(3, "api/f", false, 2, r#""Pending""#, ""),
        ],
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, _, cx| {
        s.differ
            .update(cx, |d, cx| d.toggle_viewed_path("api/f", cx))
    });
    let folded = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let d = shark.read(cx).differ.read(cx);
            d.diff().read(cx).is_file_collapsed("api/f")
        })
    };
    assert!(folded(cx), "viewed folds the file");
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.jump_to_comment(1, w, cx))
    });
    assert!(!folded(cx));
    assert_eq!(selected(cx, &shark).as_deref(), Some("api/f"));
    on(cx, &|s, w, cx| {
        s.differ.update(cx, |d, cx| d.jump_to_comment(2, w, cx))
    });
    assert_eq!(
        selected(cx, &shark).as_deref(),
        Some("api/f"),
        "orphan: no jump"
    );
    // The cursor is on thread 1: `]c` cycles 3, 1, 3 and never lands on the orphan.
    for want in [3, 1, 3] {
        on(cx, &|s, w, cx| {
            s.differ.update(cx, |d, cx| d.thread_jump(true, w, cx))
        });
        let at = cx.update(|cx| shark.read(cx).differ.read(cx).thread_at(cx));
        assert_eq!(at, Some(want), "]c");
    }
    fs::remove_dir_all(&home).ok();
}

/// Leaving a workspace with words in the comment box keeps them as a pending comment there.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn switching_workspace_keeps_the_typed_comment(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "w1", &[]).await;
    fs::write(api.join("f"), "a\n").unwrap();
    show_diff(cx, window, &shark, 1).await;
    let press = press_in(window);
    press(cx, &["down", "c"]);
    cx.update_window(window, |_, w, cx| w.input("draft", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(shark.read(cx).differ.read(cx).review().composer.is_some()));
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.create_workspace(
            "w2".into(),
            git::on_branch(&repos, "w2"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).differ.read(cx).workspace_name() == Some("w2")
    })
    .await;
    cx.update(|cx| {
        let r = shark.read(cx).differ.read(cx).review();
        assert!(r.composer.is_none());
        let kept = r.comments.get("w1").expect("w1's comments");
        assert_eq!((kept.len(), kept[0].text.as_str()), (1, "draft"));
        assert_eq!(kept[0].status, review::Status::Pending);
    });
    fs::remove_dir_all(&home).ok();
}

/// Edit Working Copy on a file outside the diff stays on the tab and says so.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn edit_working_copy_outside_the_diff_does_not_switch_tabs(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark, api) = fixture(cx, "ew", &[]).await;
    fs::write(api.join("f"), "a\n").unwrap();
    show_diff(cx, window, &shark, 1).await;
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/missing".into()), w, cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.tab == Tab::Chat, "stays where it was");
        assert!(s.differ.read(cx).editing().is_none());
    });
    fs::remove_dir_all(&home).ok();
}
