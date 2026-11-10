use super::*;
use crate::attention::Attn;
use crate::state::TaskRepo;
use gpui_kit::test::TestQueryExt;

/// A task `one` made from a repo `api`; the app has it open.
async fn one_task(cx: &mut TestAppContext) -> (PathBuf, AnyWindowHandle, Entity<Shark>) {
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
        s.create_task_for_test(
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

fn fake(name: &str) -> Task {
    Task {
        name: name.into(),
        branch: name.into(),
        ..Default::default()
    }
}

/// A task made from Home with a dirty file tab in the one left asks before it opens; the
/// first message goes out once the answer is Discard, and the prompt stays until then.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_prompt_waits_for_discard_dialog(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (home, window, shark) = one_task(cx).await;
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
        s.create_task_for_test(
            "two".into(),
            git::on_branch(&repos, "two"),
            vec![],
            Some("build it".into()),
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.tasks.len() == 2
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

/// Archiving moves both cursors: the sidebar's stays on its own task, and either one
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
            let SideRow::Task(ix) = s.sel else {
                return None;
            };
            Some(s.state.tasks[ix].name.clone())
        })
    };
    on(cx, &|s, w, cx| {
        s.state.tasks = ["a", "b", "c", "d"].map(fake).to_vec();
        s.sel = SideRow::Task(2);
        s.enter_home(w, cx);
        s.home.sel = Some("c".into());
        s.home.focus.focus(w, cx);
        // Another task goes: the cursor stays on `c`, not on whatever has index 2.
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
    let (home, window, shark) = one_task(cx).await;
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
    on(cx, &|s, w, cx| COMMANDS[row].run(s, w, cx));
    assert!(!shown(cx), "new conversation shows its chat");
    fs::remove_dir_all(&home).ok();
}

/// A turn that ends while Home covers the task is unread; going back reads it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn turn_done_under_home_is_unread(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_task(cx).await;
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

/// cmd-r and the target of palette commands are the highlighted Home row, not the task
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
        s.state.tasks = ["one", "two"]
            .map(|n| Task {
                repos: vec![TaskRepo {
                    name: "api".into(),
                    src: home.join("api"),
                    base: "main".into(),
                    branch: None,
                }],
                ..fake(n)
            })
            .to_vec();
        for ws in &s.state.tasks {
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

/// Cancelling the discard dialog keeps the old task open, but the new one still gets
/// its setup scripts; the Home prompt stays as typed.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cancelled_open_still_sets_up_the_new_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_task(cx).await;
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
        s.create_task_for_test(
            "two".into(),
            git::on_branch(&repos, "two"),
            vec![],
            Some("build it".into()),
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.tasks.len() == 2
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

/// Home offers Start from Linear issue only where Linear is in use.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_offers_linear_only_while_it_is_in_use(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_task(cx).await;
    let on = on_app(window, shark.clone());
    let offered = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("draft-add", cx);
        })
        .unwrap();
        cx.run_until_parked();
        let offered = cx
            .update_window(window, |_, w, cx| {
                w.render_frame(cx);
                w.try_find("draft-context-uuid-1").is_some()
            })
            .unwrap();
        press_in(window)(cx, &["escape"]);
        offered
    };
    on(cx, &|s, _, cx| {
        s.home.on = true;
        cx.notify();
    });
    assert!(!offered(cx), "never connected");
    on(cx, &|s, _, cx| {
        s.state.linear = Some(Default::default());
        let org = s.linear_org_id().unwrap_or_default().to_string();
        s.state.tasks[0].issues.push(Snapshot {
            org,
            id: "uuid-1".into(),
            identifier: "SHA-42".into(),
            title: "Example issue".into(),
            ..Default::default()
        });
        cx.notify();
    });
    assert!(offered(cx), "connected");
    on(cx, &|s, _, cx| {
        s.state.linear = None;
        cx.notify();
    });
    assert!(!offered(cx), "disconnected, nothing attached");
    on(cx, &|s, _, cx| {
        s.state.tasks[0].linear.attach(crate::linear::IssueRef {
            org: "acme-uuid".into(),
            issue: "uuid-1".into(),
        });
        cx.notify();
    });
    assert!(
        !offered(cx),
        "disconnected issues remain in pills, not current suggestions"
    );
    fs::remove_dir_all(&home).ok();
}

