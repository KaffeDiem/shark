//! Headless UI tests: the app on the real keymap, against real repos.
use super::*;
use super::{diff::DiffData, palette::COMMANDS};
use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use std::sync::Mutex;
use std::time::Instant;
use std::{fs, path::PathBuf, time::Duration};

/// UI tests share the process-wide `TEST_HOME` and `state.json`, so they take turns. Chat
/// tests that make workspace dirs take it too: `test_home` wipes them.
pub(crate) static UI_LOCK: Mutex<()> = Mutex::new(());

fn test_home() -> PathBuf {
    let home = crate::state::TEST_HOME
        .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())))
        .clone();
    let _ = fs::remove_dir_all(&home);
    home
}

fn init_repo(repo: &std::path::Path) {
    fs::create_dir_all(repo).unwrap();
    git::git(repo, &["init", "-q", "-b", "main"]).unwrap();
    git::git(
        repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "i",
        ],
    )
    .unwrap();
}

/// Repos `home/src/<name>`, each with one commit.
fn repos(home: &std::path::Path, names: &[&str]) -> Vec<Repo> {
    (names.iter())
        .map(|name| {
            let path = home.join("src").join(name);
            init_repo(&path);
            Repo {
                name: name.to_string(),
                path,
            }
        })
        .collect()
}

/// Saves `state`, then starts the app on the real keymap.
fn boot(
    cx: &mut TestAppContext,
    state: State,
    options: WindowOptions,
) -> (AnyWindowHandle, Entity<Shark>) {
    state.save();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::bind_keys(cx);
        crate::menubar::init(cx);
    });
    open(cx, options)
}

/// Every menu-bar item without an `os_action` has a handler from the current focus `at`.
fn assert_menus_reachable(cx: &mut TestAppContext, window: AnyWindowHandle, at: &str) {
    fn walk<'a>(items: &'a [MenuItem], out: &mut Vec<(&'a str, &'a dyn Action)>) {
        for item in items {
            match item {
                MenuItem::Action {
                    name,
                    action,
                    os_action: None,
                    ..
                } => out.push((name.as_ref(), &**action)),
                MenuItem::Submenu(m) => walk(&m.items, out),
                _ => {}
            }
        }
    }
    let menus = crate::menubar::menus();
    let mut items = vec![];
    for m in &menus {
        walk(&m.items, &mut items);
    }
    cx.update_window(window, |_, w, cx| {
        // App-level handlers (Hide, About…) aren't on the window's dispatch path. Quit's
        // global handler is registered in `main`, which the harness skips.
        let missing: Vec<_> = (items.iter())
            .filter(|(_, a)| !a.as_any().is::<crate::menubar::Quit>())
            .filter(|(_, a)| !w.is_action_available(*a, cx) && !cx.is_action_available(*a))
            .map(|(name, _)| *name)
            .collect();
        assert!(missing.is_empty(), "unreachable from {at}: {missing:?}");
    })
    .unwrap();
}

/// Shark > About Shark opens the About dialog.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn about_dialog(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.activate_window(); // About's global handler targets the active window
        w.dispatch_action(Box::new(crate::menubar::About), cx);
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.has_active_dialog(cx), "About opens a dialog");
    })
    .unwrap();
}

fn open(cx: &mut TestAppContext, options: WindowOptions) -> (AnyWindowHandle, Entity<Shark>) {
    cx.update(|cx| gpui_kit::open_window(options, cx, |w, cx| cx.new(|cx| Shark::new(w, cx))))
        .unwrap()
}

/// `on(cx, f)` runs `f` on the app, then lets it settle.
fn on_app(
    window: AnyWindowHandle,
    shark: Entity<Shark>,
) -> impl Fn(&mut TestAppContext, &dyn Fn(&mut Shark, &mut Window, &mut Context<Shark>)) {
    move |cx, f| {
        cx.update_window(window, |_, w, cx| shark.update(cx, |s, cx| f(s, w, cx)))
            .unwrap();
        cx.run_until_parked();
    }
}

/// Settings apply and persist at once; an edited prompt reaches the git button, reset drops it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn settings_apply_and_save(cx: &mut TestAppContext) {
    use crate::settings::Settings;
    use crate::state::Mode;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        let weak = shark.downgrade();
        let view = cx.new(|cx| Settings::new(weak, &state, w, cx));
        view.update(cx, |v, cx| {
            v.appearance(cx, |a| {
                a.mode = Mode::Dark;
                a.dark = "Sequoia".into();
                a.mono_font = Some("Menlo".into());
            });
            v.set_prompt(0, Some("ship {repos}".into()), cx);
        });
        assert_eq!(cx.theme().theme_name().as_ref(), "Sequoia Dark");
        // Headless tests have no system fonts, so check the config, not the resolved family.
        let mono = |t: &gpui_kit::component::ThemeConfig| t.mono_font_family.clone();
        assert_eq!(mono(&cx.theme().dark_theme).as_deref(), Some("Menlo"));
        let saved = State::load();
        assert_eq!(saved.appearance.dark, "Sequoia");
        assert_eq!(saved.prompts["commit"], "ship {repos}");
        assert_eq!(crate::prompts::render("commit", "api", "b"), "ship api");

        view.update(cx, |v, cx| {
            v.appearance(cx, |a| a.mode = Mode::Light);
            v.reset_prompt(0, w, cx);
        });
        assert_eq!(cx.theme().theme_name().as_ref(), "Linear Light");
        // The font override survives the mode switch.
        assert_eq!(mono(&cx.theme().light_theme).as_deref(), Some("Menlo"));
        assert!(State::load().prompts.is_empty());
        assert!(crate::prompts::render("commit", "api", "b").starts_with("Commit and push"));
    })
    .unwrap();
}

