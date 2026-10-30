use super::*;
use crate::attention::Attn;
use crate::state::WsRepo;

/// A workspace `one` made from a repo `api`; the app has it open.
async fn one_workspace(cx: &mut TestAppContext) -> (PathBuf, AnyWindowHandle, Entity<Shark>) {
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.create_workspace(
            "one".into(),
            git::on_branch(&repos, "one"),
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
    (home, window, shark)
}

fn fake(name: &str) -> Workspace {
    Workspace {
        name: name.into(),
        branch: name.into(),
        ..Default::default()
    }
}

/// A workspace made from Home with a dirty file tab in the one left asks before it opens; the
/// first message goes out once the answer is Discard, and the prompt stays until then.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_prompt_waits_for_discard_dialog(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (home, window, shark) = one_workspace(cx).await;
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    fs::write(home.join("shark/workspaces/one/api/a.txt"), "a\n").unwrap();
    on(cx, &|s, w, cx| s.open_path("api/a.txt".into(), w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.is_some()
    })
    .await;
    cx.update(|cx| {
        let editor = shark.read(cx).editor.clone().unwrap();
        editor.update(cx, |e, _| e.dirty = true)
    });
    press(cx, &["cmd-0"]);
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let args = quiet.with_extension("args");
    let _ = fs::remove_file(&args);
    on(cx, &|s, w, cx| {
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("build it", w, cx));
        s.create_workspace(
            "two".into(),
            git::on_branch(&repos, "two"),
            vec![],
            Some("build it".into()),
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.len() == 2
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx), "the dirty tab asks first");
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("one"));
        assert_eq!(s.home.prompt.read(cx).value().as_ref(), "build it", "kept");
    })
    .unwrap();
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("two")
    })
    .await;
    let sent = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        args.exists()
    });
    assert!(sent, "the first message started claude in `two`");
    cx.update(|cx| {
        assert_eq!(shark.read(cx).home.prompt.read(cx).value().as_ref(), "");
    });
    fs::remove_dir_all(&home).ok();
}

/// Archiving moves both cursors: the sidebar's stays on its own workspace, and either one
/// that sat on the archived row goes to the row that took its place.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn archive_keeps_cursors_on_their_rows(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    let names = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            let SideRow::Ws(ix) = s.sel else { return None };
            Some(s.state.workspaces[ix].name.clone())
        })
    };
    on(cx, &|s, w, cx| {
        s.state.workspaces = ["a", "b", "c", "d"].map(fake).to_vec();
        s.sel = SideRow::Ws(2);
        s.enter_home(w, cx);
        s.home.sel = Some("c".into());
        s.home.focus.focus(w, cx);
        // Another workspace goes: the cursor stays on `c`, not on whatever has index 2.
        s.archived("a", Ok(vec![]), w, cx);
    });
    assert_eq!(names(cx).as_deref(), Some("c"));
    on(cx, &|s, w, cx| s.archived("c", Ok(vec![]), w, cx));
    assert_eq!(names(cx).as_deref(), Some("d"), "the row that moved up");
    cx.update(|cx| {
        assert_eq!(shark.read(cx).home.sel.as_deref(), Some("d"));
    });
    press(cx, &["enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("d")));
}

/// cmd-/ and Start a new conversation on Home act on the chat and show it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn chat_commands_leave_home(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_workspace(cx).await;
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    let shown = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).home_shown());
    press(cx, &["cmd-0"]);
    assert!(shown(cx));
    on(cx, &|s, w, cx| s.pick_model(w, cx));
    assert!(!shown(cx), "the model menu is on a chat in view");
    press(cx, &["escape", "cmd-0"]);
    assert!(shown(cx));
    let row = COMMANDS
        .iter()
        .position(|r| r.0 == "Start a new conversation")
        .unwrap();
    on(cx, &|s, w, cx| (COMMANDS[row].4)(s, w, cx));
    assert!(!shown(cx), "new conversation shows its chat");
    fs::remove_dir_all(&home).ok();
}