/// Palette rows with no handler of their own dispatch their action, as its key would.
/// The keyboard's way to what `+N` lists, with nothing pending or failed to bring it up.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn palette_shows_attachments_with_nothing_pending(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_task(cx).await;
    let press = press_in(window);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.state.tasks[0].attaching.is_empty());
        assert!(!s.state.tasks[0].repos.is_empty());
    });
    press(cx, &["cmd-k"]);
    cx.update_window(window, |_, w, cx| w.input("Show attachments", cx))
        .unwrap();
    press(cx, &["enter"]);
    press(cx, &["enter"]);
    cx.update(|cx| {
        assert!(
            shark.read(cx).menu.is_some(),
            "the ready repo opens its branch menu"
        );
    });
    cx.update_window(window, |_, w, _| {
        assert!(w.try_find("attach-panel").is_none());
    })
    .unwrap();
    press(cx, &["c", "enter"]);
    let branch = cx.update(|cx| {
        let s = shark.read(cx);
        let ws = &s.state.tasks[0];
        ws.branch_of(&ws.repos[0]).to_string()
    });
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text())),
        Some(branch),
        "the ready repository's branch can be copied from Show attachments"
    );
    fs::remove_dir_all(&home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn palette_rows_dispatch_their_actions(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (home, window, shark) = one_task(cx).await;
    let press = press_in(window);
    let pick = |cx: &mut TestAppContext, query: &str| {
        press(cx, &["cmd-k"]);
        cx.update_window(window, |_, w, cx| w.input(query, cx))
            .unwrap();
        press(cx, &["enter"]);
    };
    let chats = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).state.tasks[0].chats.len());
    let before = chats(cx);
    pick(cx, "New chat");
    assert_eq!(chats(cx), before + 1, "New chat");
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(String::new())));
    pick(cx, "Copy branch name");
    let branch = cx.update(|cx| shark.read(cx).state.tasks[0].branch.clone());
    assert_eq!(
        cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text())),
        Some(branch),
        "Copy branch name"
    );
    pick(cx, "Toggle Zen mode");
    cx.update(|cx| assert!(shark.read(cx).zen_shown(), "Toggle Zen mode"));
    fs::remove_dir_all(&home).ok();
}