/// Vim mode is one switch: off, j/k type nothing in lists and the arrows move; Settings turns
/// it on for the lists and every chat at once. cmd-k acts on the sidebar's cursor row.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn vim_setting_and_palette_target(cx: &mut TestAppContext) {
    use crate::settings::Settings;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    for name in ["one", "two"] {
        on(cx, &|s, w, cx| {
            s.create_workspace(name.into(), repos.clone(), None, w, cx)
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    let press = |cx: &mut TestAppContext, keys: &[&str]| {
        cx.update_window(window, |_, w, cx| keys.iter().for_each(|k| w.press(k, cx)))
            .unwrap();
        cx.run_until_parked();
    };
    let sel = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).sel.clone());
    press(cx, &["escape", "cmd-alt-left"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).sidebar_focus.is_focused(w))
    })
    .unwrap();
    press(cx, &["home"]);
    let top = sel(cx);
    press(cx, &["j"]);
    assert_eq!(sel(cx), top, "j is a Vim key");
    press(cx, &["down"]);
    assert_ne!(sel(cx), top, "the arrows always move");
    press(cx, &["up"]);

    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        let view = cx.new(|cx| Settings::new(shark.downgrade(), &state, w, cx));
        view.update(cx, |v, cx| v.set_vim(true, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert!(State::load().vim, "the switch is saved");
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.chats.values().all(|c| c.read(cx).vim_on), "every chat follows");
    });
    press(cx, &["j"]);
    assert_ne!(sel(cx), top, "j moves with Vim on");

    // The cursor is on a workspace that isn't open: cmd-k's archive takes that one.
    let open = cx.update(|cx| shark.read(cx).active.clone().unwrap());
    let name = |cx: &mut TestAppContext| {
        let SideRow::Ws(ix) = sel(cx) else { panic!("cursor on a workspace") };
        cx.update(|cx| shark.read(cx).state.workspaces[ix].name.clone())
    };
    if name(cx) == open {
        press(cx, &["k"]);
    }
    assert_ne!(name(cx), open);
    press(cx, &["cmd-k"]);
    cx.update_window(window, |_, w, cx| w.input("archive workspace", cx))
        .unwrap();
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.len() == 1
    })
    .await;
    cx.update(|cx| assert_eq!(shark.read(cx).state.workspaces[0].name, open));
}

