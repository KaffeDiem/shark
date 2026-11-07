use super::*;
use crate::state::{ChatMeta, NEW_TASK, SEA_NAMES, TabId};

/// The app on no repos at all, on a claude that never answers; the guard holds it.
fn empty_app(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    (window, shark)
}

fn active_task(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Task {
    cx.update(|cx| shark.read(cx).active_task().unwrap().clone())
}

fn active_chat(cx: &mut TestAppContext, shark: &Entity<Shark>) -> Entity<crate::chat::Chat> {
    cx.update(|cx| shark.read(cx).active_chat().unwrap().clone())
}

fn toasts(cx: &mut TestAppContext, window: AnyWindowHandle) -> usize {
    cx.update_window(window, |_, w, cx| w.notifications(cx).len())
        .unwrap()
}

/// Duplicate folders and names in one picker result are checked against earlier additions.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn adding_repository_paths_rejects_duplicates_within_the_batch(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let first = home.join("src/api");
    let same_name = home.join("other/api");
    init_repo(&first);
    init_repo(&same_name);
    let (window, shark) = empty_app(cx);
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        let added = s.add_repo_paths(&[first.clone(), first.clone(), same_name.clone()], w, cx);
        assert_eq!(added.len(), 1);
        assert_eq!(s.state.repos.len(), 1);
        assert_eq!(s.state.repos[0].path, first.canonicalize().unwrap());
    });
    assert_eq!(State::load().repos.len(), 1);
}

/// An agent with no repository, its chat holding a plan.
async fn planned_agent(
    cx: &mut TestAppContext,
) -> (AnyWindowHandle, Entity<Shark>, Entity<crate::chat::Chat>) {
    let (window, shark) = empty_app(cx);
    press_in(window)(cx, &["cmd-n"]);
    let chat = active_chat(cx, &shark);
    cx.update(|cx| {
        chat.update(cx, |c, cx| {
            c.plan.update(cx, |p, cx| {
                p.add("p1".into(), "# Build it\n\nSteps".into(), cx)
            })
        })
    });
    (window, shark, chat)
}

/// Home's prompt with no repos registered: no dialog, a saved agent in Plan mode on a generated
/// identity, titled by the prompt, the provider started, the prompt cleared.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_prompt_starts_an_agent_without_repos(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let (window, shark) = empty_app(cx);
    cx.update_window(window, |_, w, cx| w.input("sketch a login page", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx), "no dialog");
    })
    .unwrap();
    let ws = active_task(cx, &shark);
    assert!(SEA_NAMES.contains(&ws.name.as_str()), "{}", ws.name);
    assert_eq!(ws.title, "sketch a login page");
    assert_eq!(ws.branch, ws.name);
    assert!(ws.repos.is_empty() && !ws.ready());
    assert!(ws.chats[0].plan, "starts in Plan mode");
    assert_eq!(ws.chats[0].id, ws.name);
    let dir = home.join("shark/workspaces").join(&ws.name);
    for doc in ["CLAUDE.md", "AGENTS.md"] {
        let text = fs::read_to_string(dir.join(doc)).unwrap();
        assert!(text.contains("No repositories are attached"), "{text}");
    }
    let saved = State::load();
    assert_eq!(saved.tasks[0].name, ws.name);
    assert_eq!(saved.tasks[0].title, ws.title);
    let chat = active_chat(cx, &shark);
    cx.update(|cx| {
        assert_eq!(chat.read(cx).sent_texts(), ["sketch a login page"]);
        assert!(chat.read(cx).meta.plan);
        assert_eq!(shark.read(cx).home.prompt.read(cx).value(), "");
    });
}