/// j/k and gg/G stop at the first and last sidebar row.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn sidebar_vim_keys_clamp(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            vim: true,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.state.tasks = ["one", "two", "three"].map(fake).to_vec();
        s.sidebar_focus.focus(w, cx);
    });
    let press = press_in(window);
    let sel = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).sel);
    press(cx, &["shift-g"]);
    assert_eq!(sel(cx), SideRow::Task(2));
    press(cx, &["j"]);
    assert_eq!(sel(cx), SideRow::Task(2), "j stops at the last row");
    press(cx, &["g", "g"]);
    assert_eq!(sel(cx), SideRow::Task(0));
    press(cx, &["k"]);
    assert_eq!(sel(cx), SideRow::Task(0), "k stops at the first row");
    press(cx, &["j", "j"]);
    assert_eq!(sel(cx), SideRow::Task(2));
    fs::remove_dir_all(&home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn agent_prompt_survives_failed_creation_and_agent_switching(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let (home, window, shark) = one_task(cx).await;
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    fs::write(home.join("shark/workspaces/one/api/a.txt"), "a\n").unwrap();
    on(cx, &|s, w, cx| {
        s.create_named_agent("Designer", cx).unwrap();
        s.create_named_agent("Reviewer", cx).unwrap();
        s.open_path("api/a.txt".into(), w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editor.is_some()
    })
    .await;
    on(cx, &|s, w, cx| {
        s.editor.as_ref().unwrap().update(cx, |e, _| e.dirty = true);
        s.state.read_only = true;
        s.prefill_agent_task("designer", w, cx);
        s.home
            .agent_prompt
            .update(cx, |p, cx| p.set_value("preserve this prompt", w, cx));
    });
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.tasks.len(), 1);
        assert_eq!(s.home.agent_prompt.read(cx).value(), "preserve this prompt");
    });
    on(cx, &|s, w, cx| {
        s.editor
            .as_ref()
            .unwrap()
            .update(cx, |e, _| e.dirty = false);
        s.state.read_only = true;
        s.home.agent_prompt.update(cx, |p, cx| p.focus(w, cx));
    });
    press(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.tasks.len(), 1);
        assert_eq!(s.home.agent_prompt.read(cx).value(), "preserve this prompt");
    });
    on(cx, &|s, w, cx| {
        s.state.read_only = false;
        s.prefill_agent_task("reviewer", w, cx);
        s.home
            .agent_prompt
            .update(cx, |p, cx| p.set_value("same prompt", w, cx));
        s.home_prompt_used(Some("same prompt"), Some("designer"), w, cx);
        assert_eq!(
            s.home.agent_prompt.read(cx).value(),
            "same prompt",
            "another agent's pending task cannot clear this prompt"
        );
        s.prefill_agent_task("designer", w, cx);
        assert_eq!(
            s.home.agent_prompt.read(cx).value(),
            "preserve this prompt",
            "each agent keeps its own draft"
        );
    });
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.tasks.len() == 2
    })
    .await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.tasks.len(), 2);
        assert_eq!(s.state.tasks[1].agent.as_ref().unwrap().slug, "designer");
        assert_eq!(
            s.home.agent_prompt.read(cx).value(),
            "",
            "an empty agent submission clears after success"
        );
    });
}

/// The visible picker shares the typed mention, and standalone selection clears only it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_agent_picker_preserves_draft_and_standalone_choice(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("fix spacing", w, cx))
    });
    let pick = |cx: &mut TestAppContext, query: &str| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("home-agent-picker", cx);
            w.input(query, cx);
        })
        .unwrap();
        press(cx, &["enter"]);
    };
    pick(cx, "Designer");
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.prompt.read(cx).value(),
            "@designer fix spacing"
        )
    });
    pick(cx, "No agent");
    cx.read(|cx| assert_eq!(shark.read(cx).home.prompt.read(cx).value(), "fix spacing"));
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.tasks.len() == 1
    })
    .await;
    cx.read(|cx| assert!(shark.read(cx).state.tasks[0].agent.is_none()));
    on(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    press(cx, &["cmd-n"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.home.destination,
            super::super::home::Destination::Agent("designer".into()),
            "cmd-n on an agent page stays there"
        );
        assert_eq!(s.state.tasks.len(), 1);
    });
    on(cx, &|s, w, cx| {
        s.home
            .agent_prompt
            .update(cx, |p, cx| p.set_value("another task", w, cx))
    });
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.tasks.len() == 2
    })
    .await;
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            s.state
                .tasks
                .iter()
                .any(|task| task.agent.as_ref().is_some_and(|a| a.slug == "designer"))
        );
        assert_eq!(s.home.agent_prompt.read(cx).value(), "");
    });
    fs::remove_dir_all(home).ok();
}