/// Drives the real keymap headlessly: create a workspace, vim focus moves, tabs, archive.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)] // each test owns its thread; the lock only orders them
async fn keyboard_flow(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api", "web"]),
            workspaces: vec![],
            vim: false,
            last_repos: vec!["api".into()],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let press = |cx: &mut TestAppContext, keys: &[&str]| {
        cx.update_window(window, |_, w, cx| keys.iter().for_each(|k| w.press(k, cx)))
            .unwrap()
    };

    // Commands without a key run from cmd-k; dispatching the action is the same path.
    let run = |cx: &mut TestAppContext, a: &dyn Action| {
        cx.update_window(window, |_, w, cx| w.dispatch_action(a.boxed_clone(), cx))
            .unwrap()
    };
    press(cx, &["cmd-n"]);
    cx.update_window(window, |_, w, cx| w.input("ui x", cx))
        .unwrap();
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    let ws_dir = home.join("shark/workspaces/ui-x");
    assert!(ws_dir.join("api").exists() && ws_dir.join("CLAUDE.md").exists());
    assert!(
        !ws_dir.join("web").exists(),
        "only the last-used repos start ticked"
    );

    // cmd-N opens the Nth workspace; past the end does nothing.
    press(cx, &["cmd-9", "cmd-1"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("ui-x")));

    // Opening a workspace focuses the composer: vim keys must type, not navigate.
    cx.update_window(window, |_, w, cx| w.input("jk g", cx))
        .unwrap();
    let chat = cx.update(|cx| shark.read(cx).chats["ui-x"].clone());
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));
    assert_menus_reachable(cx, window, "composer");

    // Stay awake: caffeinate runs only while the setting is on and an agent works.
    let busy = |cx: &mut TestAppContext, on: bool| {
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                c.busy = on;
                cx.notify()
            })
        });
        cx.run_until_parked();
    };
    let awake = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).awake.is_some());
    busy(cx, true);
    assert!(!awake(cx), "off by default");
    cx.update(|cx| {
        shark.update(cx, |s, cx| {
            s.state.stay_awake = true;
            s.update_awake(cx)
        })
    });
    assert!(awake(cx));
    busy(cx, false);
    assert!(!awake(cx));

    // Chat tabs: cmd-t opens one next to the current chat, ctrl-tab cycles, cmd-w closes,
    // cmd-shift-t brings back a closed chat that had a session.
    let tabs = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let ws = &shark.read(cx).state.workspaces[0];
            let ids: Vec<String> = ws.chats.iter().map(|c| c.id.clone()).collect();
            (ids, ws.active_chat, ws.closed.len())
        })
    };
    press(cx, &["cmd-t"]);
    assert_eq!(tabs(cx), (vec!["ui-x".into(), "ui-x~2".into()], 1, 0));
    press(cx, &["ctrl-tab"]);
    assert_eq!(tabs(cx).1, 0);
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));
    press(cx, &["ctrl-tab"]);
    let second = cx.update(|cx| shark.read(cx).chats["ui-x~2"].clone());
    cx.update(|cx| {
        second.update(cx, |c, cx| {
            c.meta.session_id = Some("s2".into());
            cx.emit(ChatEvent::Meta);
        })
    });
    press(cx, &["cmd-w"]);
    assert_eq!(tabs(cx), (vec!["ui-x".into()], 0, 1));
    cx.update(|cx| assert!(!shark.read(cx).chats.contains_key("ui-x~2")));
    press(cx, &["cmd-shift-t"]);
    assert_eq!(tabs(cx), (vec!["ui-x".into(), "ui-x~2".into()], 1, 0));
    cx.update(|cx| {
        let ws = &shark.read(cx).state.workspaces[0];
        assert_eq!(
            ws.chats[1].session_id.as_deref(),
            Some("s2"),
            "reopens the session"
        );
    });
    press(cx, &["cmd-t"]);
    press(cx, &["cmd-w"]); // an untouched chat isn't kept for reopening
    assert_eq!(tabs(cx), (vec!["ui-x".into(), "ui-x~2".into()], 1, 0));
    press(cx, &["cmd-w"]);
    assert_eq!(tabs(cx), (vec!["ui-x".into()], 0, 1));
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

    // Model shortcuts work while typing and keep the draft. Let the New workspace
    // dialog's deferred focus restore run first (it targets the sidebar), then go back.
    cx.executor().advance_clock(Duration::from_secs(1));
    press(cx, &["cmd-alt-right", "i"]);
    let model = |cx: &mut TestAppContext| {
        cx.update(|cx| shark.read(cx).state.workspaces[0].chats[0].model.clone())
    };
    press(cx, &["ctrl-cmd-3"]);
    assert_eq!(model(cx).as_deref(), Some("opus"));
    press(cx, &["cmd-/"]);
    cx.run_until_parked();
    press(cx, &["enter"]);
    assert_eq!(
        model(cx).as_deref(),
        Some("opus"),
        "menu opens on the current model"
    );
    press(cx, &["cmd-/"]);
    cx.update_window(window, |_, w, cx| w.input("son", cx))
        .unwrap();
    press(cx, &["enter"]);
    assert_eq!(model(cx).as_deref(), Some("sonnet"));
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(chat.read(cx).focus.is_focused(w), "escape -> transcript")
    })
    .unwrap();
    press(cx, &["cmd-alt-left"]);
    cx.update_window(window, |_, w, cx| {
        assert!(
            shark.read(cx).sidebar_focus.is_focused(w),
            "cmd-alt-left -> sidebar"
        )
    })
    .unwrap();
    assert_menus_reachable(cx, window, "sidebar");
    run(cx, &ShowDiff);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Diff));
    run(cx, &ShowChat);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));

    // The git button follows the worktree: clean, then dirty within a poll tick.
    let sync = cx.update(|cx| shark.read(cx).syncs["ui-x"].clone());
    let label = |cx: &App| sync.read(cx).action.as_ref().map(|a| a.label.clone());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        label(cx).as_deref() == Some("No changes")
    })
    .await;
    fs::write(ws_dir.join("api/new.txt"), "x\n").unwrap();
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        label(cx).as_deref() == Some("Commit and push")
    })
    .await;

    // Diff tree: the new file is listed and can be marked viewed from the tree.
    let refresh = |cx: &mut TestAppContext| {
        cx.update(|cx| shark.update(cx, |s, cx| s.refresh_diff(cx)));
        cx.run_until_parked();
    };
    run(cx, &ShowDiff);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    cx.update(|cx| {
        let names: Vec<String> = shark
            .read(cx)
            .tree_rows
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(names, ["api", "new.txt"]);
    });
    press(cx, &["cmd-alt-right"]);
    cx.update_window(window, |_, w, cx| {
        assert!(
            shark.read(cx).tree_focus.is_focused(w),
            "cmd-alt-right from the diff -> tree"
        )
    })
    .unwrap();
    press(cx, &["down", "v"]);
    let viewed = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).viewed(0));
    assert_eq!(viewed(cx), Viewed::Yes);
    assert!(
        State::load().workspaces[0]
            .viewed
            .contains_key("api/new.txt"),
        "viewed is saved"
    );
    refresh(cx);
    assert_eq!(viewed(cx), Viewed::Yes, "an unchanged file stays viewed");
    let file = ws_dir.join("api/new.txt");
    fs::write(&file, "x\ny\n").unwrap();
    refresh(cx);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).viewed(0) == Viewed::Changed
    })
    .await;
    // `v` on a folder ticks every file in it, then unticks them all.
    press(cx, &["up", "v"]);
    assert_eq!(viewed(cx), Viewed::Yes);
    press(cx, &["v", "down"]);
    assert_eq!(viewed(cx), Viewed::No);

    // `o` opens the file next to the chat and shrinks the sidebar to a rail.
    press(cx, &["o"]);
    let editor = cx.update(|cx| shark.read(cx).editor.clone().expect("editor opened"));
    let text = |cx: &mut TestAppContext| {
        cx.update(|cx| editor.read(cx).input.read(cx).value().to_string())
    };
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "x\ny\n"
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.rail && s.tab == Tab::File);
    });

    // Plain editing and cmd-s.
    cx.update_window(window, |_, w, cx| w.input("z", cx))
        .unwrap();
    assert_eq!(text(cx), "zx\ny\n");
    cx.update(|cx| assert!(editor.read(cx).dirty));
    press(cx, &["cmd-s"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !editor.read(cx).dirty
    })
    .await;
    assert_eq!(fs::read_to_string(&file).unwrap(), "zx\ny\n");
    assert_menus_reachable(cx, window, "editor");

    // Vim keys: normal mode edits, never inserts; the toggle survives a restart.
    press(cx, &["ctrl-cmd-v"]);
    assert!(State::load().vim, "vim toggle is saved");
    press(cx, &["d", "d"]);
    assert_eq!(text(cx), "y\n");
    press(cx, &["u"]);
    cx.run_until_parked();
    assert_eq!(text(cx), "zx\ny\n");
    press(cx, &["j", "q"]);
    assert_eq!(text(cx), "zx\ny\n", "normal mode never types");
    press(cx, &["shift-a", "!", "escape"]);
    assert_eq!(text(cx), "zx\ny!\n");
    // App chords still work in normal mode: `h`/`l` after `ctrl-w` aren't Vim motions.
    press(cx, &["ctrl-w", "l"]);
    cx.update_window(window, |_, w, cx| {
        assert!(
            shark.read(cx).tree_focus.is_focused(w),
            "ctrl-w l from the editor"
        )
    })
    .unwrap();
    press(cx, &["ctrl-w", "h"]);
    cx.update_window(window, |_, w, cx| {
        assert!(
            editor.read(cx).is_focused(w, cx),
            "ctrl-w h back to the editor"
        )
    })
    .unwrap();
    assert_eq!(text(cx), "zx\ny!\n");

    // The agent writes the file while it has unsaved edits: offer reload, don't clobber.
    fs::write(&file, "agent\n").unwrap();
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).conflict
    })
    .await;
    press(cx, &[":", "w", "enter"]);
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "agent\n",
        "save refuses to overwrite"
    );

    // Escape leaves the editor; closing asks before dropping edits.
    press(cx, &["escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).tree_focus.is_focused(w))
    })
    .unwrap();
    run(cx, &CloseFile);
    cx.update(|cx| assert!(shark.read(cx).editor.is_some(), "close waits for confirm"));
    press(cx, &["enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.editor.is_none() && !s.rail && s.tab == Tab::Diff);
    });

    press(cx, &["ctrl-w", "h", "ctrl-w", "h"]);
    run(cx, &ForceArchiveWorkspace);
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).state.workspaces.len(),
            1,
            "force archive waits for confirm"
        )
    });
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.is_empty()
    })
    .await;
    assert!(!ws_dir.exists());

    // Repo picker: tab to the filter, enter ticks the match, space unticks the
    // highlighted (top) row, cmd-enter creates from the picker.
    press(cx, &["cmd-n"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| w.input("kb", cx))
        .unwrap();
    press(cx, &["tab"]);
    cx.update_window(window, |_, w, cx| w.input("web", cx))
        .unwrap();
    press(cx, &["enter"]);
    cx.run_until_parked();
    press(cx, &["space", "cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !shark.read(cx).state.workspaces.is_empty()
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        let names: Vec<&str> = s.state.workspaces[0]
            .repos
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, ["web"]);
        assert_eq!(s.state.last_repos, ["web"]);
    });

    // Sections: name inline, move with `m`, fold with h/l, delete keeps the workspace.
    press(cx, &["ctrl-w", "h"]);
    run(cx, &NewSection);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| w.input("Later", cx))
        .unwrap();
    press(cx, &["enter"]);
    let sections = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).state.sections.clone());
    assert_eq!(sections(cx)[0].name, "Later");
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert!(s.sidebar_focus.is_focused(w) && s.sel == SideRow::Section(0));
    })
    .unwrap();
    press(cx, &["j", "m"]);
    cx.run_until_parked();
    press(cx, &["enter"]);
    assert_eq!(sections(cx)[0].workspaces, ["kb"]);
    press(cx, &["j", "h"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.state.sections[0].collapsed && s.sel == SideRow::Section(0));
        assert!(
            s.ws_order(false).is_empty(),
            "cmd-N skips folded workspaces"
        );
    });
    press(cx, &["l"]);
    run(cx, &DeleteSection);
    assert!(sections(cx).is_empty());
    cx.update(|cx| assert_eq!(shark.read(cx).state.workspaces.len(), 1));
    press(cx, &["ctrl-w", "h"]);
    run(cx, &ArchiveWorkspace);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.is_empty()
    })
    .await;

    // Clicking a repo in the sidebar starts a workspace with only that repo ticked.
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| s.new_workspace(vec!["web".into()], w, cx));
    cx.update_window(window, |_, w, cx| w.input("only web", cx))
        .unwrap();
    press(cx, &["enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("only-web")
    })
    .await;
    let ws_dir = home.join("shark/workspaces/only-web");
    assert!(ws_dir.join("web").exists() && !ws_dir.join("api").exists());

    // Folders dropped on the sidebar become repos; one already added is refused.
    let cli = home.join("src/cli");
    init_repo(&cli);
    let dropped = [cli, home.join("src/api")];
    on(cx, &|s, w, cx| s.add_repo_paths(&dropped, w, cx));
    cx.update(|cx| {
        let names: Vec<_> = shark
            .read(cx)
            .state
            .repos
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(names, ["api", "web", "cli"]);
    });

    // Quick bars are bare menus: no dialog close X.
    press(cx, &["cmd-k"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.within("dialog").try_find("close").is_none(),
            "palette shows a close X"
        );
    })
    .unwrap();
    fs::remove_dir_all(&home).unwrap();
}