/// cmd-n opens an empty conversation, also with repos registered, and no provider runs yet;
/// the first message titles it, and a second doesn't.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_task_is_empty_until_the_first_message(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
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
    let press = press_in(window);
    press(cx, &["cmd-n"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        w.click("new-workspace", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(shark.read(cx).state.tasks.len(), 2, "the + button too"));
    let ws = active_task(cx, &shark);
    let shown = ws.display_title().into_owned();
    assert_eq!(shown.to_lowercase(), ws.name);
    assert_ne!(shown, ws.name, "capitalised");
    assert!(ws.repos.is_empty());
    let chat = active_chat(cx, &shark);
    cx.update(|cx| assert!(chat.read(cx).sent_texts().is_empty()));

    // OK in the rename dialog, with the field as it opened.
    shark.update(cx, |s, cx| s.set_title(&ws.name, shown.clone(), cx));
    assert_eq!(active_task(cx, &shark).title, NEW_TASK, "still untitled");

    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| c.ask("plan the rollout\nin detail", w, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(active_task(cx, &shark).title, "plan the rollout");
    assert_eq!(State::load().tasks[1].title, "plan the rollout");

    // The name as a title, once it is one, is the user's: a later save doesn't undo it.
    shark.update(cx, |s, cx| s.set_title(&ws.name, shown.clone(), cx));
    chat.update(cx, |_, cx| cx.emit(crate::chat::ChatEvent::Meta));
    cx.run_until_parked();
    assert_eq!(active_task(cx, &shark).title, shown);
}

/// An agent saved with the old name and the literal placeholder is still titled by a message.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn first_message_titles_an_old_untitled_agent(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let name = "agent-19a2b";
    let dir = home.join("shark/workspaces").join(name);
    let ws = git::create_empty_task(&dir, name, NEW_TASK, None).unwrap();
    let state = State {
        tasks: vec![ws],
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.open_task_now(0, w, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(active_task(cx, &shark).display_title(), "New task");
    let chat = active_chat(cx, &shark);
    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| c.ask("plan the rollout", w, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(active_task(cx, &shark).title, "plan the rollout");
}

/// The first message can't go out: the agent stays, saved, with the message in its composer.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_first_send_keeps_agent_and_prompt(cx: &mut TestAppContext) {
    // Its one-time SHARK_CLAUDE must not land on top of the guard below.
    crate::chat::ui_tests::fake_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(std::path::Path::new("/nonexistent/claude"));
    test_home();
    let (window, shark) = empty_app(cx);
    cx.update_window(window, |_, w, cx| w.input("sketch a login page", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);
    let ws = active_task(cx, &shark);
    assert_eq!(ws.title, "sketch a login page");
    assert_eq!(State::load().tasks.len(), 1);
    assert!(ws.dir().exists());
    let chat = active_chat(cx, &shark);
    cx.update(|cx| {
        let c = chat.read(cx);
        assert_eq!(c.composer.read(cx).value(), "sketch a login page");
        assert!(c.sent_texts().is_empty());
    });
}

/// state.json can't be written: nothing starts, nothing is left behind, the prompt stays typed.
/// With state.json unreadable, creating is refused outright.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_save_leaves_no_orphan(cx: &mut TestAppContext) {
    // Its own copy: a process from an earlier test may still write the shared `.args`.
    let quiet = quiet_claude().with_file_name("unsaved-claude");
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fs::copy(quiet.with_file_name("quiet-claude"), &quiet).unwrap();
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let (window, shark) = empty_app(cx);
    let args = PathBuf::from(format!("{}.args", quiet.display()));
    let _ = fs::remove_file(&args);
    let path = crate::state::support_dir().join("state.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    cx.update_window(window, |_, w, cx| w.input("sketch a login page", cx))
        .unwrap();
    let before = toasts(cx, window);
    press_in(window)(cx, &["enter"]);
    let entries = |dir: PathBuf| fs::read_dir(dir).map_or(0, |d| d.count());
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.state.tasks.is_empty() && s.active.is_none());
        assert_eq!(s.home.prompt.read(cx).value(), "sketch a login page");
    });
    assert_eq!(entries(home.join("shark/workspaces")), 0, "no orphan");
    assert!(!args.exists(), "no provider");
    assert_eq!(toasts(cx, window), before + 1);
    fs::remove_dir(&path).unwrap();

    let on = on_app(window, shark.clone());
    on(cx, &|s, _, _| s.state.read_only = true);
    on(cx, &|s, w, cx| s.new_task(None, None, None, w, cx));
    cx.update(|cx| assert!(shark.read(cx).state.tasks.is_empty()));
    assert_eq!(entries(home.join("shark/workspaces")), 0);
}

/// Agents made in the same instant each get their own identity.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn generated_identities_never_collide(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark) = empty_app(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        for _ in 0..3 {
            s.new_task(None, None, None, w, cx);
        }
    });
    let mut names: Vec<String> = cx.update(|cx| {
        (shark.read(cx).state.tasks.iter())
            .map(|w| w.name.clone())
            .collect()
    });
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 3);
    for n in &names {
        assert!(crate::state::tasks_dir().join(n).is_dir());
    }
}