/// Group labels cannot merge distinct groups or change agent membership.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn all_tasks_groups_keep_ungrouped_separate_and_agent_instructions(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let link = crate::state::AgentLink {
        slug: "designer".into(),
        instructions: "Preserve these instructions".into(),
        ..Default::default()
    };
    let mut grouped = fake("grouped");
    grouped.agent = Some(link.clone());
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![grouped, fake("ungrouped")],
            sections: vec![
                Section {
                    name: "Ungrouped".into(),
                    icon: "folder".into(),
                    tasks: vec!["grouped".into()],
                    collapsed: false,
                    agent: None,
                },
                Section {
                    name: "Legacy agent group".into(),
                    icon: "bot".into(),
                    tasks: vec!["ungrouped".into()],
                    collapsed: false,
                    agent: Some("designer".into()),
                },
            ],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.enter_home(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("task-grouping", cx);
        w.input("Task group", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find(("home-group", 0usize)).is_some());
        assert!(
            w.try_find(("home-group", 2usize)).is_some(),
            "a plain group named Ungrouped must not absorb ungrouped tasks"
        );
        let s = shark.read(cx);
        assert_eq!(
            s.home_rows(cx).len(),
            2,
            "legacy bound groups do not hide tasks"
        );
        assert_eq!(s.state.tasks[0].agent.as_ref(), Some(&link));
    })
    .unwrap();
    on(cx, &|s, _, cx| {
        s.move_to_section("grouped", None, cx);
        assert_eq!(s.state.tasks[0].agent.as_ref(), Some(&link));
        assert_eq!(s.home_rows(cx).len(), 2);
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| {
        assert_eq!(
            shark.read(cx).home.destination,
            super::super::home::Destination::Home
        )
    });
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_composer_keeps_agent_completion_plan_and_ask_keys(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.home.prompt.update(cx, |p, cx| p.focus(w, cx));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.input("@des", cx);
    })
    .unwrap();
    press_in(window)(cx, &["tab"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.prompt.read(cx).value(), "@designer ");
        assert!(!s.home.asking);
        assert_eq!(s.home.draft.repos.len(), 1);
    });
    press_in(window)(cx, &["shift-tab"]);
    cx.read(|cx| assert!(shark.read(cx).home.draft.plan));
    press_in(window)(cx, &["tab"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(shark.read(cx).home.asking);
        assert!(w.try_find("draft-add").is_none());
        assert!(w.try_find("draft-model").is_none());
    })
    .unwrap();
    press_in(window)(cx, &["tab"]);
    cx.read(|cx| assert!(!shark.read(cx).home.asking));
    cx.update_window(window, |_, w, cx| w.input("fix it", cx))
        .unwrap();
    press_in(window)(cx, &["shift-enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.prompt.read(cx).value(), "@designer fix it\n");
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn staged_files_stay_with_their_draft_and_block_early_start(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let source = home.join("notes.txt");
    fs::create_dir_all(&home).unwrap();
    fs::write(&source, "local-only notes").unwrap();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.draft_composer_event(
            false,
            &crate::task_composer::Event::Attach {
                paths: vec![source.clone()],
                image: None,
            },
            w,
            cx,
        );
        assert_eq!(s.home.draft.staging, 1);
        s.submit_task_draft(w, cx);
        assert!(
            s.creating.is_empty(),
            "Start waits until files have been copied"
        );
        s.create_named_agent("Designer", cx).unwrap();
        s.open_agent("designer", w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).home.draft.staging == 0
    })
    .await;
    let staged = cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            s.home.agent_draft.files.is_empty(),
            "copy never lands in a different agent's draft"
        );
        assert_eq!(s.home.draft.files.len(), 1);
        s.home.draft.files[0].clone()
    });
    assert_eq!(fs::read_to_string(&staged).unwrap(), "local-only notes");
    on_app(window, shark.clone())(cx, &|s, w, cx| s.enter_home(w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click(("draft-file", 0usize), cx);
        w.render_frame(cx);
        w.within("menu-parent").click(2usize, cx);
    })
    .unwrap();
    assert!(!staged.exists());
    assert!(
        source.exists(),
        "removing context never deletes the user's original file"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn draft_tray_keyboard_opens_branch_choices_and_removes_repo(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.prefill_agent_task("designer", w, cx));
    press_in(window)(cx, &["up"]);
    press_in(window)(cx, &["enter"]);
    press_in(window)(cx, &["down", "enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).draft_picker.is_some()
    })
    .await;
    press_in(window)(cx, &["escape"]);
    press_in(window)(cx, &["backspace"]);
    cx.read(|cx| {
        let draft = &shark.read(cx).home.agent_draft;
        assert!(draft.repos.is_empty());
        assert!(draft.repos_edited);
        assert!(shark.read(cx).state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_picker_stays_in_draft_flow_after_home_ask_mode(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        s.toggle_ask_mode(w, cx);
        s.prefill_agent_task("designer", w, cx);
        s.new_task_default(w, cx);
    });
    press_in(window)(cx, &["enter"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(
            !w.has_active_dialog(cx) && shark.read(cx).draft_picker.is_none(),
            "Picking the fresh branch closes without requiring a task branch name"
        );
        let s = shark.read(cx);
        assert!(s.state.tasks.is_empty());
        assert_eq!(s.home.agent_draft.repos.len(), 1);
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        assert!(s.home.asking);
        s.new_task_default(w, cx);
        assert!(
            !s.home.asking,
            "a task-context command returns Home to its task draft"
        );
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn ui_review_regression_composer_escape_stops_answer(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let quiet = quiet_claude();
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.toggle_ask_mode(w, cx);
        s.ask("Keep streaming".into(), w, cx);
        s.home.prompt.update(cx, |p, cx| p.focus(w, cx));
        assert!(!s.home.ask.as_ref().unwrap().done);
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| {
        let ask = shark.read(cx).home.ask.as_ref().unwrap();
        assert!(
            ask.done && ask.harness.is_none(),
            "first Escape from the composer stops the answer"
        );
    });
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.toggle_ask_mode(w, cx);
        s.ask("Keep streaming with completion open".into(), w, cx);
        s.home.prompt.update(cx, |p, cx| {
            p.focus(w, cx);
            p.insert("see @", w, cx);
        });
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| {
        assert!(
            !shark.read(cx).home.ask.as_ref().unwrap().done,
            "completion consumes the first Escape"
        )
    });
    press_in(window)(cx, &["escape"]);
    cx.read(|cx| {
        assert!(
            shark.read(cx).home.ask.as_ref().unwrap().done,
            "next Escape stops the answer"
        )
    });
    fs::remove_dir_all(home).ok();
}

#[test]
fn ui_review_regression_claude_guards_are_inside_ui_lock() {
    for test in include_str!("home_tests.rs").split("async fn ").skip(1) {
        let body = test.split("#[gpui_kit::test]").next().unwrap();
        if let Some(guard) = body.find("ClaudeGuard::set") {
            let lock = body
                .find("UI_LOCK.lock()")
                .expect("environment mutation requires UI_LOCK");
            assert!(
                lock < guard,
                "{}: acquire UI_LOCK before ClaudeGuard so restoration also holds the lock",
                body.split('(').next().unwrap()
            );
        }
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_agent_chip_backspace_preserves_prompt_body_and_tokens(cx: &mut TestAppContext) {
    use gpui_kit::component::input::{InlineToken, InputContent};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        let content = InputContent::new("@designer Keep notes.txt")
            .with_token(15..24, InlineToken::new("notes.txt", "notes.txt"))
            .unwrap();
        s.home.prompt.update(cx, |prompt, cx| {
            prompt.set_value(content, w, cx);
            prompt.set_selected_range(0..0, cx);
            prompt.focus(w, cx);
        });
    });
    for _ in 0..4 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.home.tray_stop = Some(super::super::draft_view::DraftStop::Agent);
        s.home.tray_focus.focus(w, cx);
    });
    press_in(window)(cx, &["backspace"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        let content = s.home.prompt.read(cx).content();
        assert_eq!(content.text().as_ref(), "Keep notes.txt");
        assert_eq!(content.tokens().len(), 1);
        assert_eq!(content.tokens()[0].range(), 5..14);
        assert!(s.home.draft_agent.is_none());
        assert!(s.state.tasks.is_empty());
    });
}

/// Repository scope belongs to Home; other navigation still sees every task.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repo_filter_narrows_home(cx: &mut TestAppContext) {
    use super::super::home::{Destination, RepoFilter};
    use crate::state::{AgentLink, AttachStage, Attachment};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.state.repos = repos(&home, &["api", "web"]);
        let repo = |name: &str| TaskRepo {
            name: name.into(),
            src: home.join(name),
            base: "main".into(),
            branch: None,
        };
        s.state.tasks = vec![
            Task {
                repos: vec![repo("api")],
                ..fake("api-only")
            },
            Task {
                repos: vec![repo("api"), repo("web")],
                ..fake("both")
            },
            Task {
                agent: Some(AgentLink {
                    slug: "designer".into(),
                    ..Default::default()
                }),
                ..fake("none")
            },
        ];
        s.enter_home(w, cx);
        s.home.sel = Some("none".into());
        s.home.repo_filter = Some(RepoFilter::Repo("api".into()));
        s.fix_home_sel(cx);
        assert_eq!(s.home_rows(cx).len(), 2);
        assert_eq!(s.home_hidden(cx), (1, 0));
        assert_eq!(s.home.sel.as_deref(), Some("api-only"));
        assert_eq!(
            s.ws_order(true).len(),
            3,
            "the filter never changes sidebar order"
        );
        s.home.repo_filter = Some(RepoFilter::NoRepo);
        s.fix_home_sel(cx);
        assert_eq!(s.home_rows(cx).len(), 1);
        assert_eq!(s.home.sel.as_deref(), Some("none"));
        s.home.repo_filter = Some(RepoFilter::Repo("api".into()));
        s.home.destination = Destination::Agent("designer".into());
        assert_eq!(
            s.home_rows(cx).len(),
            1,
            "agent pages ignore the Home filter"
        );
        s.home.destination = Destination::Home;
        let mut attachment = Attachment::fresh(&s.state.repos[1], "none", "main");
        attachment.stage = AttachStage::SetupFailed;
        s.state.tasks[2].attaching.push(attachment);
        assert_eq!(
            s.home_hidden(cx),
            (1, 1),
            "hidden tasks retain their attention"
        );
        s.home.repo_filter = Some(RepoFilter::Repo("web".into()));
        assert_eq!(s.home_rows(cx).len(), 2, "pending attachments match too");
        s.home.repo_filter = Some(RepoFilter::NoRepo);
        assert!(s.home_rows(cx).is_empty(), "attaching is not No repository");
        s.state.tasks[2].attaching.clear();
        s.home.repo_filter = None;
        s.fix_home_sel(cx);
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("repo-filter", cx);
        w.input("api", cx);
    })
    .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.repo_filter, Some(RepoFilter::Repo("api".into())));
        assert_eq!(s.home_rows(cx).len(), 2);
        assert_eq!(s.home_hidden(cx), (1, 0));
    });
    on(cx, &|s, _, cx| {
        s.home.repo_filter = Some(RepoFilter::Repo("removed-repo".into()));
        s.fix_home_sel(cx);
        assert!(s.home.sel.is_none());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("repo-filter").is_some(),
            "the empty state keeps its controls"
        );
        w.click("show-all-repositories", cx);
    })
    .unwrap();
    cx.read(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.home.repo_filter, None);
        assert_eq!(s.home_rows(cx).len(), 3);
        assert!(s.home.sel.is_some());
    });
    fs::remove_dir_all(home).ok();
}