/// Diff states, `v` in the diff, refreshes that keep the file, New conversation, archive.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn diff_states_and_archive(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace("st".into(), repos.clone(), None, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.diff_loaded && s.diff_error.is_none() && s.diff_files.is_empty());
        assert!(
            s.diff_placeholder(cx).is_some(),
            "no changes shows the empty state"
        );
    });

    let api = home.join("shark/workspaces/st/api");
    for f in ["a", "b", "c"] {
        fs::write(api.join(f), "x\n").unwrap();
    }
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.update(|cx| assert_eq!(shark.read(cx).diff_files.len(), 3));
    let press = |cx: &mut TestAppContext, k: &str| {
        cx.update_window(window, |_, w, cx| w.press(k, cx)).unwrap();
    };
    let cursor = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            shark
                .read(cx)
                .diff
                .read(cx)
                .selected_lines()
                .map(|r| r.path().to_string())
        })
    };
    // A freshly loaded diff has a line cursor from the first `down`.
    press(cx, "down");
    assert_eq!(cursor(cx).as_deref(), Some("api/a"));
    press(cx, "}");
    press(cx, "v");
    let at = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            let row = s.tree_rows[s.tree_sel].file;
            (
                s.diff_files[s.diff_file].path().to_string(),
                row == Some(s.diff_file),
            )
        })
    };
    assert_eq!(at(cx), ("api/b".into(), true));
    cx.update(|cx| assert_eq!(shark.read(cx).viewed(1), Viewed::Yes, "v in the diff"));
    // Another file changes: the diff and the tree stay on b.
    fs::write(api.join("a"), "y\n").unwrap();
    fs::write(api.join("0"), "new\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    assert_eq!(at(cx), ("api/b".into(), true));
    // down walks a line cursor (past the collapsed, viewed b); a refresh keeps it and `c` targets it.
    press(cx, "down");
    press(cx, "down");
    assert_eq!(cursor(cx).as_deref(), Some("api/c"));
    fs::write(api.join("0"), "newer\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    assert_eq!(
        cursor(cx).as_deref(),
        Some("api/c"),
        "cursor survives a refresh"
    );
    assert_eq!(at(cx).0, "api/c", "v/o follow the cursor");
    press(cx, "c");
    cx.update_window(window, |_, w, cx| w.input("rename this", cx))
        .unwrap();
    press(cx, "enter");
    cx.run_until_parked();
    cx.update(|cx| {
        let p = review::prompt(shark.read(cx).ws_comments());
        assert!(p.contains("\napi/c:1\n> rename this\n"), "{p}");
    });

    // A git failure is not "no changes".
    on(cx, &|s, _, cx| {
        s.state.workspaces[0].repos[0].base = "no/such-base".into();
        s.refresh_diff(cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(
            s.diff_error
                .as_deref()
                .is_some_and(|e| e.starts_with("api: "))
        );
        assert!(s.diff_placeholder(cx).is_some());
    });

    // New conversation forgets the session in state.json too.
    let chat = cx.update(|cx| shark.read(cx).chats["st"].clone());
    on(cx, &|s, _, cx| {
        s.state.workspaces[0].chats[0].session_id = Some("old".into());
        chat.update(cx, |c, _| c.meta.session_id = Some("old".into()));
    });
    let row = COMMANDS
        .iter()
        .position(|r| r.0.starts_with("Start a new conversation"))
        .unwrap();
    on(cx, &|s, w, cx| (COMMANDS[row].4)(s, w, cx));
    assert_eq!(State::load().workspaces[0].chats[0].session_id, None);

    // Archive runs in the background, once.
    on(cx, &|s, w, cx| {
        s.remove_workspace("st", true, w, cx);
        assert!(s.archiving.contains("st"));
        s.remove_workspace("st", true, w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.is_empty()
    })
    .await;
    cx.update(|cx| assert!(shark.read(cx).archiving.is_empty()));
    assert!(!api.exists());
    fs::remove_dir_all(&home).ok();
}

/// A repo joins a running workspace; the open workspace and tab come back after a restart.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn add_repo_and_restore(cx: &mut TestAppContext) {
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
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace("grow".into(), repos[..1].to_vec(), None, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    // A second tab hears about the repo too.
    on(cx, &|s, w, cx| {
        s.new_chat(w, cx);
        s.add_repo_to_workspace("grow".into(), repos[1].clone(), w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces[0].repos.len() == 2
    })
    .await;
    let ws_dir = home.join("shark/workspaces/grow");
    assert!(ws_dir.join("web").exists());
    assert_eq!(State::load().workspaces[0].repos.len(), 2, "saved");
    let chat = cx.update(|cx| shark.read(cx).chats["grow"].clone());
    cx.update(|cx| assert_eq!(chat.read(cx).ws.repos.len(), 2, "the chat knows"));
    let second = cx.update(|cx| shark.read(cx).chats["grow~2"].clone());
    cx.update(|cx| assert_eq!(second.read(cx).ws.repos.len(), 2, "so does the second tab"));
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    assert_eq!(
        State::load().last_open,
        Some(("grow".into(), "diff".into()))
    );

    // A restart reopens it on the Diff tab.
    let (window2, shark2) = open(cx, WindowOptions::default());
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark2.read(cx);
        assert_eq!(s.active.as_deref(), Some("grow"));
        assert_eq!(s.tab, Tab::Diff);
    });
    cx.update_window(window2, |_, w, _| w.remove_window())
        .unwrap();

    // Sends are refused while the worktrees are being removed.
    on(cx, &|s, w, cx| {
        s.remove_workspace("grow", true, w, cx);
        assert!(chat.read(cx).archiving);
        // Also when it is first opened during the removal.
        s.chats.remove("grow");
        s.syncs.remove("grow");
        s.open_workspace_now(0, w, cx);
        assert!(s.chats["grow"].read(cx).archiving);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).state.workspaces.is_empty()
    })
    .await;
    assert!(!ws_dir.exists());
    fs::remove_dir_all(&home).ok();
}