/// A title edit changes the title and nothing else, across save and load.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn title_edit_leaves_identity_alone(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
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
    let on = on_app(window, shark.clone());
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    on(cx, &|s, w, cx| {
        s.create_task_for_test(
            "kd/login".into(),
            git::on_branch(&repos, "kd/login"),
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
    on(cx, &|s, _, _| {
        let ws = s.active_task_mut().unwrap();
        ws.viewed.insert("api/a.rs".into(), 7);
        ws.open_tab(TabId::File("api/a.rs".into()));
        let _ = s.state.save();
    });
    let was = active_task(cx, &shark);
    assert_eq!(was.display_title(), "kd-login", "no title: the name");

    on(cx, &|s, w, cx| s.rename_task(was.name.clone(), w, cx));
    cx.update_window(window, |_, w, cx| w.input("Login work", cx))
        .unwrap();
    press_in(window)(cx, &["enter"]);

    let saved = State::load();
    for ws in [active_task(cx, &shark), saved.tasks[0].clone()] {
        assert_eq!(ws.title, "Login work");
        assert_eq!(ws.display_title(), "Login work");
        assert_eq!(ws.name, was.name);
        assert_eq!(ws.dir(), was.dir());
        assert_eq!(ws.branch, was.branch);
        assert_eq!(ws.tab_order, was.tab_order);
        assert_eq!(ws.viewed, was.viewed);
        assert_eq!(ws.chats, was.chats);
    }
    assert!(was.dir().join("api").exists());
    assert_eq!(
        git::git(&was.dir().join("api"), &["branch", "--show-current"]).unwrap(),
        "kd/login"
    );
    // An empty title changes nothing.
    on(cx, &|s, _, cx| s.set_title("kd-login", String::new(), cx));
    assert_eq!(active_task(cx, &shark).title, "Login work");
}

/// shift-tab, the Plan button and the palette's toggle can't leave Plan mode, and say why with
/// the offer; approving and building refuse too, with no chat started and nothing approved.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn every_way_out_of_plan_mode_needs_a_repository(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark, chat) = planned_agent(cx).await;
    let unchanged = |cx: &mut TestAppContext, what: &str| {
        cx.update(|cx| {
            let c = chat.read(cx);
            assert!(c.meta.plan, "{what}: Plan mode stays");
            assert_eq!(c.plan.read(cx).approved, None, "{what}: not approved");
            assert!(c.sent_texts().is_empty(), "{what}: nothing sent");
            assert_eq!(
                shark.read(cx).active_task().unwrap().chats.len(),
                1,
                "{what}"
            );
        })
    };
    let offered = |cx: &mut TestAppContext, what: &str| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(w.try_find("attach-offer").is_some(), "{what}: the offer");
            w.clear_notifications(cx);
        })
        .unwrap();
    };
    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| c.focus_composer(w, cx));
        w.press("shift-tab", cx);
    })
    .unwrap();
    cx.run_until_parked();
    unchanged(cx, "shift-tab");
    offered(cx, "shift-tab");

    let actions: [(&str, Box<dyn Action>); 3] = [
        ("toggle", Box::new(TogglePlanMode)),
        ("approve", Box::new(ApprovePlan)),
        ("build", Box::new(BuildPlanInNewChat)),
    ];
    for (what, action) in actions {
        cx.update_window(window, |_, w, cx| w.dispatch_action(action, cx))
            .unwrap();
        cx.run_until_parked();
        unchanged(cx, what);
        offered(cx, what);
    }
    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| {
            c.approve_plan(w, cx);
            c.set_plan_mode(false, cx);
            assert!(!c.build_plan("# Build it", w, cx));
        })
    })
    .unwrap();
    cx.run_until_parked();
    unchanged(cx, "direct calls");
}