/// A stale Home cursor must never target a task hidden by a repository filter.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repo_filter_rejects_hidden_target(cx: &mut TestAppContext) {
    use super::super::home::RepoFilter;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.state.tasks = vec![fake("hidden")];
        s.enter_home(w, cx);
        s.home.repo_filter = Some(RepoFilter::Repo("api".into()));
        s.home.sel = Some("hidden".into());
        s.home.focus.focus(w, cx);
        assert!(
            s.target(w).is_none(),
            "a stale cursor cannot act on a filtered-out task"
        );
    });
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn backlog_folds_last_and_mutes_attention(cx: &mut TestAppContext) {
    use super::super::home::{Bucket, Destination, RepoFilter};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let parked = Task {
        backlog: true,
        agent: Some(crate::state::AgentLink {
            slug: "designer".into(),
            ..Default::default()
        }),
        repos: vec![TaskRepo {
            name: "api".into(),
            src: home.join("api"),
            base: "main".into(),
            branch: None,
        }],
        ..fake("parked")
    };
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![fake("active"), parked],
            agents: vec![crate::state::Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            sections: vec![Section {
                name: "First".into(),
                icon: "folder".into(),
                tasks: vec!["parked".into()],
                collapsed: false,
                agent: None,
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.enter_home(w, cx);
        assert!(!s.home.backlog_open);
        assert_eq!(s.home_rows(cx).len(), 1);
        s.home.waiting.insert("parked".into(), "Plan to approve");
        s.turn_done("parked", "parked", cx);
        assert_eq!(s.attention_of("parked", cx), Attn::None);
        assert_eq!(
            crate::attention::BADGE.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        s.active = Some("parked".into());
        assert!(
            !s.ws_order(false).contains(&1),
            "even the open backlog task has no cmd-N"
        );
        assert!(
            s.ws_order(true).contains(&1),
            "full task navigation includes Backlog"
        );
        assert!(
            s.side_rows(false).contains(&SideRow::Task(1)),
            "the open task stays in the sidebar"
        );
        s.home.backlog_open = true;
        for grouped in [false, true] {
            s.home.group_tasks = grouped;
            let rows = s.home_rows(cx);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows.last().unwrap().bucket, Bucket::Backlog);
            assert_eq!(s.state.tasks[rows.last().unwrap().ix].name, "parked");
        }
        s.home.destination = Destination::Agent("designer".into());
        assert_eq!(
            s.home_rows(cx).len(),
            1,
            "agent pages show their Backlog when expanded"
        );
        assert_eq!(s.home_rows(cx)[0].bucket, Bucket::Backlog);
        s.home.backlog_open = false;
        assert!(s.home_rows(cx).is_empty(), "agent pages fold Backlog too");
        s.home.backlog_open = true;
        s.home.destination = Destination::Home;
        s.home.sel = Some("parked".into());
        s.home.focus.focus(w, cx);
        cx.notify();
    });
    assert!(
        cx.shown_system_notifications().is_empty(),
        "Backlog stays quiet"
    );
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("toggle-backlog", cx);
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        assert!(!s.home.backlog_open);
        assert_ne!(s.target(w).map(|t| t.name).as_deref(), Some("parked"));
        s.home.repo_filter = Some(RepoFilter::Repo("api".into()));
        s.fix_home_sel(cx);
        assert!(s.home_rows(cx).is_empty());
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find("toggle-backlog").is_some(),
            "a filtered, folded Backlog remains reachable"
        );
        assert!(
            w.try_find("repo-empty-tasks").is_none(),
            "folding is not an empty repository"
        );
        w.click("toggle-backlog", cx);
    })
    .unwrap();
    cx.read(|cx| assert_eq!(shark.read(cx).home_rows(cx).len(), 1));
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn backlog_key_and_cursor(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            tasks: ["a", "b", "c"].map(fake).to_vec(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.open_task(0, w, cx);
        s.enter_home(w, cx);
        s.home.sel = Some("a".into());
        s.home.focus.focus(w, cx);
    });
    press(cx, &["b"]);
    on(cx, &|s, w, cx| {
        assert!(s.task("a").unwrap().backlog);
        assert_eq!(
            s.home.sel.as_deref(),
            Some("b"),
            "the cursor keeps the vacated row's height"
        );
        s.leave_home(w, cx);
        s.sel = SideRow::Task(0);
        s.sidebar_focus.focus(w, cx);
    });
    press(cx, &["b"]);
    cx.read(|cx| {
        let s = shark.read(cx);
        assert!(!s.task("a").unwrap().backlog);
        assert_eq!(s.active.as_deref(), Some("a"));
        assert!(s.ws_order(false).contains(&0));
    });
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn backlog_preserves_notification_baseline(cx: &mut TestAppContext) {
    use crate::state::{AttachStage, Attachment};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![fake("parked")],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.state.tasks[0].attaching.push(Attachment {
            repo: "api".into(),
            src: home.join("api"),
            branch: "parked".into(),
            base: "main".into(),
            fresh: true,
            stage: AttachStage::SetupFailed,
            error: None,
        });
        s.attn.set_checked("parked", vec![]);
        s.refresh_attention(cx);
        assert!(matches!(s.attention_of("parked", cx), Attn::Needs(_)));
        s.set_backlog("parked", true, Some(w), cx);
        assert_eq!(s.attention_of("parked", cx), Attn::None);
        s.set_backlog("parked", false, None, cx);
        assert!(matches!(s.attention_of("parked", cx), Attn::Needs(_)));
    });
    assert!(
        cx.shown_system_notifications().is_empty(),
        "reactivating an unchanged needs-you label must not notify"
    );
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn backlog_picker_and_turn_start(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![Task {
                backlog: true,
                ..fake("parked")
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| s.enter_home(w, cx));
    press(cx, &["cmd-shift-o"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("workspace-result-parked")
                .label()
                .is_some_and(|l| l.contains("Backlog"))
        );
    })
    .unwrap();
    press(cx, &["enter"]);
    let chat = cx.read(|cx| {
        let s = shark.read(cx);
        assert!(
            s.task("parked").unwrap().backlog,
            "opening does not reactivate"
        );
        s.active_chat().unwrap().clone()
    });
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.busy = true;
            cx.notify();
        })
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(
            !shark.read(cx).task("parked").unwrap().backlog,
            "a turn starts without needing a sent event"
        )
    });
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.busy = false;
            cx.notify();
        })
    });
    fs::remove_dir_all(home).ok();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn backlog_cannot_hide_an_open_question(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            tasks: vec![fake("question"), fake("idle")],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| s.open_task(0, w, cx));
    let chat = cx.read(|cx| shark.read(cx).active_chat().unwrap().clone());
    cx.update(|cx| chat.update(cx, |c, cx| {
        c.busy = true;
        crate::chat::ui_tests::play(c, &[r#"{"type":"control_request","request_id":"backlog-question","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"backlog-tool","input":{"questions":[{"header":"Color","question":"Which color?","options":[{"label":"Blue","description":"Blue"},{"label":"Red","description":"Red"}]}]}}}"#], cx);
        cx.notify();
    }));
    cx.run_until_parked();
    on(cx, &|s, w, cx| {
        assert!(s.asking_chat("question", cx));
        assert_eq!(s.running_chats("question", cx), 0);
        assert!(!s.can_backlog("question", cx));
        assert!(
            !ws_menu(s, s.task("question").unwrap(), cx)
                .iter()
                .any(|e| matches!(e, Entry::Item { label, .. } if label == "Move to backlog"))
        );
        s.enter_home(w, cx);
        s.home.sel = Some("question".into());
        s.home.focus.focus(w, cx);
    });
    press(cx, &["b"]);
    cx.read(|cx| assert!(!shark.read(cx).task("question").unwrap().backlog));
    for (task, count) in [("question", 0), ("idle", 1)] {
        on(cx, &|s, w, cx| {
            s.home.sel = Some(task.into());
            s.home.focus.focus(w, cx);
        });
        press(cx, &["cmd-k"]);
        cx.update_window(window, |_, w, cx| w.input("Move task to backlog", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert_eq!(
                w.find_all_by_role(Role::ListBoxOption).len(),
                count,
                "palette gate for {task}"
            );
        })
        .unwrap();
        press(cx, &["escape", "escape"]);
        cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
            .unwrap();
    }
    fs::remove_dir_all(home).ok();
}