/// Tabs that outgrow the header scroll; a new or selected tab is scrolled into view.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn many_tabs_scroll(cx: &mut TestAppContext) {
    use std::cmp::Ordering;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1000.), px(700.)),
            ))),
            ..Default::default()
        },
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace("tabs".into(), repos.clone(), None, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    for _ in 0..20 {
        on(cx, &|s, w, cx| s.new_chat(w, cx));
    }
    let hidden = |cx: &mut TestAppContext, ix: usize| cx.update(|cx| shark.read(cx).tab_hidden(ix));
    cx.update(|cx| assert_eq!(shark.read(cx).active_ws().unwrap().active_chat, 20));
    assert_eq!(hidden(cx, 20), Ordering::Equal, "the new tab is in view");
    assert_eq!(hidden(cx, 0), Ordering::Less, "the first scrolled off");
    on(cx, &|s, w, cx| s.select_chat(0, w, cx));
    assert_eq!(hidden(cx, 0), Ordering::Equal);
    assert_eq!(hidden(cx, 20), Ordering::Greater);
    fs::remove_dir_all(&home).ok();
}

/// Images and videos in the diff: before/after previews, made lazily for rows on screen,
/// downscaled when big or not decodable by gpui, kept across refreshes.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn media_in_diff(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let src = &repos[0].path;
    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    let icon_size = imagesize::size(&icon).unwrap();
    fs::copy(&icon, src.join("a.png")).unwrap();
    fs::copy(&icon, src.join("del.png")).unwrap();
    git::git(src, &["add", "."]).unwrap();
    git::git(
        src,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "media",
        ],
    )
    .unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos,
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1440.), px(900.)),
            ))),
            ..Default::default()
        },
    );
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace("media".into(), repos.clone(), None, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    let api = home.join("shark/workspaces/media/api");
    let sips = |args: &[&str]| {
        let ok = std::process::Command::new("sips")
            .args(args)
            .output()
            .unwrap();
        assert!(ok.status.success(), "sips {args:?}");
    };
    let p = |name: &str| api.join(name).to_string_lossy().to_string();
    sips(&["-z", "64", "64", &p("a.png")]);
    fs::remove_file(api.join("del.png")).unwrap();
    sips(&[
        "-z",
        "3000",
        "3000",
        &icon.to_string_lossy(),
        "--out",
        &p("big.png"),
    ]);
    sips(&[
        "-s",
        "format",
        "heic",
        &icon.to_string_lossy(),
        "--out",
        &p("new.heic"),
    ]);
    let ffmpeg = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=30",
        ])
        .args(["-t", "1", "-pix_fmt", "yuv420p", &p("clip.mp4")])
        .status()
        .is_ok_and(|s| s.success());
    fs::write(api.join("notes.txt"), "text\n").unwrap();
    fs::write(
        api.join("wide.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="8000" height="4000"><rect width="8000" height="4000" fill="teal"/></svg>"#,
    )
    .unwrap();
    fs::write(api.join("empty.png"), "").unwrap();
    fs::write(
        api.join("lfs.png"),
        "version https://git-lfs.github.com/spec/v1\noid sha256:00\nsize 1\n",
    )
    .unwrap();
    sips(&[
        "-z",
        "500",
        "500",
        &icon.to_string_lossy(),
        "--out",
        &p("race.png"),
    ]);
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    let n = if ffmpeg { 10 } else { 9 };
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == n
    })
    .await;
    let view = |cx: &mut TestAppContext, path: &str| {
        cx.update(|cx| {
            let s = shark.read(cx);
            assert_eq!(s.media.len(), n - 1, "every file but notes.txt");
            (s.media.iter().find(|(p, _)| p == path))
                .unwrap_or_else(|| panic!("no media row for {path}"))
                .1
                .clone()
        })
    };
    // Rows load as they come on screen: the first does, the last (new.heic) waits.
    let first = view(cx, "api/a.png");
    cx.wait_for(window, Duration::from_secs(20), |_, cx| {
        first.read(cx).ready()
    })
    .await;
    cx.update(|cx| assert!(!view_started(&shark, "api/new.heic", cx)));
    let t = Instant::now();
    for _ in 0..20 {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
    }
    let frame_ms = t.elapsed().as_secs_f64() * 1000. / 20.;
    println!("diff frame with media rows: {frame_ms:.2} ms");
    assert!(frame_ms < 50., "frame {frame_ms:.1} ms");
    // Rewritten after the diff ran, before its row loads: not cached under the old hash.
    sips(&["-z", "16", "16", &p("race.png")]);
    for path in [
        "api/big.png",
        "api/clip.mp4",
        "api/del.png",
        "api/empty.png",
        "api/lfs.png",
        "api/new.heic",
        "api/race.png",
        "api/wide.svg",
    ] {
        if path.ends_with(".mp4") && !ffmpeg {
            continue;
        }
        on(cx, &|s, _, cx| {
            s.diff.update(cx, |d, cx| d.scroll_to_file(path, cx))
        });
        cx.wait_for(window, Duration::from_secs(20), |_, cx| {
            view_ready(&shark, path, cx)
        })
        .await;
    }
    let shown = |cx: &mut TestAppContext, path: &str, side: usize| {
        let v = view(cx, path);
        cx.update(|cx| v.read(cx).shown(side))
    };
    let failed = |cx: &mut TestAppContext, path: &str| {
        let v = view(cx, path);
        cx.update(|cx| v.read(cx).failed(1))
    };
    assert_eq!(failed(cx, "api/empty.png").as_deref(), Some("empty file"));
    assert!(failed(cx, "api/lfs.png").is_some_and(|e| e.contains("LFS")));
    let race = failed(cx, "api/race.png");
    assert!(
        race.as_deref()
            .is_some_and(|e| e.contains("changed on disk")),
        "{race:?}"
    );
    let size = |p: &Option<PathBuf>| {
        imagesize::size(p.as_ref().unwrap())
            .map(|s| (s.width, s.height))
            .unwrap()
    };
    // Small PNGs are drawn as they are; both sides are byte-exact copies named by blob hash.
    let (old, old_size) = shown(cx, "api/a.png", 0).unwrap();
    assert_eq!(old_size, Some((icon_size.width, icon_size.height)));
    assert_eq!(fs::read(old.unwrap()).unwrap(), fs::read(&icon).unwrap());
    assert_eq!(shown(cx, "api/a.png", 1).unwrap().1, Some((64, 64)));
    // Big ones are downscaled, HEIC converted.
    let (img, big) = shown(cx, "api/big.png", 1).unwrap();
    assert_eq!(big, Some((3000, 3000)));
    assert!(
        img.as_ref()
            .unwrap()
            .to_string_lossy()
            .ends_with(".preview.png")
    );
    assert_eq!(size(&img), (1200, 1200));
    // A big SVG is drawn down to 1200 px, keeping its shape.
    let (svg, svg_size) = shown(cx, "api/wide.svg", 1).unwrap();
    assert_eq!(svg_size, Some((8000, 4000)));
    assert_eq!(size(&svg), (1200, 600));
    let (heic, _) = shown(cx, "api/new.heic", 1).unwrap();
    assert!(
        heic.as_ref()
            .unwrap()
            .to_string_lossy()
            .ends_with(".preview.png")
    );
    assert!(shown(cx, "api/new.heic", 0).is_none(), "added: no before");
    assert!(shown(cx, "api/del.png", 1).is_none(), "deleted: no after");
    assert!(shown(cx, "api/del.png", 0).is_some());
    // Quick Look may not make posters (no window server); the tile then shows without one.
    if ffmpeg && let (Some(poster), _) = shown(cx, "api/clip.mp4", 1).unwrap() {
        let (w, h) = size(&Some(poster));
        assert!(w > h && w <= 1200, "16:9 poster frame, {w}x{h}");
    }
    // A refresh keeps the rows of unchanged files; a changed file gets a new one.
    sips(&["-z", "32", "32", &p("a.png")]);
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.run_until_parked();
    assert!(view(cx, "api/big.png") == view(cx, "api/big.png"));
    let again = view(cx, "api/a.png");
    assert!(again != first, "a.png changed");
    fs::remove_dir_all(&home).ok();
}