/// New chats, a closed chat reopened, and the fresh chat a closed last tab leaves all run in
/// Plan mode without a repository, whatever they were saved with; Codex chats too.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn new_and_reopened_chats_open_in_plan_mode(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark) = empty_app(cx);
    let press = press_in(window);
    let on = on_app(window, shark.clone());
    press(cx, &["cmd-n"]);
    press(cx, &["cmd-t"]);
    let chat = active_chat(cx, &shark);
    cx.update(|cx| assert!(chat.read(cx).meta.plan && chat.read(cx).plan_open));
    assert_eq!(active_task(cx, &shark).chats.len(), 2);

    let id = format!("{}~9", active_task(cx, &shark).name);
    let closed = ChatMeta {
        id: id.clone(),
        title: "old".into(),
        model: Some("codex:gpt-6.1-sol".into()),
        ..Default::default()
    };
    on(cx, &|s, _, _| {
        s.active_task_mut().unwrap().closed.push(closed.clone());
    });
    press(cx, &["cmd-shift-t"]);
    let chat = active_chat(cx, &shark);
    cx.update(|cx| {
        let c = chat.read(cx);
        assert_eq!(c.meta.id, id);
        assert!(c.meta.plan && c.codex(), "a reopened Codex chat plans too");
    });
    let saved = State::load();
    assert!(
        (saved.tasks[0].chats.iter()).any(|c| c.id == id && c.plan),
        "saved"
    );
    // Refused as in a Claude chat: Codex reads `plan` from the meta when it starts.
    cx.update(|cx| chat.update(cx, |c, cx| c.set_plan_mode(false, cx)));
    cx.update(|cx| assert!(chat.read(cx).meta.plan));

    for _ in 0..3 {
        press(cx, &["cmd-w"]);
    }
    let ws = active_task(cx, &shark);
    assert_eq!(ws.chats.len(), 1);
    let chat = active_chat(cx, &shark);
    cx.update(|cx| assert!(chat.read(cx).meta.plan, "the last tab's replacement"));
}

/// With no repo registered the offer goes to the folder panel; a local repo attaches on a fresh
/// branch, the root docs follow, and Plan mode and the plan stay until the user ends them.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn attaching_never_ends_plan_mode(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let home = crate::state::home();
    let (window, shark, chat) = planned_agent(cx).await;
    let on = on_app(window, shark.clone());
    let name = active_task(cx, &shark).name;
    let n = name.clone();
    on(cx, &|s, w, cx| s.attach_repository(n.clone(), w, cx));
    assert!(cx.did_prompt_for_paths(), "the folder panel");
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();

    let repo = home.join("src/api");
    init_repo(&repo);
    let n = name.clone();
    on(cx, &|s, w, cx| {
        s.attach_paths(n.clone(), std::slice::from_ref(&repo), w, cx)
    });
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active_task().is_some_and(Task::ready)
    })
    .await;
    cx.run_until_parked();
    let ws = active_task(cx, &shark);
    assert_eq!(ws.repos[0].name, "api");
    assert_eq!(
        git::git(&ws.dir().join("api"), &["branch", "--show-current"]).unwrap(),
        name
    );
    let docs = fs::read_to_string(ws.dir().join("CLAUDE.md")).unwrap();
    assert!(
        docs.contains("`api/`") && !docs.contains("No repositories"),
        "{docs}"
    );
    cx.update(|cx| {
        let c = chat.read(cx);
        assert!(c.ws.ready() && c.meta.plan, "still planning");
        assert_eq!(c.plan.read(cx).approved, None, "still unapproved");
        assert!(c.sent_texts().is_empty());
    });
    // Now it's the user's call.
    cx.update(|cx| chat.update(cx, |c, cx| c.set_plan_mode(false, cx)));
    cx.update(|cx| assert!(!chat.read(cx).meta.plan));
}

/// An agent with no repository: Diff and Git show the offer instead of Git actions, the header
/// offers it for the branch, git commands refuse, and the poller stays quiet.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn empty_diff_and_git_offer_attaching(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark) = empty_app(cx);
    let on = on_app(window, shark.clone());
    press_in(window)(cx, &["cmd-n"]);
    for feature in Feature::ALL {
        on(cx, &|s, w, cx| s.show(Tab::Feature(feature), w, cx));
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            assert!(w.try_find("attach-repository").is_some());
            assert!(w.try_find("header-attach").is_some());
            assert!(w.try_find("branch").is_none(), "no branch to show");
            w.click("attach-repository", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.did_prompt_for_paths(), "the folder panel");
        cx.simulate_path_prompt_response(|_| None);
        cx.run_until_parked();
    }
    let ws = active_task(cx, &shark);
    let (label, repos) = crate::sync::check_in_background(&ws, &[], true, &[]);
    assert!(matches!(label, Ok(None)) && repos.is_empty(), "{label:?}");
    let refused = |cx: &mut TestAppContext, f: &AppUpdate<'_>, what: &str| {
        let before = toasts(cx, window);
        on(cx, f);
        assert_eq!(toasts(cx, window), before + 1, "{what}");
        cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
            .unwrap();
    };
    refused(
        cx,
        &|_, w, cx| w.dispatch_action(Box::new(CopyBranch), cx),
        "copy branch",
    );
    refused(
        cx,
        &|s, w, cx| s.stack_from(ws.name.clone(), false, w, cx),
        "stack",
    );
    refused(
        cx,
        &|s, w, cx| s.change_base(ws.name.clone(), None, w, cx),
        "base",
    );
    refused(
        cx,
        &|s, w, cx| s.with_sync(w, cx, GitSync::commit_and_push),
        "git",
    );
    let chat = active_chat(cx, &shark);
    cx.update(|cx| assert_eq!(chat.read(cx).queued(), 0, "nothing queued for the agent"));
}