/// A turn that ends while Home covers the workspace is unread; going back reads it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn turn_done_under_home_is_unread(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_workspace(cx).await;
    let press = press_in(window);
    let state = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            (s.attention_of("one", cx), s.chat_unread("one"))
        })
    };
    let chat = cx.update(|cx| shark.read(cx).active_chat().cloned().unwrap());
    let done = |cx: &mut TestAppContext| {
        cx.update(|cx| chat.update(cx, |_, cx| cx.emit(crate::chat::ChatEvent::TurnDone)));
        cx.run_until_parked();
    };
    done(cx);
    assert_eq!(state(cx), (Attn::None, false), "on screen: read");
    press(cx, &["cmd-0"]);
    done(cx);
    assert_eq!(state(cx), (Attn::Unread, true), "under Home: unread");
    cx.update(|cx| shark.update(cx, |s, _| s.unread.clear()));
    assert_eq!(state(cx).0, Attn::Unread, "the workspace itself too");
    press(cx, &["cmd-0"]);
    assert_eq!(state(cx), (Attn::None, false), "back in: read");
    fs::remove_dir_all(&home).ok();
}

/// cmd-r and the target of palette commands are the highlighted Home row, not the workspace
/// behind Home, wherever the focus is.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_row_is_the_target(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let settings = crate::state::RepoSettings {
        run: vec!["sleep 30".into()],
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            repo_settings: [("api".to_string(), settings)].into(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.state.workspaces = ["one", "two"]
            .map(|n| Workspace {
                repos: vec![WsRepo {
                    name: "api".into(),
                    src: home.join("api"),
                    base: "main".into(),
                    branch: None,
                }],
                ..fake(n)
            })
            .to_vec();
        for ws in &s.state.workspaces {
            fs::create_dir_all(ws.dir().join("api")).unwrap();
        }
        s.active = Some("one".into());
        s.enter_home(w, cx);
        s.home.sel = Some("two".into());
        s.home.prompt.update(cx, |p, cx| p.focus(w, cx));
    });
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        let target = s.target(w).map(|t| t.name);
        assert_eq!(target.as_deref(), Some("two"), "the prompt has focus");
    })
    .unwrap();
    on(cx, &|s, w, cx| s.toggle_run(w, cx));
    cx.update(|cx| {
        let runs = &shark.read(cx).runs;
        assert!(runs.contains_key("two") && !runs.contains_key("one"));
    });
    on(cx, &|s, w, cx| s.toggle_run(w, cx));
    cx.update(|cx| assert!(shark.read(cx).runs.is_empty(), "again stops it"));
    fs::remove_dir_all(&home).ok();
}

/// Cancelling the discard dialog keeps the old workspace open, but the new one still gets
/// its setup scripts; the Home prompt stays as typed.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cancelled_open_still_sets_up_the_new_workspace(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_workspace(cx).await;
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    fs::write(home.join("shark/workspaces/one/api/a.txt"), "a\n").unwrap();
    on(cx, &|s, w, cx| s.open_path("api/a.txt".into(), w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.is_some()
    })
    .await;
    on(cx, &|s, _, cx| {
        s.editor.clone().unwrap().update(cx, |e, _| e.dirty = true);
        s.state.repo_settings.insert(
            "api".into(),
            crate::state::RepoSettings {
                setup: vec!["echo $SHARK_WORKSPACE > $SHARK_ROOT_PATH/setup.txt".into()],
                ..Default::default()
            },
        );
    });
    press(cx, &["cmd-0"]);
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    // $SHARK_ROOT_PATH is the source repo; setup starts as soon as the dialog is up.
    let wt = repos[0].path.clone();
    let _ = fs::remove_file(wt.join("setup.txt"));
    on(cx, &|s, w, cx| {
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("build it", w, cx));
        s.create_workspace(
            "two".into(),
            git::on_branch(&repos, "two"),
            vec![],
            Some("build it".into()),
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.len() == 2
    })
    .await;
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press(cx, &["escape"]);
    let set_up = (0..200).any(|_| {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
        fs::read_to_string(wt.join("setup.txt")).is_ok_and(|s| s == "two\n")
    });
    assert!(set_up, "setup ran for the workspace that didn't open");
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("one"));
        assert_eq!(s.home.prompt.read(cx).value().as_ref(), "build it");
    });
    fs::remove_dir_all(&home).ok();
}