fn view_started(shark: &Entity<Shark>, path: &str, cx: &App) -> bool {
    let s = shark.read(cx);
    s.media
        .iter()
        .any(|(p, v)| p == path && v.read(cx).started())
}

fn view_ready(shark: &Entity<Shark>, path: &str, cx: &App) -> bool {
    let s = shark.read(cx);
    s.media.iter().any(|(p, v)| p == path && v.read(cx).ready())
}

/// Times the hot paths on a big change: 2000 changed files and a 50K-line file.
/// Run in release for real numbers: `cargo test --release --features test-support perf_ -- --nocapture`.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn perf_large_change(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["big"]),
            workspaces: vec![],
            vim: true,
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1440.), px(900.)),
            ))),
            ..Default::default()
        },
    );
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.create_workspace("perf".into(), repos, None, w, cx)
        })
    })
    .unwrap();
    cx.wait_for(window, Duration::from_secs(20), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    let ws_dir = home.join("shark/workspaces/perf/big");
    for i in 0..2000 {
        let dir = ws_dir.join(format!("src/mod{}/sub{}", i % 40, i % 7));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("f{i}.rs")),
            format!("fn f{i}() {{}}\n").repeat(20),
        )
        .unwrap();
    }
    let big: String = (0..50_000usize)
        .map(|i| {
            format!(
                "    let value_{i} = compute(value_{}, \"line {i}\"); // note\n",
                i.saturating_sub(1)
            )
        })
        .collect();
    fs::write(ws_dir.join("big.rs"), &big).unwrap();

    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.;
    let frame = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap()
    };
    let press = |cx: &mut TestAppContext, k: &str| {
        cx.update_window(window, |_, w, cx| w.press(k, cx)).unwrap();
    };
    let mut report = vec![];

    let ws = cx.update(|cx| shark.read(cx).active_ws().unwrap().clone());
    let t = Instant::now();
    let data = DiffData::load(&ws);
    report.push(format!(
        "diff load (git + parse, background): {:.0} ms",
        ms(t)
    ));
    assert_eq!(data.files.len(), 2001);
    // Not `space d`: the new workspace's composer has focus and would take the keys.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.show(Tab::Diff, w, cx))
    })
    .unwrap();
    let t = Instant::now();
    cx.update(|cx| shark.update(cx, |s, cx| s.apply_diff(data, cx)));
    frame(cx);
    report.push(format!("apply diff + first frame: {:.0} ms", ms(t)));
    let t = Instant::now();
    let data = DiffData::load(&ws);
    cx.update(|cx| shark.update(cx, |s, cx| s.apply_diff(data, cx)));
    frame(cx);
    report.push(format!("unchanged refresh + frame: {:.0} ms", ms(t)));

    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.focus_pane(Pane::Tree, w, cx))
    })
    .unwrap();
    frame(cx);
    let n = 200;
    // The tree cursor starts on the first file's row, not on its folders.
    let start = cx.update(|cx| shark.read(cx).tree_sel);
    let t = Instant::now();
    for _ in 0..n {
        press(cx, "j");
        frame(cx);
    }
    let tree_key = ms(t) / n as f64;
    report.push(format!("tree j + frame: {tree_key:.2} ms/key"));
    cx.update(|cx| assert_eq!(shark.read(cx).tree_sel, start + n));
    let t = Instant::now();
    for _ in 0..20 {
        press(cx, "v");
        frame(cx);
    }
    report.push(format!("toggle viewed + frame: {:.2} ms", ms(t) / 20.));
    cx.update(|cx| shark.update(cx, |s, cx| s.tree_select(0, cx)));
    let t = Instant::now();
    press(cx, "v");
    frame(cx);
    report.push(format!("toggle folder viewed + frame: {:.0} ms", ms(t)));
    cx.update(|cx| assert_eq!(shark.read(cx).viewed_now.len(), 2001));
    let t = Instant::now();
    press(cx, "shift-g");
    press(cx, "k");
    press(cx, "h");
    frame(cx);
    press(cx, "l");
    frame(cx);
    report.push(format!("collapse + expand folder: {:.1} ms", ms(t)));

    // big.rs sorts after the folders: the last row.
    let last = cx.update(|cx| shark.read(cx).tree_rows.len() - 1);
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.tree_select(last, cx);
            s.open_file(s.tree_rows[last].file.unwrap(), w, cx)
        })
    })
    .unwrap();
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    // Syntax highlighting of a big file runs on a background thread in the app; the test
    // executor runs it inline, so time only what the main thread does when a file opens.
    cx.wait_for(window, Duration::from_secs(60), |_, cx| {
        editor.read(cx).input.read(cx).text().len() == big.len()
    })
    .await;
    frame(cx);
    let t = Instant::now();
    cx.update_window(window, |_, w, cx| {
        editor.update(cx, |e, cx| {
            e.input.update(cx, |i, cx| i.set_value(big.clone(), w, cx))
        })
    })
    .unwrap();
    frame(cx);
    report.push(format!(
        "open 50K-line file, main thread (set_value + frame): {:.0} ms",
        ms(t)
    ));

    let t = Instant::now();
    (0..50).for_each(|_| frame(cx));
    report.push(format!(
        "editor idle frame (baseline): {:.2} ms",
        ms(t) / 50.
    ));
    let mut keys = |cx: &mut TestAppContext, label: &str, seq: &[&str], rounds: usize| {
        let t = Instant::now();
        for _ in 0..rounds {
            for k in seq {
                press(cx, k);
                frame(cx);
            }
        }
        let per = ms(t) / (rounds * seq.len()) as f64;
        report.push(format!("{label}: {per:.2} ms/key"));
        per
    };
    let motion = keys(cx, "vim j/w/e/b + frame", &["j", "w", "e", "b"], 50);
    keys(cx, "vim ctrl-d / ctrl-u + frame", &["ctrl-d", "ctrl-u"], 20);
    let edit = keys(cx, "vim dd / u + frame", &["d", "d", "u"], 10);
    press(cx, "shift-g");
    frame(cx);
    keys(cx, "vim G / gg + frame", &["g", "g", "shift-g"], 10);
    press(cx, "i");
    let typing = keys(
        cx,
        "insert-mode typing + frame",
        &["a", "b", "c", "space"],
        25,
    );
    press(cx, "escape");
    cx.update(|cx| assert!(editor.read(cx).dirty));
    // Baseline: the editor's own arrow key, so Vim's share of the cost is visible.
    let arrow = keys(
        cx,
        "editor's own down key + frame (baseline)",
        &["down"],
        100,
    );
    println!(
        "\n--- perf ({} build) ---",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    report.iter().for_each(|l| println!("{l}"));
    // Budgets hold in debug too; release is several times faster.
    assert!(tree_key < 50., "tree navigation {tree_key:.1} ms/key");
    assert!(motion < 50., "vim motion {motion:.1} ms/key");
    assert!(edit < 80., "vim edit {edit:.1} ms/key");
    assert!(typing < 50., "typing {typing:.1} ms/key");
    assert!(
        motion < arrow * 1.5 + 1.,
        "vim adds {:.1} ms/key over the editor",
        motion - arrow
    );
    fs::remove_dir_all(&home).ok();
}