/// Delete task says what it removes first; an agent with no repository goes cleanly, with its
/// conversation and plans.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn delete_agent_confirms_and_removes_everything(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark, _chat) = planned_agent(cx).await;
    let ws = active_task(cx, &shark);
    cx.update_window(window, |_, w, cx| {
        w.dispatch_action(Box::new(ArchiveTask), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx), "asks first");
    })
    .unwrap();
    assert!(ws.dir().exists());
    press_in(window)(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.tasks.is_empty()
    })
    .await;
    assert!(!ws.dir().exists());
    assert!(State::load().tasks.is_empty());
}

/// The branch and PR dialog stays reachable and keeps refusing with no repo to check out;
/// typing on Home first makes that its first message.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_dialog_stays_reachable(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = empty_app(cx);
    let run = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            w.dispatch_action(Box::new(NewTaskFromBranch), cx)
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| w.has_active_dialog(cx))
            .unwrap()
    };
    assert!(!run(cx), "nothing to check out");
    let repos = repos(&home, &["api"]);
    on_app(window, shark.clone())(cx, &|s, _, _| s.state.repos = repos.clone());
    assert!(run(cx));
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn persistent_agent_starts_isolated_tasks_with_instructions(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let instructions = "Always use Mobbin to ground your research";
    let state = State {
        repos: repos(&home, &["api"]),
        agents: vec![crate::state::Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            icon: "palette".into(),
            instructions: instructions.into(),
            repos: vec!["api".into()],
            model: None,
            effort: None,
            ..Default::default()
        }],
        ..Default::default()
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    for (count, prompt) in [(1, "one"), (2, "two")] {
        on_app(window, shark.clone())(cx, &|s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.prompt.update(cx, |p, cx| {
                p.set_value(format!("@designer {prompt}"), w, cx)
            });
        });
        press_in(window)(cx, &["enter"]);
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            let s = shark.read(cx);
            s.state.tasks.len() == count && s.creating.is_empty()
        })
        .await;
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(shark.read(cx).home.prompt.read(cx).value(), ""));
    }
    let tasks = cx.update(|cx| shark.read(cx).state.tasks.clone());
    assert_eq!(
        tasks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_ne!(tasks[0].branch, tasks[1].branch);
    cx.update(|cx| {
        let section = shark
            .read(cx)
            .state
            .sections
            .iter()
            .find(|s| s.agent.as_deref() == Some("designer"))
            .unwrap();
        assert_eq!(section.tasks.len(), 2);
        for task in &tasks {
            assert!(section.tasks.contains(&task.name));
        }
    });
    for (i, task) in tasks.iter().enumerate() {
        let agent = task.agent.as_ref().unwrap();
        assert_eq!(agent.slug, "designer");
        assert_eq!(agent.siblings.len(), 1);
        assert_eq!(agent.siblings[0].title, tasks[1 - i].title);
        assert!(task.branch.starts_with("designer/"));
        for doc in ["CLAUDE.md", "AGENTS.md"] {
            assert!(
                fs::read_to_string(task.dir().join(doc))
                    .unwrap()
                    .contains(instructions)
            );
        }
    }
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.agents[0].instructions = "Use the design system".into();
        s.sync_agent("designer", true, cx);
        s.set_title(&tasks[0].name, "renamed".into(), cx);
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.state.tasks[1].agent.as_ref().unwrap().siblings[0].title,
            "renamed"
        );
        for task in &s.state.tasks {
            assert_eq!(
                task.agent.as_ref().unwrap().instructions,
                "Use the design system"
            );
            assert!(
                fs::read_to_string(task.dir().join("AGENTS.md"))
                    .unwrap()
                    .contains("Use the design system")
            );
        }
    });
}
