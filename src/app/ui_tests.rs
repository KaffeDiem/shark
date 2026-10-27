//! Headless UI tests: the app on the real keymap, against real repos.
use super::*;
use super::{diff::DiffData, palette::COMMANDS};
use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro that would shadow the builtin
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use std::sync::Mutex;
use std::time::Instant;
use std::{fs, path::PathBuf, time::Duration};

pub(crate) mod chrome_tests;
mod menu_pixels;
mod tab_tests;

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
/// Presses `keys` in `window`, then lets what they started finish.
fn press_in(window: AnyWindowHandle) -> impl Fn(&mut TestAppContext, &[&str]) {
    move |cx, keys| {
        cx.update_window(window, |_, w, cx| keys.iter().for_each(|k| w.press(k, cx)))
            .unwrap();
        cx.run_until_parked();
    }
}

type AppUpdate<'a> = dyn Fn(&mut Shark, &mut Window, &mut Context<Shark>) + 'a;

fn on_app(
    window: AnyWindowHandle,
    shark: Entity<Shark>,
) -> impl Fn(&mut TestAppContext, &AppUpdate<'_>) {
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

/// Clicking a theme card in Settings › Appearance picks that family for its mode.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn settings_theme_cards(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        w.press("ctrl-tab", cx); // General → Appearance
        w.render_frame(cx);
        w.click(("theme-dark", 2usize), cx);
        assert_eq!(cx.theme().dark_theme.name.as_ref(), "Everest Dark");
    })
    .unwrap();
    assert_eq!(State::load().appearance.dark, "Everest");
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
            s.create_workspace(
                name.into(),
                git::on_branch(&repos, name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    let press = press_in(window);
    let sel = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).sel);
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
        assert!(
            s.chats.values().all(|c| c.read(cx).vim_on),
            "every chat follows"
        );
    });
    press(cx, &["j"]);
    assert_ne!(sel(cx), top, "j moves with Vim on");

    // The cursor is on a workspace that isn't open: cmd-k's archive takes that one.
    let open = cx.update(|cx| shark.read(cx).active.clone().unwrap());
    let name = |cx: &mut TestAppContext| {
        let SideRow::Ws(ix) = sel(cx) else {
            panic!("cursor on a workspace")
        };
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

/// PR sources keep their identity through refreshes and failures, and create an isolated
/// review branch against the PR base, including when the source branch is not on origin.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pull_request_picker(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let src = &repos[0].path;
    let origin = home.join("api.git");
    git::git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    git::git(src, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
    git::git(src, &["push", "-q", "origin", "main"]).unwrap();
    git::git(src, &["checkout", "-qb", "colleague/long-feature-branch"]).unwrap();
    fs::write(src.join("review.txt"), "colleague's change\n").unwrap();
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
            "review",
        ],
    )
    .unwrap();
    let head = git::git(src, &["rev-parse", "HEAD"]).unwrap();
    git::git(src, &["push", "-q", "origin", "HEAD:refs/pull/7/head"]).unwrap();
    git::git(src, &["checkout", "-q", "main"]).unwrap();
    git::git(
        src,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.display()),
            "https://github.com/acme/api.git",
        ],
    )
    .unwrap();
    let pr = |number| {
        serde_json::json!({
            "number": number, "title": format!("Review change {number}"),
            "author": {"login": "colleague"}, "headRefName": "colleague/long-feature-branch",
            "baseRefName": "main", "url": format!("https://github.com/acme/api/pull/{number}"),
            "state": "OPEN", "isDraft": number == 8,
        })
    };
    let data = home.join("prs.json");
    fs::write(&data, serde_json::to_string(&vec![pr(7), pr(8)]).unwrap()).unwrap();
    let fake = home.join("gh");
    fs::write(&fake, format!("#!/bin/sh\ncase \"$*\" in *--head*) echo '[]'; exit;; esac\n[ ! -f '{}' ] || {{ echo 'offline' >&2; exit 1; }}\ncat '{}'\n", home.join("fail-gh").display(), data.display())).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let _gh = GhGuard::set(&fake);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let press = |cx: &mut TestAppContext, keys: &[&str]| {
        cx.update_window(window, |_, w, cx| keys.iter().for_each(|k| w.press(k, cx)))
            .unwrap();
        cx.run_until_parked();
    };
    let input = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        cx.run_until_parked();
    };
    let chosen = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.find(("repo-branch", 0usize)).label().unwrap().to_string()
        })
        .unwrap()
    };
    press(cx, &["cmd-n", "tab", "cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_none() && w.try_find(("source-pr", 0usize)).is_some()
    })
    .await;
    // A PR stays highlighted when a newer item moves ahead of it.
    fs::write(&data, serde_json::to_string(&vec![pr(8), pr(7)]).unwrap()).unwrap();
    press(cx, &["cmd-r"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_none()
    })
    .await;
    press(cx, &["enter"]);
    assert!(chosen(cx).contains("#7 · colleague/long-feature-branch"));
    // Picking a branch clears the PR source; PRs are searchable by author/head as well.
    press(cx, &["cmd-b", "enter"]);
    assert_eq!(chosen(cx), "api branch: new");
    press(cx, &["cmd-p"]);
    input(cx, "7");
    press(cx, &["enter"]);
    assert!(chosen(cx).contains("#7"));
    // A filtered PR also stays selected when its new position was previously filtered out.
    press(cx, &["cmd-p"]);
    input(cx, "7");
    fs::write(&data, serde_json::to_string(&vec![pr(7), pr(8)]).unwrap()).unwrap();
    press(cx, &["cmd-r"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_none()
    })
    .await;
    press(cx, &["enter"]);
    assert!(chosen(cx).contains("#7"));
    // Failed refresh retains cached choices and reports failure, not an empty result.
    fs::write(home.join("fail-gh"), "").unwrap();
    press(cx, &["cmd-p"]);
    input(cx, "7");
    press(cx, &["cmd-r"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_some_and(|e| {
            e.label()
                .is_some_and(|s| s.contains("Refresh failed") && s.contains("offline"))
        })
    })
    .await;
    press(cx, &["enter", "cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("pr-7")
    })
    .await;
    let wt = home.join("shark/workspaces/pr-7/api");
    assert_eq!(git::git(&wt, &["rev-parse", "HEAD"]).unwrap(), head);
    assert_eq!(
        git::git(&wt, &["branch", "--show-current"]).unwrap(),
        "pr/7"
    );
    assert_eq!(
        git::git(src, &["branch", "--show-current"]).unwrap(),
        "main"
    );
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).ws("pr-7").unwrap().repos[0].base,
            "refs/shark/pull/7/base"
        )
    });
    // An explicit name creates another review branch without resetting the first one.
    press(cx, &["cmd-n"]);
    input(cx, "second-review");
    press(cx, &["tab", "cmd-p"]);
    input(cx, "7");
    press(cx, &["enter", "cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("second-review")
    })
    .await;
    assert_eq!(
        git::git(
            &home.join("shark/workspaces/second-review/api"),
            &["branch", "--show-current"]
        )
        .unwrap(),
        "second-review"
    );
}

/// The New workspace dialog's branch mode: cmd-b or the row label lists a repo's branches;
/// enter picks one, tab goes back, a checked-out branch is refused, the first row restores
/// the new branch, a fetch finds branches only origin has, and the branch name may be left
/// empty when every ticked repo has an existing branch.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_picker(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    for r in &repos {
        let origin = home.join(format!("{}.git", r.name));
        git::git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        git::git(
            &r.path,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )
        .unwrap();
        git::git(
            &r.path,
            &["push", "-q", "origin", "main", "main:shared", "main:kd/a"],
        )
        .unwrap();
    }
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            last_repos: vec!["api".into(), "web".into()],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let press = |cx: &mut TestAppContext, keys: &[&str]| {
        cx.update_window(window, |_, w, cx| keys.iter().for_each(|k| w.press(k, cx)))
            .unwrap();
        cx.run_until_parked();
    };
    let input = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        cx.run_until_parked();
    };
    // What repo row `i` says it gets; `None` while the branch list is shown.
    let gets = |cx: &mut TestAppContext, i: usize| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let row = w.try_find(("repo-branch", i))?;
            Some(row.label()?.split(": ").nth(1)?.to_string())
        })
        .unwrap()
    };
    // Highlights repo `name` in the list and lists its branches, filtered by `filter`.
    let branches_of = |cx: &mut TestAppContext, name: &str, filter: &str| {
        input(cx, name);
        press(cx, &["cmd-b"]);
        input(cx, filter);
    };
    let on = |ws: &str, repo: &str| {
        let wt = home.join("shark/workspaces").join(ws).join(repo);
        git::git(&wt, &["branch", "--show-current"]).unwrap()
    };
    let count = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).state.workspaces.len());

    // The row label says new or existing as the name is typed.
    press(cx, &["cmd-n"]);
    assert_eq!(gets(cx, 0).as_deref(), Some("new"));
    input(cx, "shared");
    assert_eq!(
        (gets(cx, 0).as_deref(), gets(cx, 1).as_deref()),
        (Some("existing"), Some("existing"))
    );

    // A branch checked out in the source checkout is refused; tab goes back unpicked.
    press(cx, &["cmd-a", "backspace"]);
    input(cx, "x1");
    press(cx, &["tab"]);
    branches_of(cx, "api", "main");
    assert_eq!(gets(cx, 0), None, "the branch list replaces the repos");
    press(cx, &["enter"]);
    assert_eq!(gets(cx, 0), None, "refused: still listing branches");
    press(cx, &["tab"]);
    assert_eq!(gets(cx, 0).as_deref(), Some("new"));
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("x1")
    })
    .await;
    assert_eq!(
        (on("x1", "api"), on("x1", "web")),
        ("x1".into(), "x1".into())
    );

    // Picking, then the first row puts the new branch back. Clicking the label lists too.
    press(cx, &["cmd-n"]);
    input(cx, "y1");
    press(cx, &["tab"]);
    branches_of(cx, "web", "shared");
    press(cx, &["enter"]);
    input(cx, "");
    let web = (0..2)
        .find(|&i| gets(cx, i).as_deref() == Some("shared"))
        .unwrap();
    // Back on the repos, the highlight is on web: space unticks and reticks web.
    let ticked = |cx: &mut TestAppContext, i: usize| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.find(("repo-pick", i)).checked()
        })
        .unwrap()
    };
    press(cx, &["space"]);
    assert_eq!(
        (ticked(cx, web), ticked(cx, 1 - web)),
        (Some(false), Some(true))
    );
    press(cx, &["space"]);
    assert_eq!(ticked(cx, web), Some(true));
    cx.update_window(window, |_, w, cx| w.click(("repo-branch", web), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(gets(cx, 0), None, "the click lists web's branches");
    press(cx, &["enter"]);
    assert_eq!(gets(cx, web).as_deref(), Some("new"));
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("y1")
    })
    .await;
    assert_eq!(on("y1", "web"), "y1");

    // Without a name, every ticked repo needs a branch: one is not enough.
    press(cx, &["cmd-n", "tab"]);
    branches_of(cx, "api", "kd/a");
    press(cx, &["enter"]);
    press(cx, &["cmd-enter"]);
    assert_eq!(count(cx), 2, "web has no branch and none was named");
    // web gets one that only origin has, so far: the fetch on cmd-b finds it.
    let web_origin = home.join("web.git");
    git::git(&web_origin, &["branch", "fresh", "main"]).unwrap();
    // The refusal focused the empty name; back to the list.
    press(cx, &["tab"]);
    input(cx, "web");
    press(cx, &["cmd-b"]);
    input(cx, "fresh");
    press(cx, &["enter"]);
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("kd-a")
    })
    .await;
    assert_eq!(
        (on("kd-a", "api"), on("kd-a", "web")),
        ("kd/a".into(), "fresh".into())
    );
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.ws("kd-a").unwrap();
        assert_eq!(ws.branch, "kd/a", "named after the first picked branch");
        let own: Vec<_> = ws.repos.iter().map(|r| r.branch.clone()).collect();
        assert_eq!(own, [None, Some("fresh".into())]);
    });
    fs::remove_dir_all(&home).unwrap();
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

    // Chat tabs: cmd-t opens one next to the current chat, cmd-shift-] cycles, cmd-w closes,
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
    press(cx, &["cmd-shift-]"]);
    assert_eq!(tabs(cx).1, 0);
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));
    press(cx, &["cmd-shift-]"]);
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
    press(cx, &["ctrl-cmd-2"]);
    assert_eq!(model(cx).as_deref(), Some("opus"));
    press(cx, &["cmd-/"]);
    cx.run_until_parked();
    press(cx, &["enter"]);
    assert_eq!(
        model(cx).as_deref(),
        Some("opus"),
        "menu opens on the current model"
    );
    // The menu jumps to the row that starts with what's typed.
    press(cx, &["cmd-/"]);
    cx.run_until_parked();
    press(cx, &["s", "enter"]);
    assert_eq!(model(cx).as_deref(), Some("sonnet"));
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

    // Fast mode is codex's: cmd-shift-f toggles it there and is saved with the chat; plan
    // mode's shift-tab works on codex too. Both keep the draft.
    let saved = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let m = &shark.read(cx).state.workspaces[0].chats[0];
            (m.fast, m.plan)
        })
    };
    press(cx, &["cmd-shift-f"]);
    assert_eq!(saved(cx), (None, false), "not on a claude model");
    press(cx, &["ctrl-cmd-5"]);
    assert_eq!(model(cx).as_deref(), Some("codex:gpt-6.1-sol"));
    let by_config = cx.update(|cx| chat.read(cx).fast()); // this machine's codex config
    press(cx, &["cmd-shift-f"]);
    assert_eq!(saved(cx), (Some(!by_config), false));
    press(cx, &["cmd-shift-f", "shift-tab"]);
    assert_eq!(saved(cx), (Some(by_config), true));
    press(cx, &["shift-tab", "ctrl-cmd-3"]);
    assert_eq!(saved(cx), (Some(by_config), false));
    assert_eq!(model(cx).as_deref(), Some("sonnet"));
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));

    // Thinking level: cmd-shift-. / cmd-shift-, step it, stopping at the top. The last pick
    // is remembered: cmd-t opens a chat on it.
    let effort = |cx: &mut TestAppContext| {
        cx.update(|cx| shark.read(cx).state.workspaces[0].chats[0].effort.clone())
    };
    press(
        cx,
        &["cmd-shift-.", "cmd-shift-.", "cmd-shift-.", "cmd-shift-,"],
    );
    assert_eq!(effort(cx).as_deref(), Some("xhigh"));
    cx.update(|cx| {
        let st = &shark.read(cx).state;
        assert_eq!(st.last_model.as_deref(), Some("sonnet"));
        assert_eq!(st.last_effort.as_deref(), Some("xhigh"));
    });
    press(cx, &["cmd-t"]);
    cx.update(|cx| {
        let m = &shark.read(cx).state.workspaces[0].chats[1];
        assert_eq!(
            (m.model.as_deref(), m.effort.as_deref()),
            (Some("sonnet"), Some("xhigh"))
        );
    });
    press(cx, &["cmd-w"]);
    cx.update(|cx| assert_eq!(chat.read(cx).composer.read(cx).value().as_ref(), "jk g"));
    // The model button shows both; its menu opens on the level, and a letter typed again
    // cycles the rows it starts: "h" is Haiku, then High.
    cx.update(|cx| assert_eq!(chat.read(cx).model_chip(), "Sonnet · Extra high"));
    press(cx, &["cmd-/"]);
    cx.run_until_parked();
    press(cx, &["h", "h", "enter"]);
    assert_eq!(effort(cx).as_deref(), Some("high"));
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
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.toggle_tree(w, cx))
    })
    .unwrap();
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

    // `o` opens the full-width file editor and shrinks the sidebar to a rail.
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
    on(cx, &|s, w, cx| {
        s.new_workspace(vec!["web".into()], None, w, cx)
    });
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

    // cmd-b lists the highlighted repo's branches; enter picks one for it and ticks it.
    let (api, origin) = (home.join("src/api"), home.join("origin.git"));
    git::git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
    git::git(
        &api,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    )
    .unwrap();
    git::git(&api, &["push", "-q", "origin", "main:remote-only"]).unwrap();
    press(cx, &["cmd-n"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| w.input("new-x", cx))
        .unwrap();
    press(cx, &["tab"]);
    cx.update_window(window, |_, w, cx| w.input("api", cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, &["cmd-b"]);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| w.input("remote", cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, &["enter"]);
    cx.run_until_parked();
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("new-x")
    })
    .await;
    let on_branch = |r: &str| {
        let wt = home.join("shark/workspaces/new-x").join(r);
        git::git(&wt, &["branch", "--show-current"]).unwrap()
    };
    assert_eq!(on_branch("api"), "remote-only");
    assert_eq!(
        on_branch("web"),
        "new-x",
        "the last-used repo gets the new branch"
    );
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.ws("new-x").unwrap();
        assert_eq!(ws.branch, "new-x");
        let api = ws.repos.iter().find(|r| r.name == "api").unwrap();
        assert_eq!(api.branch.as_deref(), Some("remote-only"));
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
        s.create_workspace(
            "st".into(),
            git::on_branch(&repos, "st"),
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
    press(cx, "cmd-enter");
    cx.run_until_parked();
    cx.update(|cx| {
        let p = review::prompt(shark.read(cx).ws_comments(), "");
        assert!(
            p.contains(" api/c:1 · working tree\n```\nx\n```\n> rename this\n"),
            "{p}"
        );
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
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.archiving.is_empty());
        assert!(!s.review.comments.contains_key("st"), "its comments go too");
    });
    assert!(!review::comments_path("st").exists());
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
        s.create_workspace(
            "grow".into(),
            git::on_branch(&repos[..1], "grow"),
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

/// Ctrl+digits chooses open tabs; holding Control previews their current numbers.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn numbered_tab_shortcuts(cx: &mut TestAppContext) {
    use gpui_kit::test::TestEventExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    press(cx, &["ctrl-1", "ctrl-9"]);
    cx.update(|cx| assert!(shark.read(cx).active.is_none()));
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "numbers".into(),
            git::on_branch(&repos, "numbers"),
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
    press(cx, &["cmd-t", "cmd-t"]);
    let selected =
        |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).active_ws().unwrap().active_chat);
    assert_eq!(selected(cx), 2);
    press(cx, &["ctrl-1"]);
    assert_eq!(selected(cx), 0);
    let first = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    cx.update_window(window, |_, w, cx| w.input("keep this draft", cx))
        .unwrap();
    press(cx, &["ctrl-2"]);
    assert_eq!(selected(cx), 1);
    let second = cx.update(|cx| shark.read(cx).active_chat().unwrap().clone());
    cx.update_window(window, |_, w, cx| {
        assert!(
            second
                .read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
        assert_eq!(
            first.read(cx).composer.read(cx).value().as_ref(),
            "keep this draft"
        );
        w.input("second draft", cx);
    })
    .unwrap();
    press(cx, &["ctrl-9", "ctrl-0"]);
    assert_eq!(selected(cx), 1, "missing numbers do nothing");

    // The same Kbd preview as the sidebar appears only for its exact modifier.
    let home_width = cx
        .update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.find("home-row").bounds().size.width
        })
        .unwrap();
    let mut visual = VisualTestContext::from_window(window, cx);
    for (modifiers, tab_hints, workspace_hints) in [
        (Modifiers::default(), false, false),
        (
            Modifiers {
                control: true,
                ..Default::default()
            },
            true,
            false,
        ),
        (
            Modifiers {
                control: true,
                shift: true,
                ..Default::default()
            },
            false,
            false,
        ),
        (
            Modifiers {
                control: true,
                alt: true,
                ..Default::default()
            },
            false,
            false,
        ),
        (
            Modifiers {
                control: true,
                platform: true,
                ..Default::default()
            },
            false,
            false,
        ),
        (
            Modifiers {
                platform: true,
                ..Default::default()
            },
            false,
            true,
        ),
        (Modifiers::default(), false, false),
    ] {
        cx.update_window(window, |_, w, cx| {
            w.change_modifiers(modifiers, cx);
        })
        .unwrap();
        for hint in ["kbd:ctrl-1", "kbd:ctrl-2", "kbd:ctrl-3", "kbd:ctrl-4"] {
            assert_eq!(visual.debug_bounds(hint).is_some(), tab_hints, "{hint}");
        }
        assert!(visual.debug_bounds("kbd:ctrl-5").is_none());
        assert_eq!(visual.debug_bounds("kbd:cmd-1").is_some(), workspace_hints);
        cx.update_window(window, |_, w, _| {
            let width = w.find("home-row").bounds().size.width;
            if workspace_hints {
                assert!(
                    width > home_width,
                    "Home grows to show cmd-0 only while Command is held"
                );
            } else {
                assert_eq!(
                    width, home_width,
                    "Home reserves no space for a hidden shortcut"
                );
            }
        })
        .unwrap();
    }
    assert_eq!(selected(cx), 1, "preview alone never switches chats");
    press(cx, &["ctrl-cmd-2"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active_ws().unwrap().active_chat, 1);
        assert_eq!(second.read(cx).meta.model.as_deref(), Some("opus"));
    });

    // Closing removes a number, reopening restores it in the current visual order.
    cx.update(|cx| {
        second.update(cx, |c, cx| {
            c.meta.session_id = Some("numbered-session".into());
            cx.emit(ChatEvent::Meta);
        })
    });
    let third = cx.update(|cx| shark.read(cx).active_ws().unwrap().chats[2].id.clone());
    press(cx, &["cmd-w", "ctrl-2"]);
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).active_chat().unwrap().read(cx).meta.id,
            third
        )
    });
    press(cx, &["cmd-shift-t", "ctrl-3"]);
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).active_chat().unwrap().read(cx).meta.id,
            second.read(cx).meta.id
        )
    });

    // The shortcut selects the chat from a diff or Vim editor, retaining open files.
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    press(cx, &["ctrl-1"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    fs::write(
        home.join("shark/workspaces/numbers/api/note.txt"),
        "file text\n",
    )
    .unwrap();
    on(cx, &|s, w, cx| {
        s.set_vim(true, cx);
        s.open_path("api/note.txt".into(), w, cx);
    });
    let editor = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    press(cx, &["ctrl-2"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.tab, Tab::Chat);
        assert_eq!(s.editor.as_ref(), Some(&editor));
        assert_eq!(s.active_ws().unwrap().active_chat, 1);
        assert!(
            s.active_chat()
                .unwrap()
                .read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
    })
    .unwrap();

    // Numbers refer to this workspace, while Cmd+digits still chooses workspaces.
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "other".into(),
            git::on_branch(&repos, "other"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("other")
    })
    .await;
    press(cx, &["cmd-t", "ctrl-1"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("other"));
        assert_eq!(s.active_ws().unwrap().active_chat, 0);
        assert_eq!(s.ws("numbers").unwrap().active_chat, 1);
    });
    press(cx, &["cmd-1"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("numbers")));
    press(cx, &["ctrl-1"]);
    cx.update(|cx| {
        assert_eq!(
            first.read(cx).composer.read(cx).value().as_ref(),
            "keep this draft"
        )
    });

    // Nine means ninth, even when a tenth chat exists; the tenth has no number.
    on(cx, &|s, _, cx| s.set_vim(false, cx));
    for _ in 0..7 {
        press(cx, &["cmd-t"]);
    }
    for n in 1..=9 {
        press(cx, &[&format!("ctrl-{n}")]);
        assert_eq!(selected(cx), n - 1);
    }
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(
            Modifiers {
                control: true,
                ..Default::default()
            },
            cx,
        );
    })
    .unwrap();
    assert!(visual.debug_bounds("kbd:ctrl-9").is_some());
    assert!(visual.debug_bounds("kbd:ctrl-10").is_none());
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(Modifiers::default(), cx)
    })
    .unwrap();
}

/// Dragging tabs changes their order, not the selected conversation or its draft.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn drag_chat_tabs(cx: &mut TestAppContext) {
    use gpui_kit::test::TestEventExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "drag-tabs".into(),
            git::on_branch(&repos, "drag-tabs"),
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
    press(cx, &["cmd-t", "cmd-t", "ctrl-2"]);
    let ids = cx.update(|cx| {
        shark
            .read(cx)
            .active_ws()
            .unwrap()
            .chats
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
    });
    let chats = cx.update(|cx| {
        ids.iter()
            .map(|id| shark.read(cx).chats[id].clone())
            .collect::<Vec<_>>()
    });
    cx.update_window(window, |_, w, cx| w.input("keep this draft", cx))
        .unwrap();
    let order = |cx: &mut TestAppContext, expected: &[usize]| {
        cx.update(|cx| {
            let s = shark.read(cx);
            let ws = s.active_ws().unwrap();
            assert_eq!(
                ws.chats.iter().map(|c| &c.id).collect::<Vec<_>>(),
                expected.iter().map(|&i| &ids[i]).collect::<Vec<_>>()
            );
            assert_eq!(s.active_chat(), Some(&chats[1]));
            assert_eq!(
                chats[1].read(cx).composer.read(cx).value().as_ref(),
                "keep this draft"
            );
            for (id, chat) in ids.iter().zip(&chats) {
                assert_eq!(
                    s.chats.get(id),
                    Some(chat),
                    "reordering keeps chat instances"
                );
            }
            let saved = State::load();
            let saved = saved
                .workspaces
                .iter()
                .find(|w| w.name == "drag-tabs")
                .unwrap();
            assert_eq!(
                saved.chats.iter().map(|c| &c.id).collect::<Vec<_>>(),
                expected.iter().map(|&i| &ids[i]).collect::<Vec<_>>()
            );
            assert_eq!(saved.chats[saved.active_chat].id, ids[1]);
        });
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let xs: Vec<_> = expected
                .iter()
                .map(|&i| {
                    w.find(SharedString::from(format!("chat-tab-{}", ids[i])))
                        .bounds()
                        .origin
                        .x
                })
                .collect();
            assert!(
                xs.windows(2).all(|pair| pair[0] < pair[1]),
                "rendered order matches saved order"
            );
        })
        .unwrap();
    };
    let drag = |cx: &mut TestAppContext, from: usize, to: Option<usize>| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let start = w
                .find(SharedString::from(format!("chat-tab-{}", ids[from])))
                .bounds()
                .center();
            let end = to
                .map(|i| {
                    w.find(SharedString::from(format!("chat-tab-{}", ids[i])))
                        .bounds()
                        .center()
                })
                .unwrap_or(point(px(30.), px(400.)));
            w.pointer_down(start, MouseButton::Left, cx);
            w.pointer_move(start + point(px(8.), px(0.)), Some(MouseButton::Left), cx);
            w.pointer_move(end, Some(MouseButton::Left), cx);
            w.pointer_up(end, MouseButton::Left, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    drag(cx, 1, Some(2));
    order(cx, &[0, 2, 1]);
    drag(cx, 0, Some(1));
    order(cx, &[2, 1, 0]);
    drag(cx, 0, Some(2));
    order(cx, &[0, 2, 1]);
    drag(cx, 1, Some(1));
    order(cx, &[0, 2, 1]);
    drag(cx, 0, None);
    order(cx, &[0, 2, 1]);
    // Payloads from another workspace or tabs closed during a drag cannot reorder this strip.
    for (workspace, from, to) in [
        ("other", ids[0].as_str(), ids[2].as_str()),
        ("drag-tabs", "closed-chat", ids[2].as_str()),
        ("drag-tabs", ids[0].as_str(), "closed-chat"),
    ] {
        on(cx, &|s, w, cx| s.move_chat(workspace, from, to, w, cx));
        order(cx, &[0, 2, 1]);
    }

    // Number shortcuts follow the new visual order, with each draft still in its chat.
    for (number, expected) in [(1, 0), (2, 2), (3, 1)] {
        press(cx, &[&format!("ctrl-{number}")]);
        cx.update(|cx| assert_eq!(shark.read(cx).active_chat(), Some(&chats[expected])));
    }
    order(cx, &[0, 2, 1]);
    cx.update(|cx| {
        chats[1].update(cx, |c, cx| {
            c.meta.session_id = Some("reordered-session".into());
            cx.emit(ChatEvent::Meta);
        })
    });
    press(cx, &["cmd-w", "ctrl-1", "cmd-shift-t"]);
    cx.update(|cx| {
        let ws = shark.read(cx).active_ws().unwrap();
        assert_eq!(
            ws.chats.iter().map(|c| &c.id).collect::<Vec<_>>(),
            ids.iter().collect::<Vec<_>>()
        );
        assert_eq!(ws.chats[ws.active_chat].id, ids[1]);
    });
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
        s.create_workspace(
            "tabs".into(),
            git::on_branch(&repos, "tabs"),
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
    // Edge buttons browse hidden tabs without selecting one or snapping back on redraw.
    cx.update_window(window, |_, w, cx| {
        for _ in 0..3 {
            w.render_frame(cx);
            w.simulate_next_frame(cx);
        }
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| w.click("tabs-right", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        for _ in 0..3 {
            w.render_frame(cx);
            w.simulate_next_frame(cx);
        }
        assert_eq!(shark.read(cx).active_ws().unwrap().active_chat, 0);
        assert_eq!(
            shark.read(cx).tab_hidden(0),
            Ordering::Less,
            "manual scrolling stays put"
        );
    })
    .unwrap();
    on(cx, &|s, w, cx| s.select_chat(20, w, cx));
    for width in [800., 1440., 900.] {
        cx.simulate_window_resize(window, size(px(width), px(700.)));
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            for _ in 0..3 {
                w.render_frame(cx);
                w.simulate_next_frame(cx);
            }
            assert_eq!(
                shark.read(cx).tab_hidden(20),
                Ordering::Equal,
                "selected tab follows resize to {width}"
            );
        })
        .unwrap();
    }
    fs::remove_dir_all(&home).ok();
}

/// Titlebar controls stay usable on Home and beside overflowing tabs, at either sidebar width.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn titlebar_controls(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    for width in [800., 1000., 1440.] {
        let (window, shark) = boot(
            cx,
            State {
                repos: repos.clone(),
                ..Default::default()
            },
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(width), px(700.)),
                ))),
                ..Default::default()
            },
        );
        let press = press_in(window);
        let on = on_app(window, shark.clone());
        let check_controls = |cx: &mut TestAppContext, rail: bool| {
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                for id in ["toggle-sidebar", "new-workspace"] {
                    let controls = w.find_all(id);
                    assert_eq!(controls.len(), 1, "one {id}, rail={rail}, width={width}");
                    assert!(
                        controls[0].visible(),
                        "visible {id}, rail={rail}, width={width}"
                    );
                }
                let toggle = w.find("toggle-sidebar").bounds();
                if !rail {
                    assert!(
                        w.find("home-row").bounds().size.width < px(150.),
                        "Home fits its label"
                    );
                }
                for id in ["show-tree", "hide-tree"] {
                    if let Some(right) = w.try_find(id) {
                        assert_eq!(
                            right.bounds().top(),
                            toggle.top(),
                            "{id} aligns with left toggle"
                        );
                        assert_eq!(right.bounds().size.height, toggle.size.height);
                    }
                }
                assert_eq!(toggle.origin, point(px(80.), px(8.)));
                assert_eq!(toggle.size, size(px(32.), px(28.)));
                let titlebar = w.find("workspace-titlebar").bounds();
                assert_eq!(titlebar.top(), px(0.));
                assert_eq!(titlebar.size.height, px(44.));
                assert_eq!(w.find("content-island").bounds().top(), titlebar.bottom());
                assert!(
                    cx.theme().notification.margins.top >= titlebar.bottom() + px(44.),
                    "toasts clear the titlebar and workspace toolbar"
                );
                let create = w.find("new-workspace").bounds();
                if rail {
                    assert_eq!(create.left(), px(116.));
                    assert_eq!(create.center().y, toggle.center().y);
                    assert!(create.left() >= toggle.right());
                } else {
                    assert!(
                        create.top() >= px(44.),
                        "expanded create control is below the titlebar"
                    );
                    assert!(create.right() <= w.find("sidebar").bounds().right());
                    assert_eq!(create.center().y, w.find("home-row").bounds().center().y);
                }
                w.click("new-workspace", cx);
                assert!(
                    w.has_active_dialog(cx),
                    "create opens New workspace, rail={rail}, width={width}, tab={:?}, tools={}",
                    shark.read(cx).tab,
                    shark.read(cx).tree_shown()
                );
                w.close_dialog(cx);
            })
            .unwrap();
            cx.run_until_parked();
        };
        // The create action is available before a workspace exists, in both sidebar modes.
        check_controls(cx, false);
        press(cx, &["cmd-b"]);
        check_controls(cx, true);
        press(cx, &["cmd-b"]);
        let name = format!("chrome-{}", width as usize);
        on(cx, &|s, w, cx| {
            s.create_workspace(
                name.clone(),
                git::on_branch(&repos, &name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name.as_str())
        })
        .await;
        for _ in 0..20 {
            on(cx, &|s, w, cx| s.new_chat(w, cx));
        }
        for rail in [false, true] {
            check_controls(cx, rail);
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let s = shark.read(cx);
                let ws = s.active_ws().unwrap();
                let tab = w.find(SharedString::from(format!(
                    "chat-tab-{}",
                    ws.chats[ws.active_chat].id
                )));
                let new_chat = w.find("new-chat");
                assert_eq!(tab.bounds().top(), px(8.));
                assert_eq!(tab.bounds().size.height, px(28.));
                assert!(new_chat.visible(), "overflow leaves new-chat visible");
                assert!(new_chat.bounds().left() >= tab.bounds().right());
                assert!(new_chat.bounds().right() <= px(width - 72.));
                if rail {
                    assert!(tab.bounds().left() >= w.find("new-workspace").bounds().right());
                }
                assert_eq!(s.tab_hidden(ws.active_chat), std::cmp::Ordering::Equal);
                w.click("new-chat", cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update(|cx| {
                let ws = shark.read(cx).active_ws().unwrap();
                assert_eq!(ws.chats.len(), if rail { 23 } else { 22 });
                assert_eq!(ws.active_chat, ws.chats.len() - 1);
            });
            if !rail {
                press(cx, &["cmd-b"]);
            }
        }
        let path = format!("api/{}.txt", "long-file-name-".repeat(10));
        fs::write(
            home.join("shark/workspaces").join(&name).join(&path),
            "text\n",
        )
        .unwrap();
        on(cx, &|s, w, cx| s.open_path(path.clone(), w, cx));
        let file_tab = SharedString::from(format!("file-tab-{path}"));
        let chat_tab = cx.update(|cx| {
            let ws = shark.read(cx).active_ws().unwrap();
            SharedString::from(format!("chat-tab-{}", ws.chats[ws.active_chat].id))
        });
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.simulate_next_frame(cx); // Native frames reveal the selected tab after the file changes its viewport.
            w.render_frame(cx);
            let file = w.find(file_tab.clone());
            assert_eq!(w.find("chat-tabs").role(), Some(Role::TabList));
            assert_eq!(file.role(), Some(Role::Tab));
            assert_eq!(file.selected(), Some(true));
            assert_eq!(file.label(), Some(path.as_str()));
            assert_eq!(w.find("diff-tab").selected(), Some(false));
            assert_eq!(w.find(chat_tab.clone()).selected(), Some(false));
            assert!(file.visible(), "the file tab survives chat overflow");
            assert_eq!(file.bounds().top(), px(8.));
            assert_eq!(file.bounds().size.height, px(28.));
            assert!(file.bounds().right() <= w.find("new-chat").bounds().left());
            assert_eq!(
                file.bounds().size.width,
                w.find("diff-tab").bounds().size.width
            );
            assert_eq!(
                file.bounds().size.width,
                w.find(chat_tab.clone()).bounds().size.width
            );
            assert!(file.bounds().right() <= w.find("workspace-titlebar").bounds().right());
            let file_ix = shark.read(cx).active_ws().unwrap().chats.len() + 1;
            assert_eq!(
                shark.read(cx).tab_hidden(file_ix),
                std::cmp::Ordering::Equal,
                "opening a file reveals its selected tab"
            );
            assert_eq!(shark.read(cx).tab, Tab::File);
            assert!(shark.read(cx).editor.is_some());
        })
        .unwrap();
        on(cx, &|s, w, cx| {
            let active = s.active_ws().unwrap().active_chat;
            s.select_chat(active, w, cx);
            s.show(Tab::Chat, w, cx);
        });
        cx.update_window(window, |_, w, cx| {
            w.click(chat_tab.clone(), cx);
            assert_eq!(shark.read(cx).tab, Tab::Chat);
        })
        .unwrap();
        on(cx, &|s, w, cx| s.select_file(0, w, cx));
        cx.update_window(window, |_, w, cx| {
            w.click(file_tab.clone(), cx);
            assert_eq!(shark.read(cx).tab, Tab::File);
        })
        .unwrap();
        on(cx, &|s, w, cx| s.toggle_tree(w, cx));
        for rail in [true, false] {
            check_controls(cx, rail);
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let titlebar = w.find("workspace-titlebar").bounds();
                for id in [file_tab.clone(), SharedString::from("new-chat")] {
                    let control = w.find(id.clone());
                    assert!(control.visible(), "visible {id} with Workspace tools open");
                    assert!(
                        control.bounds().left() >= titlebar.left(),
                        "{id} inside titlebar, width={width}, rail={rail}: control={:?}, titlebar={titlebar:?}", control.bounds()
                    );
                    assert!(
                        control.bounds().right() <= titlebar.right(),
                        "{id} inside titlebar, width={width}, rail={rail}: control={:?}, titlebar={titlebar:?}", control.bounds()
                    );
                }
            })
            .unwrap();
            if rail {
                press(cx, &["cmd-b"]);
            }
        }
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
    }
    fs::remove_dir_all(&home).ok();
}

/// File tabs keep independent buffers, and leaving a workspace protects hidden dirty files.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn file_tabs_preserve_buffers(cx: &mut TestAppContext) {
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
    let press = press_in(window);
    for name in ["files", "other-files"] {
        on(cx, &|s, w, cx| {
            s.create_workspace(
                name.into(),
                git::on_branch(&repos, name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    on(cx, &|s, w, cx| s.open_workspace(0, w, cx));
    let dir = home.join("shark/workspaces/files");
    for (path, text) in [
        ("api/shared.txt", "api text\n"),
        ("web/shared.txt", "web text\n"),
        ("api/replacement.txt", "replacement\n"),
        ("api/final.txt", "final\n"),
    ] {
        fs::write(dir.join(path), text).unwrap();
    }
    on(cx, &|s, w, cx| s.open_path("api/shared.txt".into(), w, cx));
    let first = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        first.read(cx).input.read(cx).value().as_ref() == "api text\n"
    })
    .await;
    cx.update_window(window, |_, w, cx| w.input("unsaved ", cx))
        .unwrap();
    cx.update(|cx| assert!(first.read(cx).dirty));

    // Same basename in another repository opens separately, without prompting about the first.
    on(cx, &|s, w, cx| {
        s.open_path_tab("web/shared.txt".into(), w, cx)
    });
    let second = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        second.read(cx).input.read(cx).value().as_ref() == "web text\n"
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        assert_ne!(first, second);
        assert_eq!(
            first.read(cx).input.read(cx).value().as_ref(),
            "unsaved api text\n"
        );
    })
    .unwrap();
    on(cx, &|s, w, cx| {
        s.open_path_tab("api/shared.txt".into(), w, cx)
    });
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).file_tabs.len(),
            2,
            "opening an existing file deduplicates"
        );
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&first));
    });
    on(cx, &|s, w, cx| s.select_file(1, w, cx));
    on(cx, &|s, _, cx| s.set_vim(true, cx));
    cx.update(|cx| assert!(first.read(cx).vim_on && second.read(cx).vim_on));
    on(cx, &|s, _, cx| s.set_vim(false, cx));

    // Ordinary opening replaces only the clean current tab, retaining a hidden dirty editor.
    on(cx, &|s, w, cx| {
        s.open_path("api/replacement.txt".into(), w, cx)
    });
    let replacement = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        replacement.read(cx).input.read(cx).value().as_ref() == "replacement\n"
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        assert!(
            !w.has_active_dialog(cx),
            "hidden dirty file is not being replaced"
        );
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        w.input("edited ", cx);
    })
    .unwrap();
    on(cx, &|s, w, cx| s.open_path("api/final.txt".into(), w, cx));
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press(cx, &["escape"]);
    cx.update(|cx| assert_eq!(shark.read(cx).editor.as_ref(), Some(&replacement)));
    on(cx, &|s, w, cx| s.open_path("api/final.txt".into(), w, cx));
    press(cx, &["enter"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        assert_eq!(
            shark.read(cx).editor.as_ref().unwrap().read(cx).path,
            "api/final.txt"
        );
        assert!(
            first.read(cx).dirty,
            "discard replaces only the selected buffer"
        );
    });

    // An in-flight save cannot be discarded by closing or leaving the workspace.
    let saving = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    cx.update(|cx| saving.update(cx, |e, _| e.saving = true));
    on(cx, &|s, w, cx| s.close_file(w, cx));
    on(cx, &|s, w, cx| s.open_workspace(1, w, cx));
    cx.update_window(window, |_, w, cx| {
        assert!(
            !w.has_active_dialog(cx),
            "wait for the write instead of offering Discard"
        );
        assert_eq!(shark.read(cx).active.as_deref(), Some("files"));
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
    })
    .unwrap();
    cx.update(|cx| saving.update(cx, |e, _| e.saving = false));
    // Closing a clean tab reveals its neighbor; cancelling a dirty close retains it.
    on(cx, &|s, w, cx| s.close_file(w, cx));
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 1);
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&first));
        assert_eq!(shark.read(cx).tab, Tab::File);
    });
    on(cx, &|s, w, cx| s.close_file(w, cx));
    press(cx, &["escape"]);
    cx.update(|cx| assert_eq!(shark.read(cx).file_tabs.len(), 1));
    on(cx, &|s, w, cx| {
        s.open_path_tab("web/shared.txt".into(), w, cx)
    });
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/shared.txt".into()), w, cx)
    });
    cx.update(|cx| {
        assert!(
            shark.read(cx).editing.is_none(),
            "inline editing refuses hidden unsaved buffers"
        )
    });
    on(cx, &|s, w, cx| s.open_workspace(1, w, cx));
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press(cx, &["escape"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active.as_deref(), Some("files"));
        assert!(first.read(cx).dirty);
    });
    on(cx, &|s, w, cx| s.open_workspace(1, w, cx));
    press(cx, &["enter"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).active.as_deref(), Some("other-files"));
        assert!(shark.read(cx).file_tabs.is_empty());
        assert!(shark.read(cx).editor.is_none());
    });
    assert_eq!(
        fs::read_to_string(dir.join("api/shared.txt")).unwrap(),
        "api text\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("api/replacement.txt")).unwrap(),
        "replacement\n"
    );
    fs::remove_dir_all(&home).ok();
}

/// Command-click opens a file tab from the tree, while ordinary click follows the current view.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn command_click_file_tabs(cx: &mut TestAppContext) {
    use gpui_kit::test::TestEventExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
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
        s.create_workspace(
            "click-files".into(),
            git::on_branch(&repos, "click-files"),
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
    let dir = home.join("shark/workspaces/click-files/api");
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    on(cx, &|s, w, cx| {
        s.show_tree = true;
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 2
    })
    .await;
    let click_file = |cx: &mut TestAppContext, path: &str, command: bool| {
        let row = cx.update(|cx| {
            shark
                .read(cx)
                .tree_rows
                .iter()
                .position(|r| r.path.as_str() == path)
                .unwrap()
        });
        cx.update_window(window, |_, w, cx| {
            w.click_with_modifiers(
                ("tree-row", row),
                Modifiers {
                    platform: command,
                    ..Default::default()
                },
                cx,
            )
        })
        .unwrap();
        cx.run_until_parked();
    };
    for rows in [1, 3] {
        cx.update_window(window, |_, w, cx| {
            w.click_with_modifiers(
                ("tree-row", 0usize),
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                cx,
            );
            assert!(
                shark.read(cx).file_tabs.is_empty(),
                "Command-clicking a folder only folds it"
            );
            assert_eq!(shark.read(cx).tree_rows.len(), rows);
        })
        .unwrap();
        cx.run_until_parked();
    }
    click_file(cx, "api/a.txt", false);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).tab, Tab::Diff);
        assert!(shark.read(cx).file_tabs.is_empty());
    });
    click_file(cx, "api/a.txt", true);
    let first = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    click_file(cx, "api/b.txt", true);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        assert_eq!(
            shark.read(cx).editor.as_ref().unwrap().read(cx).path,
            "api/b.txt"
        );
    });
    click_file(cx, "api/a.txt", true);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 2);
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&first));
    });
    // Ctrl+Tab follows the visual order and wraps through chat, Diff and both files.
    let press = press_in(window);
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    for (key, tab, path) in [
        ("ctrl-tab", Tab::Diff, None),
        ("ctrl-tab", Tab::File, Some("api/a.txt")),
        ("ctrl-tab", Tab::File, Some("api/b.txt")),
        ("ctrl-tab", Tab::Chat, None),
        ("ctrl-shift-tab", Tab::File, Some("api/b.txt")),
        ("ctrl-shift-tab", Tab::File, Some("api/a.txt")),
        ("ctrl-4", Tab::File, Some("api/b.txt")),
        ("ctrl-2", Tab::Diff, None),
        ("ctrl-1", Tab::Chat, None),
        ("ctrl-3", Tab::File, Some("api/a.txt")),
        ("ctrl-9", Tab::File, Some("api/a.txt")),
    ] {
        press(cx, &[key]);
        cx.update(|cx| {
            assert_eq!(shark.read(cx).tab, tab, "{key}");
            if let Some(path) = path {
                assert_eq!(shark.read(cx).editor.as_ref().unwrap().read(cx).path, path);
            }
        });
    }
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(
            Modifiers {
                control: true,
                ..Default::default()
            },
            cx,
        );
    })
    .unwrap();
    let mut visual = VisualTestContext::from_window(window, cx);
    for hint in ["kbd:ctrl-1", "kbd:ctrl-2", "kbd:ctrl-3", "kbd:ctrl-4"] {
        assert!(visual.debug_bounds(hint).is_some(), "{hint}");
    }
    assert!(visual.debug_bounds("kbd:ctrl-5").is_none());
    cx.update_window(window, |_, w, cx| {
        w.change_modifiers(Modifiers::default(), cx);
    })
    .unwrap();
    // A background :wq completion closes its own editor, not the newly selected file.
    let second = cx.update(|cx| shark.read(cx).file_tabs[1].editor.clone());
    cx.update(|cx| second.update(cx, |_, cx| cx.emit(EditorEvent::Close)));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 1);
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&first));
    });
    click_file(cx, "api/b.txt", true);
    on(cx, &|s, w, cx| {
        s.select_file(0, w, cx);
        s.tab_scroll.scroll_to_item(3);
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_move(w.find("file-tab-api/b.txt").bounds().center(), None, cx);
        w.within("file-tab-api/b.txt").click("close-file-tab", cx);
        assert_eq!(
            shark.read(cx).editor.as_ref(),
            Some(&first),
            "closing a background tab preserves selection"
        );
        assert_eq!(shark.read(cx).file_tabs.len(), 1);
    })
    .unwrap();
    click_file(cx, "api/b.txt", true);
    // File tabs remain keyboard-closeable even after the tree takes focus.
    on(cx, &|s, w, cx| s.focus_pane(Pane::Tree, w, cx));
    press_in(window)(cx, &["cmd-w"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).file_tabs.len(), 1);
        assert_eq!(shark.read(cx).active_ws().unwrap().chats.len(), 1);
    });
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    press(cx, &["cmd-w"]);
    cx.update(|cx| {
        assert_eq!(shark.read(cx).tab, Tab::Chat);
        assert_eq!(
            shark.read(cx).active_ws().unwrap().chats.len(),
            1,
            "closing Diff never closes the chat behind it"
        );
        assert_eq!(shark.read(cx).file_tabs.len(), 1);
    });
    // A clean retained editor cannot hide newer unsaved edits made in the diff.
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.edit_working_copy(Some("api/a.txt".into()), w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.is_some()
    })
    .await;
    on(cx, &|s, w, cx| {
        s.editing.as_mut().unwrap().dirty = true;
        s.show(Tab::Diff, w, cx);
    });
    press(cx, &["ctrl-3"]);
    cx.update(|cx| {
        assert_eq!(
            shark.read(cx).tab,
            Tab::Diff,
            "a tab switch must not bypass the inline-edit guard"
        )
    });
    fs::remove_dir_all(&home).ok();
}

/// The chat tab's right-click menu: copy only with a session, close only beside another tab.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn chat_tab_menu(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
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
        s.create_workspace(
            "tabs".into(),
            git::on_branch(&repos, "tabs"),
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
    let labels = |cx: &mut TestAppContext| {
        shark.update(cx, |s, cx| {
            (s.chat_tab_menu(0, cx).into_iter())
                .map(|e| match e {
                    Entry::Item { label, .. } | Entry::Sub { label, .. } => label.to_string(),
                    Entry::Sep => "-".into(),
                })
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(labels(cx), ["New chat"]);
    on(cx, &|s, w, cx| s.new_chat(w, cx));
    on(cx, &|s, _, _| {
        s.active_ws_mut().unwrap().chats[0].session_id = Some("s1".into())
    });
    assert_eq!(
        labels(cx),
        ["New chat", "Copy session ID", "-", "Close chat"]
    );
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
        s.create_workspace(
            "media".into(),
            git::on_branch(&repos, "media"),
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
            s.create_workspace(
                "perf".into(),
                git::on_branch(&repos, "perf"),
                vec![],
                None,
                w,
                cx,
            )
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
    let data = DiffData::load(&ws, false);
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
    let data = DiffData::load(&ws, false);
    cx.update(|cx| shark.update(cx, |s, cx| s.apply_diff(data, cx)));
    frame(cx);
    report.push(format!("unchanged refresh + frame: {:.0} ms", ms(t)));

    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.show_tree = true;
            s.focus_pane(Pane::Tree, w, cx)
        })
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
/// Open in an app: terminals take the workspace folder, Fork asks which repo, Xcode shows
/// only where a repo has a project and opens it. Through cmd-k and the right-click menu.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn open_in_apps(cx: &mut TestAppContext) {
    use crate::apps::OPENED;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["ios", "web"]);
    fs::create_dir_all(repos[0].path.join("App.xcodeproj")).unwrap();
    fs::write(repos[0].path.join("App.xcodeproj/project.pbxproj"), "").unwrap();
    git::git(&repos[0].path, &["add", "."]).unwrap();
    git::git(
        &repos[0].path,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "x",
        ],
    )
    .unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    for (name, picked) in [("web-only", &repos[1..]), ("both", &repos[..])] {
        on(cx, &|s, w, cx| {
            s.create_workspace(
                name.into(),
                git::on_branch(picked, name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    let ws_dir = home.join("shark/workspaces/both");
    let press = press_in(window);
    let run = |cx: &mut TestAppContext, query: &str| {
        press(cx, &["cmd-k"]);
        cx.update_window(window, |_, w, cx| w.input(query, cx))
            .unwrap();
        press(cx, &["enter"]);
    };
    let last = || OPENED.lock().unwrap().last().cloned();

    // The right-click menu: (label, hint, submenu rows) per "Open in" entry.
    let menu_of = |cx: &mut TestAppContext, ws: &str| {
        shark.update(cx, |s, cx| {
            let ws = s.ws(ws).unwrap().clone();
            (ws_menu(s, &ws, cx).into_iter())
                .filter_map(|e| match e {
                    Entry::Item { label, hint, .. } if label.starts_with("Open in") => {
                        Some((label.to_string(), hint.map(|h| h.to_string()), vec![]))
                    }
                    Entry::Sub { label, entries, .. } if label.starts_with("Open in") => {
                        let rows = (entries.iter())
                            .filter_map(|r| match r {
                                Entry::Item { label, .. } => Some(label.to_string()),
                                _ => None,
                            })
                            .collect();
                        Some((label.to_string(), None, rows))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let all = s(&["Workspace folder", "ios", "web"]);
    assert_eq!(
        menu_of(cx, "both"),
        [
            ("Open in Terminal".into(), None, all.clone()),
            ("Open in Code".into(), None, all),
            ("Open in Fork".into(), None, s(&["ios", "web"])),
            (
                "Open in Xcode".into(),
                Some("ios/App.xcodeproj".into()),
                vec![]
            ),
        ]
    );
    // "Add repo" lists only the repos the workspace lacks, and hides when there are none.
    let add_repo = |cx: &mut TestAppContext, ws: &str| {
        shark.update(cx, |s, cx| {
            let ws = s.ws(ws).unwrap().clone();
            (ws_menu(s, &ws, cx).into_iter()).find_map(|e| match e {
                Entry::Sub { label, entries, .. } if label == "Add repo" => Some(
                    (entries.iter())
                        .filter_map(|r| match r {
                            Entry::Item { label, .. } => Some(label.to_string()),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
        })
    };
    assert_eq!(add_repo(cx, "web-only"), Some(s(&["ios"])));
    assert_eq!(add_repo(cx, "both"), None);
    let web_only = menu_of(cx, "web-only");
    assert!(
        web_only.iter().all(|e| e.0 != "Open in Xcode"),
        "no project, no Xcode"
    );
    assert!(
        web_only.contains(&("Open in Fork".into(), Some("web".into()), vec![])),
        "one repo opens directly"
    );

    // cmd-k on the open workspace.
    run(cx, "open in terminal");
    assert_eq!(last(), Some(("Terminal".into(), ws_dir.clone())));
    run(cx, "open in xcode");
    assert_eq!(
        last(),
        Some(("Xcode".into(), ws_dir.join("ios/App.xcodeproj")))
    );
    run(cx, "open in fork");
    assert_eq!(last().unwrap().0, "Xcode", "Fork asks first");
    cx.update_window(window, |_, w, cx| w.input("web", cx))
        .unwrap();
    press(cx, &["enter"]);
    assert_eq!(last(), Some(("Fork".into(), ws_dir.join("web"))));

    // A workspace without a project has no Xcode row.
    on(cx, &|s, w, cx| {
        let ix = s
            .state
            .workspaces
            .iter()
            .position(|w| w.name == "web-only")
            .unwrap();
        s.open_workspace(ix, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("web-only")
    })
    .await;
    let before = OPENED.lock().unwrap().len();
    run(cx, "open in xcode");
    assert_eq!(
        OPENED.lock().unwrap().len(),
        before,
        "nothing matched, nothing opened"
    );
    press(cx, &["escape"]);
    run(cx, "open in fork");
    assert_eq!(
        last(),
        Some(("Fork".into(), home.join("shark/workspaces/web-only/web")))
    );
    fs::remove_dir_all(&home).ok();
}

/// A changed file's right-click menu, in the tree and on the open file's header.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn file_menu_labels(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
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
        s.create_workspace(
            "fm".into(),
            git::on_branch(&repos, "fm"),
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
    fs::write(home.join("shark/workspaces/fm/api/a.md"), "x\n").unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    let labels = |entries: Vec<Entry>| {
        (entries.into_iter())
            .map(|e| match e {
                Entry::Item { label, .. } | Entry::Sub { label, .. } => label.to_string(),
                Entry::Sep => "-".into(),
            })
            .collect::<Vec<_>>()
    };
    let file = [
        "Comment",
        "Viewed",
        "-",
        "Reveal in Finder",
        "Open in Code",
        "Copy path",
        "Copy relative path",
    ];
    let menu = shark.update(cx, |s, cx| labels(s.file_menu("api/a.md", cx)));
    assert_eq!(menu[..2], ["Open in editor", "Edit working copy"]);
    assert_eq!(menu[2..], file);
    on(cx, &|s, w, cx| s.open_path("api/a.md".into(), w, cx));
    let menu = shark.update(cx, |s, cx| labels(s.editor_menu(cx)));
    assert_eq!(menu[0], "Edit working copy", "already open");
    assert_eq!(menu[1..], [&file[..], &["-", "Close file"]].concat()[..]);
    fs::remove_dir_all(&home).ok();
}

/// Cmd+P opens the highlighted workspace file, including unchanged and untracked files.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn file_picker_opens_selected_file(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    for repo in &repos {
        fs::write(
            repo.path.join("shared.txt"),
            format!("{} contents\n", repo.name),
        )
        .unwrap();
        git::git(&repo.path, &["add", "."]).unwrap();
        git::git(
            &repo.path,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "file",
            ],
        )
        .unwrap();
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
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "picker".into(),
            git::on_branch(&repos, "picker"),
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
    let dir = home.join("shark/workspaces/picker");
    fs::write(dir.join("api/untracked.txt"), "untracked contents\n").unwrap();
    fs::write(dir.join("api/file with spaces.txt"), "spaced contents\n").unwrap();
    fs::write(dir.join("api/binary.bin"), [0xff, 0xfe]).unwrap();
    let input = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        cx.run_until_parked();
    };
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find(IndexPath::new(2)).is_some()
    })
    .await;
    input(cx, "web/shared.txt");
    press(cx, &["enter"]);
    let editor = cx.update(|cx| {
        shark
            .read(cx)
            .editor
            .clone()
            .expect("Cmd+P then Enter opens the selected file")
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        editor.read(cx).input.read(cx).value().as_ref() == "web contents\n"
    })
    .await;
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        assert_eq!(shark.read(cx).tab, Tab::File);
        assert_eq!(editor.read(cx).path, "web/shared.txt");
        assert!(editor.read(cx).is_focused(w, cx));
    })
    .unwrap();

    // Both navigation and clicking use the displayed, filtered row's full repo path.
    for (query, path, contents, click) in [
        (
            "untracked.txt",
            "api/untracked.txt",
            "untracked contents\n",
            true,
        ),
        ("shared.txt", "web/shared.txt", "web contents\n", false),
        (
            "file with spaces",
            "api/file with spaces.txt",
            "spaced contents\n",
            false,
        ),
    ] {
        press(cx, &["cmd-p"]);
        cx.wait_for(window, Duration::from_secs(10), |w, _| {
            w.try_find(IndexPath::new(0)).is_some()
        })
        .await;
        input(cx, query);
        if click {
            cx.update_window(window, |_, w, cx| w.click(IndexPath::new(0), cx))
                .unwrap();
            cx.run_until_parked();
        } else {
            press(cx, &["down", "up", "down", "enter"]);
        }
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).editor.as_ref().is_some_and(|e| {
                e.read(cx).path == path && e.read(cx).input.read(cx).value().as_ref() == contents
            })
        })
        .await;
        cx.update_window(window, |_, w, cx| {
            assert!(!w.has_active_dialog(cx));
            assert!(
                shark
                    .read(cx)
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .is_focused(w, cx)
            );
        })
        .unwrap();
    }
    let kept = cx.update(|cx| shark.read(cx).editor.clone().unwrap());
    // No matches cannot open a stale row, and cancelling preserves the current editor.
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find(IndexPath::new(0)).is_some()
    })
    .await;
    input(cx, "no-such-file-987654321");
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx));
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&kept));
    })
    .unwrap();
    press(cx, &["escape", "escape"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&kept));
    })
    .unwrap();

    // Unsupported bytes leave the editor intact.
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find(IndexPath::new(0)).is_some()
    })
    .await;
    input(cx, "binary.bin");
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(!w.has_active_dialog(cx));
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&kept));
    })
    .unwrap();

    // Selecting another file must preserve the existing discard/cancel protection.
    input(cx, "unsaved ");
    cx.update(|cx| assert!(kept.read(cx).dirty));
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find(IndexPath::new(0)).is_some()
    })
    .await;
    input(cx, "file with spaces");
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(
            !w.has_active_dialog(cx),
            "reopening the same file needs no discard"
        );
        assert_eq!(shark.read(cx).editor.as_ref(), Some(&kept));
        assert!(kept.read(cx).dirty);
        assert!(kept.read(cx).input.read(cx).value().starts_with("unsaved "));
    })
    .unwrap();
    for discard in [false, true] {
        press(cx, &["cmd-p"]);
        cx.wait_for(window, Duration::from_secs(10), |w, _| {
            w.try_find(IndexPath::new(0)).is_some()
        })
        .await;
        input(cx, "web/shared.txt");
        press(cx, &["enter"]);
        cx.update_window(window, |_, w, cx| {
            assert!(
                w.has_active_dialog(cx),
                "unsaved edits require confirmation"
            );
            assert_eq!(shark.read(cx).editor.as_ref(), Some(&kept));
            assert!(kept.read(cx).dirty);
        })
        .unwrap();
        press(cx, &[if discard { "enter" } else { "escape" }]);
        cx.update(|cx| {
            let current = shark.read(cx).editor.as_ref().unwrap();
            if discard {
                assert_eq!(current.read(cx).path, "web/shared.txt");
            } else {
                assert_eq!(current, &kept);
                assert!(kept.read(cx).input.read(cx).value().starts_with("unsaved "));
            }
        });
    }
    assert_eq!(
        fs::read_to_string(dir.join("api/file with spaces.txt")).unwrap(),
        "spaced contents\n",
        "discard never saves the edit"
    );
}

/// Home shows while nothing is open; cmd-0 toggles it over a workspace, esc and opening a
/// workspace leave it, the arrows and enter open a row, and a relaunch comes back to it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_navigation(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            last_repos: vec!["api".into()],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let press = press_in(window);
    let shown = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).home_shown());
    let active = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).active.clone());
    assert!(shown(cx), "nothing open: Home");
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert!(
            s.home.prompt.read(cx).focus_handle(cx).is_focused(w),
            "the prompt has focus"
        );
    })
    .unwrap();
    // tab switches the prompt to Ask Shark and back.
    press(cx, &["tab"]);
    cx.update(|cx| assert!(shark.read(cx).home.asking));
    press(cx, &["tab"]);
    cx.update(|cx| assert!(!shark.read(cx).home.asking));
    // enter opens the New workspace dialog with the text as the first message.
    cx.update_window(window, |_, w, cx| w.input("add a login page", cx))
        .unwrap();
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx), "enter opens New workspace");
        w.close_dialog(cx);
    })
    .unwrap();

    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    for name in ["one", "two"] {
        on(cx, &|s, w, cx| {
            s.create_workspace(
                name.into(),
                git::on_branch(&repos, name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    assert!(!shown(cx), "opening a workspace leaves Home");
    press(cx, &["cmd-0"]);
    assert!(shown(cx));
    assert_eq!(State::load().last_open, Some(("two".into(), "home".into())));
    press(cx, &["cmd-0"]);
    assert!(!shown(cx), "cmd-0 toggles back");
    assert_eq!(active(cx).as_deref(), Some("two"));
    press(cx, &["cmd-0"]);
    press(cx, &["escape"]);
    assert!(!shown(cx), "esc goes back to the workspace");

    press(cx, &["cmd-0"]);
    cx.update_window(window, |_, w, cx| {
        assert!(
            shark.read(cx).home.focus.is_focused(w),
            "the list has focus"
        )
    })
    .unwrap();
    let sel = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).home.sel.clone());
    assert_eq!(
        sel(cx).as_deref(),
        Some("two"),
        "the cursor starts on the workspace just left"
    );
    press(cx, &["up"]);
    assert_eq!(sel(cx).as_deref(), Some("one"));
    // The palette gives focus back to what had it when it opened, here a closed select's
    // popover that is gone by then; keys land on the list instead.
    let gone = cx.update(|cx| cx.focus_handle());
    on(cx, &|s, w, cx| {
        gone.focus(w, cx);
        s.toggle_palette(w, cx);
    });
    cx.update_window(window, |_, w, cx| assert!(w.has_active_dialog(cx)))
        .unwrap();
    press(cx, &["escape"]);
    cx.executor().advance_clock(Duration::from_secs(1)); // the dialog's deferred restore
    cx.run_until_parked();
    press(cx, &["down"]);
    assert_eq!(sel(cx).as_deref(), Some("two"), "the list has the keys");
    press(cx, &["up"]);
    press(cx, &["enter"]);
    assert_eq!(active(cx).as_deref(), Some("one"));
    assert!(!shown(cx));

    // A relaunch restores Home over the workspace it was shown over.
    press(cx, &["cmd-0"]);
    let (window2, shark2) = open(cx, WindowOptions::default());
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark2.read(cx);
        assert!(s.home.on && s.active.as_deref() == Some("one"));
    });
    cx.update_window(window2, |_, w, _| w.remove_window())
        .unwrap();
    fs::remove_dir_all(&home).ok();
}

/// Points `SHARK_CLAUDE` at another script until dropped, then back. Take it after UI_LOCK,
/// so it drops (and restores) first.
struct ClaudeGuard(Option<std::ffi::OsString>);

impl ClaudeGuard {
    fn set(path: &std::path::Path) -> Self {
        let old = std::env::var_os("SHARK_CLAUDE");
        // SAFETY: tests that start claude hold UI_LOCK.
        unsafe { std::env::set_var("SHARK_CLAUDE", path) };
        ClaudeGuard(old)
    }
}

impl Drop for ClaudeGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`; the lock is still held.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("SHARK_CLAUDE", v),
                None => std::env::remove_var("SHARK_CLAUDE"),
            }
        }
    }
}

/// A claude that never answers: a reply from a real thread would trip the deterministic
/// scheduler. It writes its arguments next to itself.
fn quiet_claude() -> PathBuf {
    let quiet = crate::chat::ui_tests::fake_claude().with_file_name("quiet-claude");
    fs::write(
        &quiet,
        "#!/bin/sh\necho \"$*\" > \"$0.args\"\nexec cat >/dev/null\n",
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&quiet)
        .status()
        .unwrap();
    quiet
}

/// Ask Shark: tab, a question, enter; the answer streams in as markdown, and esc stops one
/// still being written. claude here is a script that never answers: a reply from a real
/// thread would trip the deterministic scheduler, so the test plays it in (the process round
/// trip is `chat::ui_tests::ask_process`).
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_ask_shark(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.run_until_parked();
    let press = press_in(window);
    press(cx, &["tab"]);
    cx.update_window(window, |_, w, cx| w.input("how do I open PRs?", cx))
        .unwrap();
    let _ = fs::remove_file(quiet.with_extension("args"));
    press(cx, &["enter"]);
    // The script writes them from its own process: nothing to wait on but the file.
    let args = (0..100)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(50));
            fs::read_to_string(quiet.with_extension("args")).ok()
        })
        .expect("claude started");
    assert!(
        args.contains("--model haiku") && args.contains("# Shortcuts"),
        "{args}"
    );
    assert!(
        args.contains("cmd-n: New workspace…"),
        "the live keymap: {args}"
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, _, cx| {
        let ask = s.home.ask.as_ref().unwrap();
        assert_eq!(ask.question, "how do I open PRs?");
        assert!(!ask.done && ask.agent.is_some(), "asking");
        assert_eq!(
            s.home.prompt.read(cx).value().as_ref(),
            "",
            "the box is cleared"
        );
        s.apply_ask(crate::agent::Ev::TextDelta("Forty".into()), cx);
        s.apply_ask(crate::agent::Ev::Text("Forty-two".into()), cx);
        s.apply_ask(
            crate::agent::Ev::Result {
                is_error: false,
                text: String::new(),
                cost: 0.,
                secs: 0,
            },
            cx,
        );
        let ask = s.home.ask.as_ref().unwrap();
        assert_eq!(
            (ask.text.as_str(), &ask.error, ask.done),
            ("Forty-two", &None, true)
        );
    });

    on(cx, &|s, w, cx| {
        s.ask("again?".into(), w, cx);
        assert!(s.home.ask.as_ref().is_some_and(|a| !a.done));
        s.home_escape(w, cx);
        let ask = s.home.ask.as_ref().unwrap();
        assert!(ask.done && ask.agent.is_none(), "esc stops it");
    });
    assert!(
        cx.update(|cx| shark.read(cx).home_shown()),
        "and stays on Home"
    );

    // claude's error streams in as the answer too; it shows once, and a login one says how.
    on(cx, &|s, w, cx| {
        s.ask("again?".into(), w, cx);
        let msg = "Invalid API key · Please run /login";
        s.apply_ask(crate::agent::Ev::Text(msg.into()), cx);
        s.apply_ask(
            crate::agent::Ev::Result {
                is_error: true,
                text: msg.into(),
                cost: 0.,
                secs: 0,
            },
            cx,
        );
        let ask = s.home.ask.as_ref().unwrap();
        assert_eq!(ask.text, "", "not also as the answer");
        assert_eq!(
            ask.error.as_deref(),
            Some("claude isn't logged in: run `claude` in a terminal and log in")
        );
    });
}

/// The prompt's text survives a cancelled New workspace dialog and clears once the workspace
/// is made from it. Chat keys on Home act on the chat and show it (cmd-w, with no chat shown,
/// does nothing), and a turn's end re-reads what the workspace waits on.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_prompt_and_chat_keys(cx: &mut TestAppContext) {
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos(&home, &["api"]),
            last_repos: vec!["api".into()],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    cx.run_until_parked();
    let press = press_in(window);
    let text = |cx: &mut TestAppContext| {
        cx.update(|cx| shark.read(cx).home.prompt.read(cx).value().to_string())
    };
    cx.update_window(window, |_, w, cx| w.input("add a login page", cx))
        .unwrap();
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| w.close_dialog(cx))
        .unwrap();
    assert_eq!(text(cx), "add a login page", "cancelled: kept");

    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "one".into(),
            git::on_branch(&repos, "one"),
            vec![],
            Some("add a login page".into()),
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    assert_eq!(text(cx), "", "made from it: cleared");

    let chats = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            (s.home_shown(), s.active_ws().unwrap().chats.len())
        })
    };
    press(cx, &["cmd-0"]);
    assert_eq!(chats(cx), (true, 1));
    press(cx, &["cmd-w"]);
    assert_eq!(chats(cx), (true, 1), "cmd-w on Home: nothing to close");
    press(cx, &["cmd-t"]);
    assert_eq!(chats(cx), (false, 2), "cmd-t shows the new chat");

    let chat = cx.update(|cx| shark.read(cx).active_chat().cloned().unwrap());
    let id = cx.update(|cx| chat.read(cx).meta.id.clone());
    let path = crate::agent::transcript_path(&id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let said = |t: &str| {
        format!(
            "{}\n{}\n",
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}}),
            serde_json::json!({"type": "result", "is_error": false, "result": t}),
        )
    };
    let waiting = |cx: &mut TestAppContext, said: String| {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut f, said.as_bytes()).unwrap();
        cx.update(|cx| chat.update(cx, |_, cx| cx.emit(crate::chat::ChatEvent::TurnDone)));
        cx.run_until_parked();
        cx.update(|cx| shark.read(cx).home.waiting.get("one").copied())
    };
    assert_eq!(
        waiting(cx, said("Shall I push it?")),
        Some(crate::agent::QUESTION)
    );
    assert_eq!(
        waiting(cx, said("Pushed.")),
        None,
        "re-read once it changed"
    );
    fs::remove_dir_all(&home).ok();
}

/// Get started ticks itself off and hides once all done, or when dismissed.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_checklist(cx: &mut TestAppContext) {
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
    cx.run_until_parked();
    let done = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let s = shark.read(cx);
            let items = s.checklist(cx).map(|(_, d)| d);
            (items, s.checklist_open(&s.checklist(cx)))
        })
    };
    assert_eq!(done(cx), ([false, true, false, false, false], true));
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "ws".into(),
            git::on_branch(&repos, "ws"),
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
    let plan = crate::plan::tests_store_path("ws");
    fs::create_dir_all(plan.parent().unwrap()).unwrap();
    fs::write(
        &plan,
        r##"{"versions":[{"id":"t1","text":"# Plan"}],"approved":0}"##,
    )
    .unwrap();
    on(cx, &|s, w, cx| {
        s.deps = vec![crate::deps::Dep {
            name: "git",
            ok: true,
            fix: "",
            detail: String::new(),
        }];
        s.state.opened_pr = true;
        s.enter_home(w, cx);
    });
    assert_eq!(done(cx), ([true; 5], false), "all done: hidden");

    on(cx, &|s, _, _| s.state.opened_pr = false);
    assert!(done(cx).1);
    on(cx, &|s, _, _| s.state.home_checklist_dismissed = true);
    assert!(!done(cx).1, "dismissed");
    fs::remove_dir_all(&home).ok();
}

/// Changes' toolbar and the Files panel: counts, Split / Unified, the filter and its Enter,
/// viewed boxes, and a Files width that survives Chat ↔ Changes and a restart.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn changes_toolbar_and_files_panel(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let options = || WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(1440.), px(900.)),
        ))),
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        options(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "tb".into(),
            git::on_branch(&repos, "tb"),
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
    let api = home.join("shark/workspaces/tb/api");
    fs::create_dir_all(api.join("src")).unwrap();
    fs::write(api.join("a.md"), "x\n").unwrap();
    fs::write(api.join("src/b.rs"), "1\n2\n").unwrap();
    fs::write(api.join("src/chat.rs"), "1\n2\n3\n").unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    let base = cx.update(|cx| shark.read(cx).state.workspaces[0].repos[0].base.clone());
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.changes_stats(), (base.clone(), 6, 0));
        assert_eq!((s.viewed_now.len(), s.diff_files.len()), (0, 3));
        assert!(!s.tree_shown(), "Files is closed until asked for");
        assert_eq!(s.diff.read(cx).mode(), DiffMode::Split, "split by default");
    });
    on(cx, &|s, w, cx| s.toggle_tree(w, cx));
    cx.update(|cx| assert!(shark.read(cx).tree_shown(), "Files N opens it"));

    // `s` flips Split / Unified, and the choice persists.
    press(cx, &["s"]);
    cx.update(|cx| assert_eq!(shark.read(cx).diff.read(cx).mode(), DiffMode::Unified));
    assert!(State::load().unified);
    press(cx, &["s"]);
    assert!(!State::load().unified);

    // The filter narrows the tree; its rows keep their file; Enter shows it in the diff.
    let input = cx.update(|cx| shark.read(cx).tree_filter.read(cx).focus_handle(cx));
    on(cx, &|_, w, cx| input.focus(w, cx));
    cx.update_window(window, |_, w, cx| w.input("CHAT", cx))
        .unwrap();
    cx.run_until_parked();
    let chat_ix = cx.update(|cx| {
        let s = shark.read(cx);
        let files: Vec<usize> = s.tree_rows.iter().filter_map(|r| r.file).collect();
        assert_eq!(files.len(), 1, "only chat.rs matches");
        assert_eq!(s.diff_files[files[0]].path().as_str(), "api/src/chat.rs");
        files[0]
    });
    press(cx, &["enter"]);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert_eq!(s.diff_file, chat_ix, "Enter jumps to the match");
        assert!(s.diff.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();

    // A viewed box ticks the file, and the counts follow.
    on(cx, &|s, _, cx| s.toggle_viewed(chat_ix, cx));
    cx.update(|cx| assert_eq!(shark.read(cx).viewed_now.len(), 1));

    // The Files edge: a dragged width persists and survives Chat ↔ Changes.
    let resize = cx.update(|cx| shark.read(cx).tree_resize.clone());
    on(cx, &|_, w, cx| {
        resize.update(cx, |r, cx| r.resize_panel(1, px(333.), w, cx))
    });
    assert_eq!(State::load().tree_width, Some(333.));
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    cx.update(|cx| assert!(shark.read(cx).tree_shown(), "Files stays beside the chat"));
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.diff_file, chat_ix, "file selection kept");
        assert_eq!(s.tree_query, "chat", "filter kept");
        assert_eq!(s.tree_resize.read(cx).sizes()[1], px(333.));
    });

    // Committed branch changes disappear in the HEAD comparison; local changes remain.
    git::git(&api, &["add", "."]).unwrap();
    git::git(
        &api,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "branch changes",
        ],
    )
    .unwrap();
    fs::write(api.join("a.md"), "staged\n").unwrap();
    git::git(&api, &["add", "a.md"]).unwrap();
    fs::write(api.join("a.md"), "unstaged\n").unwrap();
    fs::write(api.join("new.txt"), "untracked\n").unwrap();
    fs::remove_file(api.join("src/b.rs")).unwrap();
    let index = git::git(&api, &["ls-files", "--stage"]).unwrap();
    let click_scope = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("uncommitted-only", cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    click_scope(cx);
    assert!(State::load().uncommitted_only);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.changes_stats(), ("HEAD".into(), 2, 3));
        let paths: Vec<_> = s.diff_files.iter().map(|f| f.path().as_str()).collect();
        assert_eq!(paths, ["api/src/b.rs", "api/a.md", "api/new.txt"]);
        let patch =
            git::combined_diff(s.active_ws().unwrap(), api.parent().unwrap(), true).unwrap();
        assert!(
            patch.contains("-x\n+unstaged\n"),
            "HEAD is the original side: {patch}"
        );
        assert!(!patch.contains("+staged\n"));
    });
    assert_eq!(
        git::git(&api, &["ls-files", "--stage"]).unwrap(),
        index,
        "real index untouched"
    );

    // Original-side review comments quote HEAD, where a.md was first committed.
    on(cx, &|s, w, cx| {
        let line = crate::diff_view::DiffLineRange::new(
            "api/a.md",
            crate::diff_view::DiffSide::Original,
            1,
            1,
        );
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
        s.comment_dialog(w, cx);
        let draft = &s.review.composer.as_ref().unwrap().draft;
        assert_eq!(draft.snapshot, "x");
        s.cancel_composer(w, cx);
    });

    // Switching scope cannot silently discard an inline edit or reuse its old base.
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/a.md".into()), w, cx);
        s.editing.as_mut().unwrap().dirty = true;
        s.toggle_uncommitted(w, cx);
        assert!(w.has_active_dialog(cx));
        assert!(s.state.uncommitted_only && s.editing.is_some());
    });
    press(cx, &["escape"]);
    on(cx, &|s, _, cx| {
        s.editing.as_mut().unwrap().dirty = false;
        s.end_edit(cx);
    });

    // A staged-only change is included too; committing everything leaves the toggle usable.
    fs::write(api.join("a.md"), "staged\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.update(|cx| assert_eq!(shark.read(cx).diff_files.len(), 3));
    git::git(&api, &["add", "."]).unwrap();
    git::git(
        &api,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "local changes",
        ],
    )
    .unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.update(|cx| assert!(shark.read(cx).diff_files.is_empty()));
    click_scope(cx);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(!s.state.uncommitted_only);
        assert_eq!(
            s.diff_files.len(),
            3,
            "branch changes restored from an empty view"
        );
        assert_eq!(s.changes_stats().0, base);
    });
    click_scope(cx);

    // The scope stays visible and usable beside Chat, even when no files are uncommitted.
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    for uncommitted_only in [false, true] {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("files-change-scope", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| {
            let s = shark.read(cx);
            assert!(s.tab == Tab::Chat && s.tree_shown());
            assert!(s.tree_focus.is_focused(w));
            assert_eq!(s.state.uncommitted_only, uncommitted_only);
            assert_eq!(s.diff_files.len(), if uncommitted_only { 0 } else { 3 });
        })
        .unwrap();
        assert_eq!(State::load().uncommitted_only, uncommitted_only);
    }

    // After a restart Files is closed, and opens at the saved width; the saved modes stay.
    cx.update_window(window, |_, w, _| w.remove_window())
        .unwrap();
    let (window2, shark2) = open(cx, options());
    cx.run_until_parked();
    cx.update_window(window2, |_, w, cx| {
        assert!(!shark2.read(cx).tree_shown());
        shark2.update(cx, |s, cx| s.toggle_tree(w, cx));
        w.render_frame(cx)
    })
    .unwrap();
    cx.update(|cx| {
        let s = shark2.read(cx);
        assert!(s.tree_shown());
        assert_eq!(s.tree_resize.read(cx).sizes()[1], px(333.));
        assert_eq!(s.diff.read(cx).mode(), DiffMode::Split);
        assert!(s.state.uncommitted_only);
        assert!(s.diff_files.is_empty());
    });
    cx.update_window(window2, |_, w, _| w.remove_window())
        .unwrap();
    fs::remove_dir_all(&home).ok();
}

/// The right side beside every tab: a rail whose Diff button replaces the old Changes tab, and
/// whose toggle expands Workspace tools in place; the sidebar toggle clears the traffic lights, and the
/// expanded sidebar's dragged width survives a restart.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn side_rails_and_toggles(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let options = || WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(1440.), px(900.)),
        ))),
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        options(),
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "rails".into(),
            git::on_branch(&repos, "rails"),
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
    fs::write(home.join("shark/workspaces/rails/api/a.md"), "x\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    let chat_tab = cx.update(|cx| {
        format!(
            "chat-tab-{}",
            shark.read(cx).state.workspaces[0].chats[0].id
        )
    });
    let bounds = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.try_find(id).map(|e| e.bounds())
        })
        .unwrap()
    };
    let click = |cx: &mut TestAppContext, id: &str| {
        let id = SharedString::from(id.to_string());
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click(id, cx)
        })
        .unwrap();
        cx.run_until_parked();
    };

    // A toast at the top right stays clear of the rail.
    let toasts = cx.update_window(window, |_, w, cx| w.notifications(cx).len());
    assert!(
        toasts.unwrap() > 0,
        "the test shows a toast over where the rail is"
    );
    // On the chat: the rail and the titlebar both offer Diff.
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));
    assert!(bounds(cx, "right-rail").is_some(), "the rail by the chat");
    assert!(bounds(cx, "Changes 1").is_none(), "no Changes tab");
    assert!(bounds(cx, "hide-tree").is_none());

    // Both pane controls share the traffic-light row.
    let (left, right) = (
        bounds(cx, "toggle-sidebar").unwrap(),
        bounds(cx, "show-tree").unwrap(),
    );
    assert_eq!(left.size, size(px(32.), px(28.)));
    assert_eq!(right.size, left.size);
    assert_eq!(left.top(), px(8.));
    assert_eq!(left.left(), px(80.));
    assert_eq!(
        right.top(),
        left.top(),
        "pane toggles share the titlebar row"
    );
    assert_eq!(px(1440.) - right.right(), px(20.));
    let diff_button = bounds(cx, "rail-diff").unwrap();
    assert_eq!(right.center().x, diff_button.center().x);
    assert_eq!(
        diff_button.top(),
        bounds(cx, "workspace-titlebar").unwrap().bottom() + px(16.)
    );

    // Diff shows the diff; the chat tab brings the chat back.
    click(cx, "rail-diff");
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Diff));
    assert!(bounds(cx, "rail-diff").is_some(), "and by the diff");
    click(cx, &chat_tab);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Chat));

    // The rail's toggle expands Workspace tools, its hide button in the same spot; and back.
    click(cx, "show-tree");
    cx.update(|cx| assert!(shark.read(cx).tree_shown()));
    assert!(bounds(cx, "right-rail").is_none());
    assert_eq!(bounds(cx, "hide-tree"), Some(right));
    assert_eq!(bounds(cx, "toggle-sidebar"), Some(left));
    click(cx, "hide-tree");
    cx.update(|cx| assert!(!shark.read(cx).tree_shown()));
    assert_eq!(bounds(cx, "show-tree"), Some(right));

    // cmd-alt-b toggles Workspace tools, cmd-shift-d shows the diff.
    let press = press_in(window);
    press(cx, &["cmd-alt-b"]);
    cx.update(|cx| assert!(shark.read(cx).tree_shown()));
    press(cx, &["cmd-alt-b"]);
    cx.update(|cx| assert!(!shark.read(cx).tree_shown()));
    press(cx, &["cmd-shift-d"]);
    cx.update(|cx| assert_eq!(shark.read(cx).tab, Tab::Diff));

    // The sidebar control stays put when it moves from the sidebar into the main titlebar.
    click(cx, "toggle-sidebar");
    cx.update(|cx| assert!(shark.read(cx).rail));
    let collapsed = bounds(cx, "toggle-sidebar").unwrap();
    assert_eq!(collapsed.size, left.size);
    assert_eq!(collapsed.top(), left.top());
    assert_eq!(collapsed, left);
    assert_eq!(bounds(cx, "sidebar").unwrap().size.width, px(72.));
    assert!(collapsed.left() >= bounds(cx, "sidebar").unwrap().right());
    assert_eq!(bounds(cx, "show-tree"), Some(right));
    click(cx, "show-tree");
    assert_eq!(bounds(cx, "hide-tree"), Some(right));
    assert_eq!(bounds(cx, "toggle-sidebar"), Some(collapsed));
    click(cx, "hide-tree");
    assert_eq!(bounds(cx, "show-tree"), Some(right));
    click(cx, "toggle-sidebar");
    cx.update(|cx| assert!(!shark.read(cx).rail));
    assert_eq!(bounds(cx, "toggle-sidebar"), Some(left));

    // The expanded sidebar's edge: a dragged width persists, and survives a restart.
    assert_eq!(bounds(cx, "sidebar").unwrap().size.width, px(244.));
    let resize = cx.update(|cx| shark.read(cx).sidebar_resize.clone());
    on(cx, &|_, w, cx| {
        resize.update(cx, |r, cx| r.resize_panel(0, px(300.), w, cx))
    });
    assert_eq!(State::load().sidebar_width, Some(300.));
    assert_eq!(bounds(cx, "toggle-sidebar"), Some(left));
    assert_eq!(bounds(cx, "show-tree"), Some(right));
    cx.update_window(window, |_, w, _| w.remove_window())
        .unwrap();
    let (window2, _) = open(cx, options());
    cx.run_until_parked();
    cx.update_window(window2, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(w.find("sidebar").bounds().size.width, px(300.));
        w.remove_window()
    })
    .unwrap();
    fs::remove_dir_all(&home).ok();
}

/// The split ratio persists and is shared; a file's working copy is edited in the diff: the
/// comparison follows the unsaved buffer, line keys and paste edit it, it survives Chat ↔
/// Changes, the view stays put, and cmd-s writes it.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn split_ratio_and_inline_edit(cx: &mut TestAppContext) {
    use crate::diff_view::{DiffEvent, DiffLinePosition, DiffLineRange, DiffSide};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let lines = |f: &dyn Fn(usize) -> String| (1..=300).map(f).collect::<String>();
    fs::write(
        repos[0].path.join("f.txt"),
        lines(&|i| format!("line {i}\n")),
    )
    .unwrap();
    git::git(&repos[0].path, &["add", "."]).unwrap();
    let commit = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "f",
    ];
    git::git(&repos[0].path, &commit).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "ed".into(),
            git::on_branch(&repos, "ed"),
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
    let file = home.join("shark/workspaces/ed/api/f.txt");
    // A change every ten lines: a diff many screens tall.
    let changed = |i: usize| match i % 10 {
        0 => format!("changed {i}\n"),
        _ => format!("line {i}\n"),
    };
    fs::write(&file, lines(&changed)).unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.diff.update(cx, |d, cx| d.set_mode(DiffMode::Split, cx));
    });

    // A finished divider drag is saved, and a new window starts with it.
    on(cx, &|s, _, cx| {
        s.diff.update(cx, |d, cx| {
            d.set_split_ratio(0.4, cx);
            cx.emit(DiffEvent::SplitRatioChanged(0.4));
        })
    });
    assert_eq!(State::load().split_ratio, Some(0.4));
    let (_, again) = open(cx, WindowOptions::default());
    cx.update(|cx| assert_eq!(again.read(cx).diff.read(cx).split_ratio(), 0.4));

    // Edit from the selected working line, deep in the file.
    on(cx, &|s, _, cx| {
        let line = DiffLineRange::new("api/f.txt", DiffSide::Modified, 250, 250);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
    });
    let draw = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
    };
    // A comment on line 250 in Split: the original column hints at it across the gap
    // (Unified has no gap).
    on(cx, &|s, w, cx| s.comment_dialog(w, cx));
    let hints = |cx: &mut TestAppContext| {
        draw(cx);
        cx.update_window(window, |_, w, _| w.find_all("annotation-hint").len())
            .unwrap()
    };
    assert_eq!(hints(cx), 1);
    on(cx, &|s, _, cx| {
        s.diff.update(cx, |d, cx| d.set_mode(DiffMode::Unified, cx))
    });
    assert_eq!(hints(cx), 0);
    on(cx, &|s, w, cx| {
        s.cancel_composer(w, cx);
        s.diff.update(cx, |d, cx| d.set_mode(DiffMode::Split, cx));
        let line = DiffLineRange::new("api/f.txt", DiffSide::Modified, 250, 250);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
    });
    draw(cx);
    press(cx, &["i"]);
    draw(cx);
    let top =
        |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).diff.read(cx).scroll_top().item_ix);
    let before = top(cx);
    assert!(
        before > 3,
        "editing line 250 scrolled to it, not row {before}"
    );
    let buf = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let e = shark.read(cx).editing.as_ref().unwrap();
            (e.buf.lines.clone(), e.buf.line)
        })
    };
    assert_eq!(buf(cx).1, 249);
    // ↓ then typing: the comparison has the unsaved line before any save.
    press(cx, &["down"]);
    cx.update_window(window, |_, w, cx| w.input("X", cx))
        .unwrap();
    draw(cx);
    let (lines_now, at) = buf(cx);
    assert_eq!((lines_now[250].as_str(), at), ("line 251X", 250));
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        let e = s.editing.as_ref().unwrap();
        assert!(e.focused(w, cx), "the line input keeps focus");
        assert!(e.patch.contains("+line 251X\n"), "{}", e.patch);
        assert_eq!(s.diff.read(cx).files()[0].additions(), 31);
        assert_eq!(s.working_heading("api/f.txt"), "Working copy · unsaved");
        assert!(
            s.edit_status()
                .unwrap()
                .starts_with("Editing the working copy")
        );
    })
    .unwrap();
    assert!(
        top(cx).abs_diff(before) <= 2,
        "the view stays near row {before}"
    );

    // enter splits, backspace at column 0 joins, up moves, paste inserts lines.
    press(cx, &["enter"]);
    assert_eq!(
        buf(cx).0[250..252],
        ["line 251X".to_string(), String::new()]
    );
    press(cx, &["backspace"]);
    assert_eq!(
        (buf(cx).0[250].as_str(), buf(cx).0.len()),
        ("line 251X", 300)
    );
    press(cx, &["up"]);
    assert_eq!(buf(cx).1, 249);
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("p1\np2".into())));
    press(cx, &["cmd-v"]);
    let (lines_now, at) = buf(cx);
    // The caret kept its column (9) moving up.
    assert_eq!(
        lines_now[249..251],
        ["changed 2p1".to_string(), "p250".into()]
    );
    assert_eq!(at, 250);

    // Chat and back: the unsaved edits are still there and still compared.
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    cx.update(|cx| {
        let s = shark.read(cx);
        let e = s.editing.as_ref().unwrap();
        assert!(e.dirty && e.buf.lines[250] == "p250");
        assert!(s.diff.read(cx).files()[0].additions() >= 31);
    });

    // cmd-s writes the buffer and the comparison stays.
    on(cx, &|s, w, cx| s.focus_edit(w, cx));
    press(cx, &["cmd-s"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.as_ref().is_some_and(|e| e.saved)
    })
    .await;
    let want = cx.update(|cx| shark.read(cx).editing.as_ref().unwrap().buf.text());
    assert_eq!(fs::read_to_string(&file).unwrap(), want);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.working_heading("api/f.txt"),
            "Working copy · saved just now"
        );
        assert!(s.edit_status().unwrap().starts_with("Saved · "));
    });
    // Escape goes back to reviewing without asking.
    press(cx, &["escape"]);
    cx.update(|cx| assert!(shark.read(cx).editing.is_none()));

    // Back in: the base is the commit's blob, not empty (git's file shows again, with its
    // index header).
    let counts = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let f = &shark.read(cx).diff.read(cx).files()[0];
            (f.additions(), f.deletions())
        })
    };
    let git = counts(cx);
    assert!(git.1 > 0, "{git:?}");
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    draw(cx);
    assert_eq!(counts(cx), git);
    // Line 5 is far from any change, and still gets the input: the buffer is compared in
    // full and the diff folds the rest.
    on(cx, &|s, w, cx| {
        let five = DiffLinePosition::new("api/f.txt", DiffSide::Modified, 5);
        s.edit_click(&five, w, cx)
    });
    draw(cx);
    cx.update_window(window, |_, w, cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        assert!(
            e.buf.line == 4 && e.focused(w, cx),
            "line 5's input is drawn"
        );
    })
    .unwrap();
    // Dirty, then another file with Discard: f.txt shows the disk version, not the
    // dropped buffer.
    fs::write(home.join("shark/workspaces/ed/api/g.txt"), "g\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 2
    })
    .await;
    // A new file reloads the diff; the caret's line comes back into view.
    on(cx, &|s, w, cx| {
        let five = DiffLinePosition::new("api/f.txt", DiffSide::Modified, 5);
        s.edit_click(&five, w, cx)
    });
    draw(cx);
    cx.update_window(window, |_, w, cx| w.input("Z", cx))
        .unwrap();
    draw(cx);
    assert_eq!(counts(cx), (git.0 + 1, git.1 + 1));
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/g.txt".into()), w, cx)
    });
    draw(cx);
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx), "switching files asks first")
    })
    .unwrap();
    press(cx, &["enter"]);
    draw(cx);
    cx.update(|cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        assert_eq!(e.path.as_str(), "api/g.txt");
    });
    assert_eq!(counts(cx), git);
    fs::remove_dir_all(&home).ok();
}

/// The review queue end to end: compose (c), suggest, edit (e) and delete (x) threads, the
/// send sheet (shift-c) sending only what is ticked, agent reports and changed lines judging
/// the threads, resolve / reopen / re-queue, a review queued behind a busy turn and removed
/// from its chat card, orphaned threads, and sharing to GitHub through a fake gh.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_queue(cx: &mut TestAppContext) {
    use crate::diff_view::{DiffLineRange, DiffSide};
    use review::Status;
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
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "rq".into(),
            git::on_branch(&repos, "rq"),
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
    let api = home.join("shark/workspaces/rq/api");
    fs::write(api.join("f"), "a\nb\nc\n").unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    cx.update(|cx| assert_eq!(shark.read(cx).diff_files.len(), 1));
    let press = press_in(window);
    let comments = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            (shark.read(cx).ws_comments().iter())
                .map(|c| (c.id, c.text.clone(), c.status.clone()))
                .collect::<Vec<_>>()
        })
    };
    let typing = |cx: &mut TestAppContext, t: &str| {
        cx.update_window(window, |_, w, cx| w.input(t, cx)).unwrap();
        cx.run_until_parked();
    };

    // c on line 1: the composer; its letters don't reach the diff's keys. cmd-shift-s opens
    // Suggest change on the lines; cmd-enter adds a pending thread with the snapshot.
    press(cx, &["down", "c"]);
    cx.update(|cx| assert!(shark.read(cx).review.composer.is_some()));
    typing(cx, "rename vars");
    press(cx, &["cmd-shift-s"]);
    typing(cx, "!");
    press(cx, &["cmd-enter"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.review.composer.is_none());
        let c = &s.ws_comments()[0];
        assert_eq!((c.id, c.text.as_str()), (1, "rename vars"));
        assert_eq!(
            (c.snapshot.as_str(), c.suggestion.as_deref()),
            ("a", Some("a!"))
        );
        assert_eq!(c.status, Status::Pending);
    });
    // e edits the thread at the cursor; Esc leaves it as it was.
    press(cx, &["e"]);
    cx.update(|cx| {
        let comp = shark.read(cx).review.composer.as_ref().unwrap();
        assert_eq!(comp.input.read(cx).value().as_ref(), "rename vars");
    });
    press(cx, &["escape"]);
    assert_eq!(comments(cx)[0].1, "rename vars");
    // Two more threads, x deletes the one at the cursor (line 2).
    press(cx, &["down", "down", "c"]);
    typing(cx, "third line");
    press(cx, &["cmd-enter", "up", "c"]);
    typing(cx, "drop me");
    press(cx, &["cmd-enter"]);
    assert_eq!(comments(cx).len(), 3);
    press(cx, &["x"]);
    let ids: Vec<u64> = comments(cx).iter().map(|c| c.0).collect();
    assert_eq!(ids, [1, 2], "x deleted the line-2 thread");
    // A new thread there gets a fresh id: 3 was the deleted one's.
    press(cx, &["c"]);
    typing(cx, "fresh");
    press(cx, &["cmd-enter"]);
    assert!(comments(cx).iter().any(|c| c.0 == 4 && c.1 == "fresh"));
    press(cx, &["x"]);

    // shift-c: the sheet; space unticks the first row, cmd-enter sends only the second.
    press(cx, &["shift-c"]);
    cx.update(|cx| {
        let s = shark.read(cx).review.sheet.as_ref().unwrap();
        assert_eq!(s.rows, [(1, true), (2, true)]);
    });
    press(cx, &["space", "cmd-enter"]);
    let chat = cx.update(|cx| shark.read(cx).active_chat().cloned().unwrap());
    let sent = cx.update(|cx| chat.read(cx).sent_texts());
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("\n[c2] api/f:3 · working tree\n```\nc\n```\n> third line\n"),
        "{}",
        sent[0]
    );
    assert!(!sent[0].contains("rename vars"), "unticked stays out");
    assert_eq!(
        comments(cx).iter().map(|c| c.2.clone()).collect::<Vec<_>>(),
        [Status::Pending, Status::Sent { turn: 1 }]
    );
    // The agent reports c2 addressed.
    let said = |t: &str| {
        [
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}, "parent_tool_use_id": null}).to_string(),
            serde_json::json!({"type": "result", "is_error": false, "result": t, "total_cost_usd": 0, "duration_ms": 1}).to_string(),
        ]
    };
    let reply = |cx: &mut TestAppContext, t: &str| {
        let lines = said(t);
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                let lines: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
                crate::chat::ui_tests::play(c, &lines, cx)
            })
        });
        cx.run_until_parked();
    };
    reply(cx, "Renamed it.\n\n[c2] addressed: Renamed the third line.");
    assert_eq!(
        comments(cx)[1].2,
        Status::Addressed {
            turn: 1,
            note: "Renamed the third line.".into()
        }
    );

    // c1 goes too; the agent rewrites line 1 without a report: Outdated.
    press(cx, &["shift-c", "cmd-enter"]);
    assert_eq!(comments(cx)[0].2, Status::Sent { turn: 2 });
    fs::write(api.join("f"), "A\nb\nc\n").unwrap();
    reply(cx, "Done.");
    assert_eq!(comments(cx)[0].2, Status::Outdated);
    // Re-queue puts the outdated one back; r resolves the addressed one; R reopens.
    on(cx, &|s, _, cx| s.requeue_outdated(cx));
    assert_eq!(comments(cx)[0].2, Status::Pending);
    on(cx, &|s, w, cx| s.jump_to_comment(2, w, cx));
    press(cx, &["r"]);
    assert_eq!(comments(cx)[1].2, Status::Resolved);
    on(cx, &|s, _, cx| s.set_status(&[1], Status::Outdated, cx));
    on(cx, &|s, w, cx| s.jump_to_comment(1, w, cx));
    press(cx, &["shift-r"]);
    assert_eq!(comments(cx)[0].2, Status::Pending);
    // r without a thread under the cursor refreshes the diff, as before.
    fs::write(api.join("h"), "new\n").unwrap();
    on(cx, &|s, _, cx| {
        let two = DiffLineRange::new("api/f", DiffSide::Modified, 2, 2);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(two), cx));
    });
    press(cx, &["r"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 2
    })
    .await;
    fs::remove_file(api.join("h")).unwrap();

    // A busy chat queues the review: the tab counts it, the card's Remove puts it back.
    // (The fake claude never echoes, so the reviews sent above still count as unread.)
    let unread = cx.update(|cx| chat.read(cx).review_count());
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| s.send_review(&[1], "", false, w, cx));
    assert_eq!(comments(cx)[0].2, Status::Queued);
    cx.update(|cx| assert_eq!(chat.read(cx).review_count(), unread + 1));
    // Queued, e and x would orphan its card: neither runs.
    on(cx, &|s, w, cx| s.jump_to_comment(1, w, cx));
    press(cx, &["e", "x"]);
    cx.update(|cx| assert!(shark.read(cx).review.composer.is_none()));
    assert_eq!(comments(cx)[0].2, Status::Queued);
    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| c.remove_queued(0, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(comments(cx)[0].2, Status::Pending);
    cx.update(|cx| assert_eq!(chat.read(cx).review_count(), unread));
    // Queued again, it goes out when the turn ends, read in the next turn; claude's echo
    // of it (the same text) marks it read.
    on(cx, &|s, w, cx| s.send_review(&[1], "", false, w, cx));
    reply(cx, "Busy work done.");
    assert_eq!(comments(cx)[0].2, Status::Sent { turn: 3 });
    let echo = cx.update(|cx| chat.read(cx).sent_texts().pop().unwrap());
    let echo = serde_json::json!({"type": "user", "message": {"role": "user", "content": echo}})
        .to_string();
    cx.update(|cx| chat.update(cx, |c, cx| crate::chat::ui_tests::play(c, &[&echo], cx)));
    cx.update(|cx| assert_eq!(chat.read(cx).review_count(), unread));
    reply(cx, "[c1] addressed: Renamed.");
    assert!(matches!(
        comments(cx)[0].2,
        Status::Addressed { turn: 3, .. }
    ));
    // e with new text: pending again, out of the chat; unchanged text keeps the status.
    on(cx, &|s, w, cx| s.jump_to_comment(1, w, cx));
    press(cx, &["e"]);
    typing(cx, "!");
    press(cx, &["cmd-enter"]);
    assert_eq!(comments(cx)[0].2, Status::Pending);
    cx.update(|cx| assert_eq!(shark.read(cx).comment(1).unwrap().chat, None));
    press(cx, &["e", "backspace", "cmd-enter"]);
    assert_eq!(comments(cx)[0].1, "rename vars");
    on(cx, &|s, _, cx| s.set_status(&[1], Status::Outdated, cx));
    press(cx, &["e", "cmd-enter"]);
    assert_eq!(comments(cx)[0].2, Status::Outdated);
    on(cx, &|s, _, cx| s.set_status(&[1], Status::Pending, cx));
    // An event about a comment that isn't waiting in that chat changes nothing.
    cx.update(|cx| {
        chat.update(cx, |_, cx| {
            cx.emit(ChatEvent::ReviewSent {
                ids: vec![1],
                turn: 9,
            })
        })
    });
    cx.run_until_parked();
    assert_eq!(comments(cx)[0].2, Status::Pending);
    // Sent but never read when the process goes (a new conversation): pending again.
    on(cx, &|s, w, cx| s.send_review(&[1], "", false, w, cx));
    assert!(matches!(comments(cx)[0].2, Status::Sent { .. }));
    cx.update(|cx| chat.update(cx, |c, cx| c.new_conversation(cx)));
    cx.run_until_parked();
    assert_eq!(comments(cx)[0].2, Status::Pending);
    // Queued in a chat that is closed: pending again.
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| s.send_review(&[1], "", false, w, cx));
    assert_eq!(comments(cx)[0].2, Status::Queued);
    cx.update(|cx| chat.update(cx, |c, _| c.busy = false));
    on(cx, &|s, w, cx| s.close_chat(w, cx));
    assert_eq!(
        (
            comments(cx)[0].2.clone(),
            cx.update(|cx| shark.read(cx).comment(1).unwrap().chat.clone())
        ),
        (Status::Pending, None)
    );

    // Share to GitHub through a fake gh: argv logged, URL kept; a failure keeps the comment.
    let bin = home.join("gh-bin");
    fs::create_dir_all(&bin).unwrap();
    let (log, fail) = (bin.join("log"), bin.join("fail"));
    let gh = bin.join("gh");
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s|' \"$@\" >> {log}\necho >> {log}\n[ -e {fail} ] && {{ echo 'gh: offline' >&2; exit 1; }}\ncase \"$1\" in api) echo https://github.com/o/api/pull/7#pullrequestreview-5 ;; *) echo https://github.com/o/api/pull/7#issuecomment-9 ;; esac\n",
            log = log.display(),
            fail = fail.display()
        ),
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&gh)
        .status()
        .unwrap();
    let gh_env = GhGuard::set(&gh);
    let github = |cx: &mut TestAppContext, id: u64| {
        cx.update(|cx| shark.read(cx).comment(id).and_then(|c| c.github.clone()))
    };
    on(cx, &|s, w, cx| {
        s.active_sync()
            .unwrap()
            .update(cx, |g, _| g.set_pr("api", 7, false));
        s.open_share(1, w, cx);
        s.post_share(w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).review.share.is_none()
    })
    .await;
    assert_eq!(
        github(cx, 1).as_deref(),
        Some("https://github.com/o/api/pull/7#issuecomment-9")
    );
    let argv = fs::read_to_string(&log).unwrap();
    assert!(
        argv.starts_with(
            "pr|comment|7|--body|**api/f** · working copy · 1\n\n```\nA\n```\n\nrename vars"
        ),
        "{argv}"
    );
    // The sheet with Also publish: the chat gets it, and the PR a Comment review.
    fs::write(&log, "").unwrap();
    on(cx, &|s, w, cx| {
        s.edit_comments(cx, |l| l[0].github = None);
        s.open_sheet(w, cx);
        s.review.sheet.as_mut().unwrap().publish = true;
        s.sheet_send(w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .comment(1)
            .is_some_and(|c| c.github.is_some())
    })
    .await;
    let argv = fs::read_to_string(&log).unwrap();
    assert!(
        argv.starts_with("api|--method|POST|repos/{owner}/{repo}/pulls/7/reviews|-f|event=COMMENT|-f|commit_id=h|-f|body=**api/f**"),
        "{argv}"
    );
    assert!(matches!(comments(cx)[0].2, Status::Sent { .. }));
    on(cx, &|s, _, cx| s.set_status(&[1], Status::Pending, cx));
    // A PR review of pushed lines sits on them: gh api with the line.
    fs::write(&log, "").unwrap();
    on(cx, &|s, w, cx| {
        s.set_status(&[2], Status::Pending, cx);
        s.active_sync()
            .unwrap()
            .update(cx, |g, _| g.set_pr("api", 7, true));
        s.open_share(2, w, cx);
        s.review.share.as_mut().unwrap().review = true;
        s.post_share(w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).review.share.is_none()
    })
    .await;
    let argv = fs::read_to_string(&log).unwrap();
    assert!(
        argv.starts_with("api|--method|POST|repos/{owner}/{repo}/pulls/7/reviews|-f|event=COMMENT|-f|commit_id=h|-f|body=api/f:3|-f|comments[][path]=f|-F|comments[][line]=3|-f|comments[][side]=RIGHT|-f|comments[][body]=third line|"),
        "{argv}"
    );
    assert!(github(cx, 2).is_some_and(|u| u.ends_with("#pullrequestreview-5")));
    // gh fails: the dialog stays with the error, the comment unshared.
    fs::write(&fail, "").unwrap();
    on(cx, &|s, w, cx| {
        s.edit_comments(cx, |l| l[0].github = None);
        s.open_share(1, w, cx);
        s.post_share(w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .review
            .share
            .as_ref()
            .is_some_and(|s| !s.posting)
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        let err = s.review.share.as_ref().unwrap().error.clone().unwrap();
        assert!(err.contains("gh: offline"), "{err}");
        assert_eq!(s.comment(1).unwrap().github, None);
    });
    // The sheet publishes per repo: api's comment to api's PR; web has none, so its comment
    // stays unshared. The URL lands in the workspace the post came from, though another
    // one is open by the time gh answers.
    fs::remove_file(&fail).unwrap();
    fs::write(&log, "").unwrap();
    let ws = cx.update(|cx| shark.read(cx).active.clone().unwrap());
    on(cx, &|s, w, cx| {
        s.close_share(w, cx);
        let web: review::Comment = serde_json::from_str(
            r#"{"id":99,"path":"web/x","start":[false,1],"end":[false,1],"text":"web note"}"#,
        )
        .unwrap();
        s.edit_comments(cx, |l| l.push(web));
        let both = [
            s.comment(1).cloned().unwrap(),
            s.comment(99).cloned().unwrap(),
        ];
        s.publish(&both, "", false, w, cx);
        s.active = Some("elsewhere".into());
    });
    let github_in = |cx: &mut TestAppContext, id: u64| {
        cx.update(|cx| {
            let l = &shark.read(cx).review.comments[&ws];
            l.iter().find(|c| c.id == id).unwrap().github.clone()
        })
    };
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).review.comments[&ws][0].github.is_some()
    })
    .await;
    assert!(github_in(cx, 1).is_some());
    assert_eq!(github_in(cx, 99), None);
    let argv = fs::read_to_string(&log).unwrap();
    assert_eq!(argv.matches("pr|comment|").count(), 1, "{argv}");
    assert!(
        argv.contains("**api/f**") && !argv.contains("web note"),
        "{argv}"
    );
    cx.update(|cx| assert!(!shark.read(cx).review.comments.contains_key("elsewhere")));
    on(cx, &|s, _, cx| {
        s.active = Some(ws.clone());
        s.edit_comments(cx, |l| l.retain(|c| c.id != 99));
    });
    drop(gh_env);
    on(cx, &|s, w, cx| s.close_share(w, cx));

    // On a file being edited in place, a new thread quotes the buffer, not the disk.
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/f".into()), w, cx)
    });
    on(cx, &|s, w, cx| {
        s.editing.as_mut().unwrap().buf.lines[1] = "edited".into();
        let two = DiffLineRange::new("api/f", DiffSide::Modified, 2, 2);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(two), cx));
        s.comment_dialog(w, cx);
    });
    cx.update(|cx| {
        let comp = shark.read(cx).review.composer.as_ref().unwrap();
        assert_eq!(comp.draft.snapshot, "edited");
    });
    on(cx, &|s, w, cx| {
        s.cancel_composer(w, cx);
        s.editing = None;
    });

    // A comments file that doesn't parse is kept aside, not overwritten; one queued in a
    // chat from before a restart loads pending.
    let dir = crate::state::support_dir().join("comments");
    fs::write(dir.join("bad.json"), "{oops").unwrap();
    on(cx, &|s, _, _| s.review.load_comments("bad"));
    assert!(dir.join("bad.json.1.bak").exists() && !dir.join("bad.json").exists());
    fs::write(
        dir.join("old.json"),
        r#"[{"id":5,"path":"api/f","start":[false,1],"end":[false,1],"text":"q","status":"Queued","chat":"c1"}]"#,
    )
    .unwrap();
    on(cx, &|s, _, _| s.review.load_comments("old"));
    cx.update(|cx| {
        let c = &shark.read(cx).review.comments["old"][0];
        assert_eq!((&c.status, &c.chat), (&Status::Pending, &None));
    });

    // A thread whose file left the diff still shows, with its snapshot, below it.
    fs::remove_file(api.join("f")).unwrap();
    fs::write(api.join("g"), "x\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    on(cx, &|s, _, cx| {
        assert_eq!(s.diff_files.len(), 1);
        assert!(s.render_orphans(cx).is_some());
    });

    // Another workspace opens: the composer and the sheet were about this one's comments.
    on(cx, &|s, w, cx| {
        let one = DiffLineRange::new("api/g", DiffSide::Modified, 1, 1);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(one), cx));
        s.comment_dialog(w, cx);
        s.open_sheet(w, cx);
        assert!(s.review.composer.is_some() && s.review.sheet.is_some());
        s.create_workspace(
            "rq2".into(),
            git::on_branch(&repos, "rq2"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("rq2")
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.review.composer.is_none() && s.review.sheet.is_none());
    });
    fs::remove_dir_all(&home).ok();
}

/// A file card's header buttons stay at the diff's right edge, though a long line is wider
/// than the view and its code scrolls sideways.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn card_header_pinned(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
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
        s.create_workspace(
            "wide".into(),
            git::on_branch(&repos, "wide"),
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
    let long = format!("{}\n", "x".repeat(3000));
    fs::write(home.join("shark/workspaces/wide/api/f.txt"), long).unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        !shark.read(cx).diff_files.is_empty()
    })
    .await;
    let check = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            for _ in 0..3 {
                w.render_frame(cx);
            }
            let (button, collapse) = (w.find("edit-working-copy"), w.find("collapse-file"));
            let (b, body) = (button.bounds(), w.find("diff-body").bounds());
            assert!(button.visible(), "{b:?} in {body:?}");
            assert!(
                b.right() <= body.right() && b.right() > body.right() - px(40.),
                "{b:?} in {body:?}"
            );
            assert!(collapse.visible() && collapse.bounds().left() >= body.left());
        })
        .unwrap();
    };
    check(cx);
    // Scrolled sideways: the header stays.
    let plus = |w: &Window| w.find(("line", 0usize)).bounds().left();
    let before = cx
        .update_window(window, |_, w, _| {
            (plus(w), w.find("edit-working-copy").bounds())
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        let at = w.find("collapse-file").bounds().center() + point(px(0.), px(60.));
        w.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(-400.), px(0.))),
                ..Default::default()
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    check(cx);
    let after = cx
        .update_window(window, |_, w, _| {
            (plus(w), w.find("edit-working-copy").bounds())
        })
        .unwrap();
    // Only the code scrolls: line numbers and the header stay.
    assert_eq!(after, before);
    fs::remove_dir_all(&home).ok();
}

/// A narrow working column (split at 0.8): the composer's buttons, and a posted, addressed
/// thread's status pill, "On GitHub" and buttons stay inside the column.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn narrow_column_comments(cx: &mut TestAppContext) {
    use crate::diff_view::{DiffLineRange, DiffSide};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(1000.), px(800.)),
        ))),
        ..Default::default()
    };
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        options,
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "nc".into(),
            git::on_branch(&repos, "nc"),
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
    fs::write(home.join("shark/workspaces/nc/api/f"), "a\nb\nc\nd\n").unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    let posted: review::Comment = serde_json::from_str(
        r#"{"id":1,"path":"api/f","start":[false,1],"end":[false,1],"text":"Use the shared constant",
            "status":{"Addressed":{"turn":1,"note":"Pane ratio is now stored once and applied to every file block."}},
            "github":"https://github.com/o/api/pull/7#issuecomment-1"}"#,
    )
    .unwrap();
    on(cx, &|s, w, cx| {
        s.diff.update(cx, |d, cx| d.set_split_ratio(0.8, cx));
        s.edit_comments(cx, |l| l.push(posted.clone()));
        let three = DiffLineRange::new("api/f", DiffSide::Modified, 3, 3);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(three), cx));
        s.comment_dialog(w, cx);
    });
    cx.update_window(window, |_, w, cx| {
        for _ in 0..3 {
            w.render_frame(cx);
        }
        let body = w.find("diff-body").bounds();
        let inside = |card: ElementId, ids: &[ElementId]| {
            let col = w.find(card.clone()).bounds();
            assert!(col.size.width < body.size.width / 3., "{col:?} in {body:?}");
            for id in ids {
                let b = w.find(id.clone()).bounds();
                assert!(
                    b.left() >= col.left() && b.right() <= col.right() && b.size.width > px(0.),
                    "{id:?} {b:?} in {card:?} {col:?}"
                );
            }
        };
        inside(
            "composer-card".into(),
            &[
                "suggest-change".into(),
                "composer-esc".into(),
                "composer-add".into(),
            ],
        );
        inside(
            ("thread-card", 1usize).into(),
            &[
                ("thread-pill", 1usize).into(),
                ("thread-where", 1usize).into(),
                ("show-chat", 1usize).into(),
                ("view-gh", 1usize).into(),
            ],
        );
    })
    .unwrap();
    fs::remove_dir_all(&home).ok();
}

/// A thread card's right-click menu follows its status; Delete stays last.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn thread_menu_labels(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
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
        s.create_workspace(
            "tm".into(),
            git::on_branch(&repos, "tm"),
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
    fs::write(home.join("shark/workspaces/tm/api/f"), "a\nb\n").unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    let thread = |id: u64, status: &str| -> review::Comment {
        serde_json::from_str(&format!(
            r#"{{"id":{id},"path":"api/f","start":[false,1],"end":[false,1],"text":"x","status":{status}}}"#
        ))
        .unwrap()
    };
    let (open, done) = (
        thread(1, r#"{"Addressed":{"turn":1,"note":"n"}}"#),
        thread(2, r#""Resolved""#),
    );
    on(cx, &|s, _, cx| {
        s.edit_comments(cx, |l| l.extend([open.clone(), done.clone()]))
    });
    let labels = |cx: &mut TestAppContext, id: u64| {
        shark.update(cx, |s, cx| {
            (s.thread_menu(id, cx).into_iter())
                .map(|e| match e {
                    Entry::Item { label, .. } => label.to_string(),
                    _ => "-".into(),
                })
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        labels(cx, 1),
        [
            "Edit",
            "Resolve",
            "Reopen",
            "View change",
            "Show chat",
            "Share to GitHub…",
            "Copy comment",
            "-",
            "Delete"
        ]
    );
    assert_eq!(
        labels(cx, 2),
        [
            "Edit",
            "Reopen",
            "Share to GitHub…",
            "Copy comment",
            "-",
            "Delete"
        ]
    );
    fs::remove_dir_all(&home).ok();
}

/// In-place editing, round 2: the line input lines up with the code, owns tab and the
/// letters, a click in it keeps its caret, a second ⌘S waits for the first, other writers
/// are noticed, a refresh adding a file keeps the input's focus, the composer takes focus
/// from it, and leaving edit mode or the workspace asks first.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn inline_edit_round2(cx: &mut TestAppContext) {
    use crate::diff_view::{DiffLinePosition, DiffLineRange, DiffSide};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    let lines = |f: &dyn Fn(usize) -> String| (1..=300).map(f).collect::<String>();
    fs::write(
        repos[0].path.join("f.txt"),
        lines(&|i| format!("line {i}\n")),
    )
    .unwrap();
    git::git(&repos[0].path, &["add", "."]).unwrap();
    let commit = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "f",
    ];
    git::git(&repos[0].path, &commit).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    for name in ["ed", "other"] {
        on(cx, &|s, w, cx| {
            s.create_workspace(
                name.into(),
                git::on_branch(&repos, name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    on(cx, &|s, w, cx| {
        let ix = (s.state.workspaces.iter())
            .position(|w| w.name == "ed")
            .unwrap();
        s.open_workspace(ix, w, cx)
    });
    let file = home.join("shark/workspaces/ed/api/f.txt");
    // A change every ten lines: a diff many screens tall.
    let changed = |i: usize| match i % 10 {
        0 => format!("changed {i}\n"),
        _ => format!("line {i}\n"),
    };
    fs::write(&file, lines(&changed)).unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.diff.update(cx, |d, cx| d.set_mode(DiffMode::Split, cx));
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    // Files is closed until asked for.
    cx.update(|cx| assert!(!shark.read(cx).tree_shown()));
    let draw = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
    };
    on(cx, &|s, w, cx| {
        let line = DiffLineRange::new("api/f.txt", DiffSide::Modified, 250, 250);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    draw(cx);
    let value = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let e = shark.read(cx).editing.as_ref().unwrap();
            (
                e.buf.lines[e.buf.line].clone(),
                e.input.read(cx).value().to_string(),
            )
        })
    };
    assert_eq!(value(cx).1, "changed 250");
    let focused = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            shark.read(cx).editing.as_ref().unwrap().focused(w, cx)
        })
        .unwrap()
    };
    assert!(focused(cx));
    // The input sits where the line's code would, one row tall; the header has no Comment.
    cx.update_window(window, |_, w, cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        let input = w.find(("input", e.input.entity_id())).bounds();
        let row = w.find_all(("line", 249usize))[0].bounds();
        let code = (w.find_all("source").iter())
            .filter(|s| s.bounds().left() == input.left())
            .count();
        assert_eq!(input.size.height, row.size.height, "{input:?} vs {row:?}");
        assert!(code > 2, "{input:?} starts where the code does");
    })
    .unwrap();

    // Letters are typed, not run as the diff's keys; tab indents and keeps focus.
    let unified = cx.update(|cx| shark.read(cx).state.unified);
    press(
        cx,
        &["s", "r", "c", "v", "x", "d", "e", "o", "i", "shift-c", "}"],
    );
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.state.unified, unified);
        assert!(s.review.composer.is_none() && s.editor.is_none() && s.review.sheet.is_none());
        assert!(s.viewed_now.is_empty() && s.editing.is_some());
    });
    cx.update_window(window, |_, w, cx| w.input("sr", cx))
        .unwrap();
    press(cx, &["tab"]);
    draw(cx);
    let (buf, input) = value(cx);
    assert_eq!(buf, input);
    assert_eq!(buf, "changed 250srcvxdeoiC}sr    ");
    assert!(focused(cx), "tab kept the focus");
    // Home and end move in the line, not the diff.
    press(cx, &["home"]);
    let caret = |cx: &mut TestAppContext| {
        cx.update(|cx| (shark.read(cx).editing.as_ref().unwrap().input.read(cx)).cursor())
    };
    assert!(focused(cx));
    assert_eq!(caret(cx), 0);
    press(cx, &["end"]);
    assert_eq!(caret(cx), buf.len());

    // A click on the line being edited leaves its caret and selection to the input.
    on(cx, &|s, _, cx| {
        let e = s.editing.as_ref().unwrap();
        e.input.update(cx, |i, cx| i.set_selected_range(2..4, cx));
    });
    on(cx, &|s, w, cx| {
        s.edit_click(
            &DiffLinePosition::new("api/f.txt", DiffSide::Modified, 250),
            w,
            cx,
        )
    });
    cx.update(|cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        assert_eq!(e.input.read(cx).selected_range(), 2..4);
    });

    // Unsaved: the editor won't open the same file; leaving the workspace asks.
    on(cx, &|s, w, cx| s.open_path("api/f.txt".into(), w, cx));
    cx.update(|cx| assert!(shark.read(cx).editor.is_none()));
    on(cx, &|s, w, cx| {
        s.home.sel = Some("other".into());
        s.try_plan_mode(w, cx)
    });
    cx.update_window(window, |_, w, cx| {
        assert!(w.has_active_dialog(cx), "asks before leaving");
        w.close_dialog(cx);
    })
    .unwrap();
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("ed")));

    // Two ⌘S in a row: one write, no false "changed on disk".
    on(cx, &|s, w, cx| {
        s.save_edit(false, w, cx);
        s.save_edit(false, w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.as_ref().is_some_and(|e| e.saved)
    })
    .await;
    cx.update_window(window, |_, w, cx| assert!(!w.has_active_dialog(cx)))
        .unwrap();
    let want = cx.update(|cx| shark.read(cx).editing.as_ref().unwrap().buf.text());
    assert_eq!(fs::read_to_string(&file).unwrap(), want);

    // Someone else writes the file: a clean buffer takes it, the input too.
    let theirs = want
        .replace("changed 250srcvxdeoiC}sr    ", "theirs changed 250")
        .replace("line 3\n", "line 3!\n");
    fs::write(&file, &theirs).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .editing
            .as_ref()
            .is_some_and(|e| e.buf.text() == theirs)
    })
    .await;
    draw(cx);
    assert_eq!(value(cx).1, "theirs changed 250");
    // Under unsaved edits it is only marked, and ⌘S asks.
    cx.update_window(window, |_, w, cx| w.input("!", cx))
        .unwrap();
    draw(cx);
    let agent = theirs.replace("line 5\n", "line 5!\n");
    fs::write(&file, &agent).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.as_ref().is_some_and(|e| e.stale)
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.working_heading("api/f.txt"),
            "Working copy · changed on disk"
        );
        assert!(s.editing.as_ref().unwrap().dirty);
    });
    on(cx, &|s, w, cx| s.save_edit(false, w, cx));
    let mut shown = false;
    for _ in 0..100 {
        cx.run_until_parked();
        shown = cx
            .update_window(window, |_, w, cx| w.has_active_dialog(cx))
            .unwrap();
        if shown {
            break;
        }
        cx.background_executor
            .timer(Duration::from_millis(20))
            .await;
    }
    assert!(shown, "saving over their change asks");
    cx.update_window(window, |_, w, cx| w.close_dialog(cx))
        .unwrap();
    // Cancel: their change stays on disk.
    assert_eq!(fs::read_to_string(&file).unwrap(), agent);
    // Overwrite: the buffer is written over it.
    on(cx, &|s, w, cx| s.save_edit(true, w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .editing
            .as_ref()
            .is_some_and(|e| e.saved && !e.stale)
    })
    .await;
    let ours = cx.update(|cx| shark.read(cx).editing.as_ref().unwrap().buf.text());
    assert_eq!(fs::read_to_string(&file).unwrap(), ours);
    assert!(ours.contains("theirs changed 250!\n") && !ours.contains("line 5!"));

    // A new file reloads the diff; the line input keeps its focus.
    on(cx, &|s, w, cx| s.focus_edit(w, cx));
    draw(cx);
    assert!(focused(cx));
    fs::write(home.join("shark/workspaces/ed/api/g.txt"), "g\n").unwrap();
    // As after a mouse scroll: no keyboard view to come back to.
    on(cx, &|s, _, cx| {
        s.lose_view();
        s.refresh_diff(cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 2
    })
    .await;
    draw(cx);
    assert!(focused(cx), "the refresh kept the input's focus");
    // …and it is drawn, so typing reaches it.
    cx.update_window(window, |_, w, cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        let input = w.find_all(("input", e.input.entity_id()));
        let top = shark.read(cx).diff.read(cx).scroll_top();
        assert!(input.iter().any(|i| i.visible()), "{input:?} at {top:?}");
    })
    .unwrap();
    let before = value(cx).0;
    cx.update_window(window, |_, w, cx| w.input("Q", cx))
        .unwrap();
    draw(cx);
    assert_eq!(value(cx).0, format!("{before}Q"));

    // A filter: a folder's box ticks only the files it shows.
    on(cx, &|s, _, cx| {
        s.filter_tree("g.txt".into(), cx);
        s.toggle_viewed_dir("api", cx);
        // Folding a folder the filter shows open would only change hidden state.
        s.tree_sel = 0;
        s.set_dir_open(Some(false), cx);
        assert!(s.collapsed_dirs.is_empty());
        s.filter_tree(String::new(), cx);
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.viewed_now.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            ["api/g.txt"]
        );
    });

    // The composer, opened while editing, takes the typing.
    on(cx, &|s, w, cx| {
        let line = DiffLineRange::new("api/f.txt", DiffSide::Original, 249, 249);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
        s.comment_dialog(w, cx)
    });
    draw(cx);
    let before = value(cx);
    cx.update_window(window, |_, w, cx| w.input("hello", cx))
        .unwrap();
    draw(cx);
    assert_eq!(value(cx), before);
    cx.update(|cx| {
        let c = shark.read(cx).review.composer.as_ref().unwrap();
        assert_eq!(c.input.read(cx).value().as_ref(), "hello");
    });
    // The edited file's header: [Review] [Save ⌘S], no Comment.
    on(cx, &|s, _, cx| {
        s.diff.update(cx, |d, cx| d.scroll_to_file("api/f.txt", cx))
    });
    draw(cx);
    cx.update_window(window, |_, w, _| {
        assert!(w.find_all("edit-save").len() == 1);
        assert!(w.find_all("comment:api/f.txt").is_empty());
    })
    .unwrap();
    // Back to reviewing: git's view as of now, not of the last refresh.
    fs::write(&file, lines(&|i| format!("line {i}\n")) + "tail\n").unwrap();
    on(cx, &|s, w, cx| {
        s.editing.as_mut().unwrap().dirty = false;
        s.leave_edit(w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        s.editing.is_none()
            && s.diff_files
                .iter()
                .any(|f| f.additions() == 1 && f.deletions() == 0)
    })
    .await;
    fs::remove_dir_all(&home).ok();
}

/// Review, round 3: keys leave a mouse selection behind; the composer at the bottom shows
/// whole; the sheet publishes to the ticked rows' PRs and opens its preview; a review names
/// a fresh chat; a turn judges what claude read, whatever the turn count; a closed chat hands
/// back what it never read; a share posted under another dialog leaves that dialog open.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_round3(cx: &mut TestAppContext) {
    use review::Status;
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
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "r3".into(),
            git::on_branch(&repos, "r3"),
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
    let api = home.join("shark/workspaces/r3/api");
    let text: String = (1..=80).map(|i| format!("line {i}\n")).collect();
    fs::write(api.join("f"), text).unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    let draw = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
    };
    let comments = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            (shark.read(cx).ws_comments().iter())
                .map(|c| (c.id, c.status.clone()))
                .collect::<Vec<_>>()
        })
    };
    on(cx, &|s, _, _| s.review.at_file("api/f"));
    draw(cx);

    // Lines 3–5 dragged over with the mouse, then a key moves the cursor: c is about the
    // cursor's line, not the selection left behind.
    let (from, to) = cx
        .update_window(window, |_, w, _| {
            let mut code: Vec<_> = (w.find_all("source").iter()).map(|s| s.bounds()).collect();
            let left = code[0].left();
            code.retain(|b| b.left() == left);
            code.sort_by(|a, b| a.top().partial_cmp(&b.top()).unwrap());
            let at = |b: Bounds<Pixels>| point(b.left() + px(2.), b.center().y);
            (at(code[2]), at(code[4]))
        })
        .unwrap();
    let mouse = |cx: &mut TestAppContext, e: PlatformInput| {
        cx.update_window(window, |_, w, cx| {
            w.dispatch_event(e, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let left = MouseButton::Left;
    mouse(
        cx,
        PlatformInput::MouseDown(MouseDownEvent {
            button: left,
            position: from,
            click_count: 1,
            ..Default::default()
        }),
    );
    mouse(
        cx,
        PlatformInput::MouseMove(MouseMoveEvent {
            position: to,
            pressed_button: Some(left),
            ..Default::default()
        }),
    );
    mouse(
        cx,
        PlatformInput::MouseUp(MouseUpEvent {
            button: left,
            position: to,
            click_count: 1,
            ..Default::default()
        }),
    );
    let dragged = |cx: &mut TestAppContext| {
        cx.update(|cx| shark.read(cx).diff.read(cx).text_selection_lines(cx))
    };
    let lines = dragged(cx).expect("a mouse selection");
    assert_eq!(lines.start(), 3);
    press(cx, &["down", "c"]);
    assert!(dragged(cx).is_none(), "the key cleared it");
    cx.update(|cx| {
        let comp = shark.read(cx).review.composer.as_ref().unwrap();
        assert_eq!(comp.draft.lines(), "1");
    });
    press(cx, &["escape"]);

    // The composer near the end shows whole above the bar, measured once laid out: here
    // with Suggest change open on ten lines, taller than a first guess.
    on(cx, &|s, w, cx| {
        s.diff_move(0, Some(false), w, cx);
        let ten = crate::diff_view::DiffLineRange::new(
            "api/f",
            crate::diff_view::DiffSide::Modified,
            70,
            79,
        );
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(ten), cx));
        s.comment_dialog(w, cx);
    });
    draw(cx);
    press(cx, &["cmd-shift-s"]);
    draw(cx);
    cx.update_window(window, |_, w, _| {
        let (add, body) = (
            w.find("composer-add").bounds(),
            w.find("diff-body").bounds(),
        );
        assert!(add.bottom() <= body.bottom(), "{add:?} in {body:?}");
    })
    .unwrap();
    press(cx, &["cmd-shift-s"]); // back to the text box
    cx.update_window(window, |_, w, cx| w.input("last lines", cx))
        .unwrap();
    press(cx, &["cmd-enter"]);
    assert_eq!(comments(cx), [(1, Status::Pending)]);

    // The sheet's Also publish follows the ticked rows' repos; ticking it opens the preview.
    on(cx, &|s, w, cx| {
        s.active_sync()
            .unwrap()
            .update(cx, |g, _| g.set_pr("api", 7, false));
        let web: review::Comment = serde_json::from_str(
            r#"{"id":50,"path":"web/x","start":[false,1],"end":[false,1],"text":"web"}"#,
        )
        .unwrap();
        s.edit_comments(cx, |l| l.push(web));
        s.open_sheet(w, cx);
        assert_eq!(s.sheet_prs(cx), [("api".to_string(), 7)]);
        s.sheet_toggle(cx); // unticks api's row
        assert!(s.sheet_prs(cx).is_empty(), "web has no PR");
        s.sheet_toggle(cx);
        s.sheet_publish(true, cx);
        assert!(s.review.sheet.as_ref().unwrap().preview);
        s.close_sheet(w, cx);
        s.edit_comments(cx, |l| l.retain(|c| c.id != 50));
    });

    // A review as a chat's first message names it.
    let chat = cx.update(|cx| shark.read(cx).active_chat().cloned().unwrap());
    on(cx, &|s, w, cx| s.send_review(&[1], "", false, w, cx));
    cx.update(|cx| assert_eq!(chat.read(cx).meta.title, "Review · 1 comment"));
    let reply = |cx: &mut TestAppContext, t: &str| {
        let lines = [
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": t}]}, "parent_tool_use_id": null}).to_string(),
            serde_json::json!({"type": "result", "is_error": false, "result": t, "total_cost_usd": 0, "duration_ms": 1}).to_string(),
        ];
        cx.update(|cx| {
            chat.update(cx, |c, cx| {
                let lines: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
                crate::chat::ui_tests::play(c, &lines, cx)
            })
        });
        cx.run_until_parked();
    };
    // Sent in a turn numbered past the chat's count (as after a restart replays fewer
    // messages): its report still lands.
    on(cx, &|s, _, cx| {
        s.set_status(&[1], Status::Sent { turn: 99 }, cx)
    });
    reply(cx, "[c1] addressed: Done.");
    assert!(matches!(comments(cx)[0].1, Status::Addressed { .. }));

    // Steered into a running turn and not read yet: that turn's end doesn't judge it.
    on(cx, &|s, _, cx| s.set_status(&[1], Status::Pending, cx));
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| s.send_review(&[1], "", false, w, cx));
    assert_eq!(comments(cx)[0].1, Status::Queued);
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    cx.update_window(window, |_, w, cx| {
        chat.update(cx, |c, cx| c.focus_queue(w, cx));
        w.render_frame(cx);
    })
    .unwrap();
    press(cx, &["enter"]);
    assert!(matches!(comments(cx)[0].1, Status::Sent { .. }));
    reply(cx, "[c1] addressed: Too early.");
    assert!(
        matches!(comments(cx)[0].1, Status::Sent { .. }),
        "still unread"
    );

    // Closed before claude read it: pending again.
    cx.update(|cx| chat.update(cx, |c, _| c.busy = false));
    on(cx, &|s, w, cx| s.close_chat(w, cx));
    assert_eq!(comments(cx)[0].1, Status::Pending);
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));

    // Posted while another dialog sits on the share dialog: that one stays open; the share
    // dialog says it posted and closes from there.
    let bin = home.join("gh-bin");
    fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\necho https://github.com/o/api/pull/7#issuecomment-9\n",
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&gh)
        .status()
        .unwrap();
    let gh_env = GhGuard::set(&gh);
    on(cx, &|s, w, cx| {
        s.open_share(1, w, cx);
        w.open_dialog(cx, |d, _, _| d.title("Other"));
        s.post_share(w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark
            .read(cx)
            .comment(1)
            .is_some_and(|c| c.github.is_some())
    })
    .await;
    drop(gh_env);
    cx.update(|cx| {
        assert!(
            shark.read(cx).review.share.is_some(),
            "not closed under Other"
        )
    });
    on(cx, &|s, w, cx| {
        w.close_dialog(cx); // Other
        s.post_share(w, cx); // "Posted · Close"
    });
    cx.update_window(window, |_, w, cx| {
        assert!(shark.read(cx).review.share.is_none() && !w.has_active_dialog(cx));
    })
    .unwrap();
    fs::remove_dir_all(&home).ok();
}

/// In-place editing, round 3: with Vim on, its keys type in the line input, the composer
/// and the Files filter; tab indents by the file's unit and ⌘Z undoes it; esc leaves even
/// when the input lost focus; a file leaving the diff ends a clean edit and keeps a dirty
/// one; an emptied file has a line to edit; leaving waits for a save under way.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn inline_edit_round3(cx: &mut TestAppContext) {
    use crate::diff_view::{DiffLineRange, DiffSide};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    fs::write(repos[0].path.join("f.txt"), "fn a() {\n    b();\n}\n").unwrap();
    fs::write(repos[0].path.join("h.txt"), "h\n").unwrap();
    git::git(&repos[0].path, &["add", "."]).unwrap();
    let commit = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "f",
    ];
    git::git(&repos[0].path, &commit).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            vim: true,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "ed".into(),
            git::on_branch(&repos, "ed"),
            vec![],
            None,
            w,
            cx,
        )
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("ed")
    })
    .await;
    let dir = home.join("shark/workspaces/ed/api");
    let file = dir.join("f.txt");
    fs::write(&file, "fn a() {\n    c();\n}\n").unwrap();
    let wait_files = async |cx: &mut TestAppContext, n: usize| {
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).diff_files.len() == n
        })
        .await
    };
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    wait_files(cx, 1).await;
    let draw = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
    };
    const VIM: [&str; 6] = ["j", "k", "h", "l", "g", "shift-g"];

    // The Files filter.
    on(cx, &|s, w, cx| s.toggle_tree(w, cx));
    let filter = cx.update(|cx| shark.read(cx).tree_filter.clone());
    on(cx, &|_, w, cx| {
        filter.read(cx).focus_handle(cx).focus(w, cx)
    });
    press(cx, &VIM);
    cx.update(|cx| assert_eq!(filter.read(cx).value().as_ref(), "jkhlgG"));
    on(cx, &|s, w, cx| {
        filter.update(cx, |i, cx| i.set_value("", w, cx));
        s.filter_tree(String::new(), cx);
    });

    // The line input; tab indents by the file's four spaces, ⌘Z undoes it.
    on(cx, &|s, w, cx| {
        let line = DiffLineRange::new("api/f.txt", DiffSide::Modified, 2, 2);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    draw(cx);
    let line = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let e = shark.read(cx).editing.as_ref().unwrap();
            (
                e.buf.lines[e.buf.line].clone(),
                e.input.read(cx).value().to_string(),
            )
        })
    };
    press(cx, &VIM);
    assert_eq!(line(cx).0, "    c();jkhlgG");
    press(cx, &["tab"]);
    assert_eq!(
        line(cx),
        ("    c();jkhlgG    ".into(), "    c();jkhlgG    ".into())
    );
    press(cx, &["cmd-z"]);
    assert_eq!(line(cx).0, "    c();jkhlgG");
    // Enter over a selection replaces it, then splits.
    on(cx, &|s, _, cx| {
        let input = s.editing.as_ref().unwrap().input.clone();
        input.update(cx, |i, cx| i.set_selected_range(6..8, cx));
    });
    press(cx, &["enter"]);
    cx.update(|cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        assert_eq!(e.buf.lines[1..3], ["    c(", "jkhlgG"]);
    });

    // The composer, opened while editing.
    on(cx, &|s, w, cx| {
        let line = DiffLineRange::new("api/f.txt", DiffSide::Original, 1, 1);
        s.diff
            .update(cx, |d, cx| d.set_selected_lines(Some(line), cx));
        s.comment_dialog(w, cx)
    });
    draw(cx);
    press(cx, &VIM);
    cx.update(|cx| {
        let c = shark.read(cx).review.composer.as_ref().unwrap();
        assert_eq!(c.input.read(cx).value().as_ref(), "jkhlgG");
    });
    // Esc in the composer closes only the composer.
    press(cx, &["escape"]);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.review.composer.is_none());
        assert!(s.editing.is_some(), "still editing");
    });

    // Esc with the focus on the diff, not the input: back to reviewing.
    on(cx, &|s, w, cx| {
        s.editing.as_mut().unwrap().dirty = false;
        s.focus_diff(w, cx)
    });
    press(cx, &["escape"]);
    cx.update(|cx| assert!(shark.read(cx).editing.is_none()));

    // Unsaved edits, Esc in the line, Discard: the diff has the focus again, for j/k.
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    draw(cx);
    cx.update_window(window, |_, w, cx| w.input("z", cx))
        .unwrap();
    press(cx, &["escape"]);
    draw(cx);
    press(cx, &["enter"]);
    draw(cx);
    cx.background_executor
        .timer(Duration::from_millis(500))
        .await;
    draw(cx);
    cx.update_window(window, |_, w, cx| {
        let s = shark.read(cx);
        assert!(s.editing.is_none(), "discarded");
        assert!(
            s.diff.read(cx).focus_handle(cx).is_focused(w),
            "the diff has the focus"
        );
    })
    .unwrap();

    // A save under way: leaving waits for it.
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    on(cx, &|s, w, cx| {
        s.editing.as_mut().unwrap().saving = true;
        s.leave_edit(w, cx);
        s.editing.as_mut().unwrap().saving = false;
    });
    cx.update(|cx| assert!(shark.read(cx).editing.is_some()));

    // The file leaves the diff: a clean edit ends…
    git::git(&dir, &["checkout", "--", "f.txt"]).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    wait_files(cx, 0).await;
    cx.update(|cx| assert!(shark.read(cx).editing.is_none()));
    // …a dirty one keeps its file.
    fs::write(&file, "fn a() {\n    c();\n}\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    wait_files(cx, 1).await;
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    draw(cx);
    cx.update_window(window, |_, w, cx| w.input("!", cx))
        .unwrap();
    press(cx, &["enter"]);
    git::git(&dir, &["checkout", "--", "f.txt"]).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    for _ in 0..50 {
        cx.run_until_parked();
        cx.background_executor
            .timer(Duration::from_millis(20))
            .await;
    }
    cx.update(|cx| {
        let s = shark.read(cx);
        let e = s.editing.as_ref().unwrap();
        assert!(e.dirty);
        assert!(s.diff_files.iter().any(|f| f.path() == "api/f.txt"));
        // git's original stays, not the buffer (two added lines).
        assert_eq!((e.file.additions(), e.orig.0.additions()), (2, 1));
    });

    // An emptied file: its one empty line is there to edit.
    on(cx, &|s, _, cx| {
        s.editing.as_mut().unwrap().dirty = false;
        s.end_edit(cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        let f = s.diff_files.iter().find(|f| f.path() == "api/f.txt");
        assert_eq!(f.map(|f| f.additions()), Some(1));
        assert_eq!(s.diff_fp, 0, "the next refresh rebuilds");
    });
    fs::write(dir.join("h.txt"), "").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).diff_files.iter()).any(|f| f.path() == "api/h.txt")
    })
    .await;
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/h.txt".into()), w, cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.editing.as_ref().unwrap().path.as_str(), "api/h.txt");
        assert!(
            s.review.hunks["api/h.txt"]
                .iter()
                .flatten()
                .any(|r| r.0 == b'+' && r.2 == 1)
        );
    });

    // A mode-only change: the base is the same text, not an empty added file.
    on(cx, &|s, _, cx| s.end_edit(cx));
    git::git(&dir, &["checkout", "--", "h.txt"]).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir.join("h.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        (s.diff_files.iter()).any(|f| f.path() == "api/h.txt" && f.additions() == 0)
    })
    .await;
    on(cx, &|s, w, cx| {
        s.edit_working_copy(Some("api/h.txt".into()), w, cx)
    });
    cx.update(|cx| {
        let e = shark.read(cx).editing.as_ref().unwrap();
        assert_eq!(e.path.as_str(), "api/h.txt");
        assert_eq!((e.file.additions(), e.file.deletions()), (0, 0));
    });
    fs::remove_dir_all(&home).ok();
}

/// In-place editing, round 4: a refresh started before switching files leaves the new
/// buffer alone; Quit, the window's close button, opening or removing the workspace ask
/// before unsaved edits go; back from Chat, `i` and ⌘S reach the line input; a viewed file
/// stays open while edited; saving back to the base keeps editing; a deleted file is marked;
/// symlinks aren't edited.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn inline_edit_round4(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    fs::write(repos[0].path.join("f.txt"), "f\n").unwrap();
    fs::write(repos[0].path.join("g.txt"), "g\n").unwrap();
    git::git(&repos[0].path, &["add", "."]).unwrap();
    let commit = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "f",
    ];
    git::git(&repos[0].path, &commit).unwrap();
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let on = on_app(window, shark.clone());
    let press = press_in(window);
    for name in ["other", "ed"] {
        on(cx, &|s, w, cx| {
            s.create_workspace(
                name.into(),
                git::on_branch(&repos, name),
                vec![],
                None,
                w,
                cx,
            )
        });
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            shark.read(cx).active.as_deref() == Some(name)
        })
        .await;
    }
    let dir = home.join("shark/workspaces/ed/api");
    fs::write(dir.join("f.txt"), "f2\n").unwrap();
    fs::write(dir.join("g.txt"), "g2\n").unwrap();
    on(cx, &|s, w, cx| {
        s.show(Tab::Diff, w, cx);
        s.refresh_diff(cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 2
    })
    .await;
    let draw = |cx: &mut TestAppContext| {
        for _ in 0..3 {
            cx.update_window(window, |_, w, cx| w.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
    };
    let edit = |path: &'static str| {
        move |s: &mut Shark, w: &mut Window, cx: &mut Context<Shark>| {
            s.edit_working_copy(Some(path.into()), w, cx)
        }
    };
    let text = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let e = shark.read(cx).editing.as_ref().unwrap();
            (e.path.to_string(), e.buf.text())
        })
    };
    let dialog = |cx: &mut TestAppContext| {
        draw(cx);
        cx.update_window(window, |_, w, cx| w.has_active_dialog(cx))
            .unwrap()
    };
    let cancel = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| w.close_dialog(cx))
            .unwrap();
        draw(cx);
    };
    let focused = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            shark.read(cx).editing.as_ref().unwrap().focused(w, cx)
        })
        .unwrap()
    };

    // A refresh that read f.txt lands after the switch to g.txt: g's buffer keeps g.
    on(cx, &edit("api/f.txt"));
    fs::write(dir.join("f.txt"), "f3\n").unwrap();
    on(cx, &|s, w, cx| {
        s.refresh_diff(cx);
        s.edit_working_copy(Some("api/g.txt".into()), w, cx)
    });
    draw(cx);
    assert_eq!(text(cx), ("api/g.txt".into(), "g2\n".into()));

    // Unsaved edits: each way out asks, and Cancel keeps them.
    cx.update_window(window, |_, w, cx| w.input("!", cx))
        .unwrap();
    draw(cx);
    assert_eq!(text(cx).1, "g2!\n");
    on(cx, &|s, w, cx| s.quit(w, cx));
    assert!(dialog(cx), "Quit asks");
    cancel(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    assert!(!vcx.simulate_close(), "the close button waits");
    assert!(dialog(cx), "the close button asks");
    cancel(cx);
    on(cx, &|s, w, cx| {
        let ix = (s.state.workspaces.iter())
            .position(|w| w.name == "other")
            .unwrap();
        s.open_workspace(ix, w, cx)
    });
    assert!(dialog(cx), "opening another workspace asks");
    cancel(cx);
    on(cx, &|s, w, cx| s.remove_workspace("ed", false, w, cx));
    assert!(dialog(cx), "removing the workspace asks");
    cancel(cx);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(s.active.as_deref(), Some("ed"));
        assert!(s.ws("ed").is_some() && s.editing.as_ref().unwrap().dirty);
    });

    // Back from Chat the line input has the focus; with the focus on the diff, `i` and ⌘S
    // still reach it.
    on(cx, &|s, w, cx| s.show(Tab::Chat, w, cx));
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    draw(cx);
    assert!(focused(cx), "back from Chat");
    // `i` goes by the selected line's file (g), not the file last moved to (f).
    on(cx, &|s, w, cx| {
        use crate::diff_view::{DiffLineRange, DiffSide};
        s.diff_file = (s.diff_files.iter())
            .position(|f| f.path() == "api/f.txt")
            .unwrap();
        let line = DiffLineRange::new("api/g.txt", DiffSide::Modified, 1, 1);
        s.diff.update(cx, |d, cx| {
            d.set_selected_lines(Some(line), cx);
            d.scroll_to_file("api/f.txt", cx)
        });
        s.focus_diff(w, cx)
    });
    press(cx, &["i"]);
    draw(cx);
    assert!(!dialog(cx));
    assert!(focused(cx), "i on the edited file");
    on(cx, &|s, w, cx| s.focus_diff(w, cx));
    press(cx, &["cmd-s"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.as_ref().is_some_and(|e| e.saved)
    })
    .await;
    assert_eq!(fs::read_to_string(dir.join("g.txt")).unwrap(), "g2!\n");

    // Viewed: the edited file's card stays open through a refresh.
    on(cx, &|s, _, cx| {
        let ix = (s.diff_files.iter())
            .position(|f| f.path() == "api/g.txt")
            .unwrap();
        s.toggle_viewed(ix, cx);
        s.diff_fp = 0;
        s.refresh_diff(cx);
    });
    draw(cx);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.viewed_now.iter().any(|p| p == "api/g.txt"));
        assert!(!s.diff.read(cx).is_file_collapsed("api/g.txt"));
    });

    // Saved back to the base: still editing.
    on(cx, &|s, w, cx| {
        let e = s.editing.as_mut().unwrap();
        e.buf = inline_edit::Buffer::new("g\n");
        e.dirty = true;
        s.save_edit(false, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        s.editing.as_ref().is_some_and(|e| e.saved && !e.dirty)
    })
    .await;
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    draw(cx);
    assert_eq!(fs::read_to_string(dir.join("g.txt")).unwrap(), "g\n");
    assert_eq!(text(cx), ("api/g.txt".into(), "g\n".into()));

    // Deleted on disk: marked.
    on(cx, &edit("api/f.txt"));
    assert_eq!(text(cx).0, "api/f.txt");
    fs::remove_file(dir.join("f.txt")).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.as_ref().is_some_and(|e| e.stale)
    })
    .await;
    // Back as it was read: no longer stale.
    fs::write(dir.join("f.txt"), "f3\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).editing.as_ref().is_some_and(|e| !e.stale)
    })
    .await;
    // From another tab (the menu, the palette), editing shows the diff.
    on(cx, &|s, w, cx| {
        s.tab = Tab::File;
        s.edit_working_copy(Some("api/f.txt".into()), w, cx)
    });
    cx.update(|cx| assert!(shark.read(cx).tab == Tab::Diff));

    // A symlink isn't edited (saving would write through it).
    on(cx, &|s, _, cx| s.end_edit(cx));
    std::os::unix::fs::symlink("g.txt", dir.join("link.txt")).unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).diff_files.iter()).any(|f| f.path() == "api/link.txt")
    })
    .await;
    on(cx, &edit("api/link.txt"));
    cx.update(|cx| assert!(shark.read(cx).editing.is_none()));

    // Unsaved, the close button's Discard closes the window.
    fs::write(dir.join("f.txt"), "f4\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).diff_files.iter()).any(|f| f.path() == "api/f.txt" && f.additions() == 1)
    })
    .await;
    on(cx, &edit("api/f.txt"));
    draw(cx);
    cx.update_window(window, |_, w, cx| w.input("?", cx))
        .unwrap();
    draw(cx);
    assert!(text(cx).1.contains('?'));
    let mut vcx = VisualTestContext::from_window(window, cx);
    assert!(!vcx.simulate_close());
    assert!(dialog(cx));
    press(cx, &["enter"]);
    assert!(cx.update_window(window, |_, _, _| ()).is_err(), "closed");
    fs::remove_dir_all(&home).ok();
}

/// `SHARK_GH` pointed at a fake gh while it lives, so a failing test doesn't leave it set.
struct GhGuard;

impl GhGuard {
    fn set(path: &std::path::Path) -> Self {
        // SAFETY: tests that read SHARK_GH hold UI_LOCK.
        unsafe { std::env::set_var("SHARK_GH", path) };
        GhGuard
    }
}

impl Drop for GhGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`; the lock is still held.
        unsafe { std::env::remove_var("SHARK_GH") };
    }
}

/// `]c` steps through threads that start on lines another one covers, instead of going back
/// to the first.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_thread_jumps_overlapping(cx: &mut TestAppContext) {
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
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "tj".into(),
            git::on_branch(&repos, "tj"),
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
    let api = home.join("shark/workspaces/tj/api");
    fs::write(api.join("f"), "1\n2\n3\n4\n5\n6\n").unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    // c1 on 1-4, c2 on 3-4, c3 on 6.
    let c = |id, a, b| -> review::Comment {
        serde_json::from_str(&format!(
            r#"{{"id":{id},"path":"api/f","start":[false,{a}],"end":[false,{b}],"text":"t"}}"#
        ))
        .unwrap()
    };
    on(cx, &|s, _, cx| {
        s.edit_comments(cx, |l| l.extend([c(1, 1, 4), c(2, 3, 4), c(3, 6, 6)]))
    });
    let at = |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).thread_at(cx));
    on(cx, &|s, w, cx| s.jump_to_comment(1, w, cx));
    assert_eq!(at(cx), Some(1));
    for (forward, want) in [(true, [2, 3, 1]), (false, [3, 2, 1])] {
        let mut seen = vec![];
        for _ in 0..3 {
            on(cx, &|s, w, cx| s.thread_jump(forward, w, cx));
            seen.push(at(cx).unwrap());
        }
        assert_eq!(seen, want);
    }
    fs::remove_dir_all(&home).ok();
}

/// Threads follow their lines when code moves above them; `r` leaves pending threads alone;
/// a failed GitHub post retries from the same dialog; a shared thread isn't posted again by
/// the sheet's Also publish.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn review_followups(cx: &mut TestAppContext) {
    use review::Status;
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
    let repos = cx.update(|cx| shark.read(cx).state.repos.clone());
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "rf".into(),
            git::on_branch(&repos, "rf"),
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
    let api = home.join("shark/workspaces/rf/api");
    fs::write(api.join("f"), "a\nb\nc\n").unwrap();
    on(cx, &|s, w, cx| s.show(Tab::Diff, w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).diff_files.len() == 1
    })
    .await;
    let ws = cx.update(|cx| shark.read(cx).active.clone().unwrap());
    let chat = cx.update(|cx| shark.read(cx).active_chat().cloned().unwrap());
    let chat_id = cx.update(|cx| chat.read(cx).meta.id.clone());
    let comment =
        |cx: &mut TestAppContext| cx.update(|cx| shark.read(cx).comment(1).cloned().unwrap());

    // A thread sent on "b" (line 2); two lines land above it: it moves to line 4 and the
    // turn's end finds its lines unchanged, not Outdated.
    let c: review::Comment = serde_json::from_str(
        r#"{"id":1,"path":"api/f","start":[false,2],"end":[false,2],"text":"t","snapshot":"b"}"#,
    )
    .unwrap();
    on(cx, &|s, _, cx| {
        let (c, chat_id) = (c.clone(), chat_id.clone());
        s.edit_comments(cx, |l| l.push(c));
        s.set_status(&[1], Status::Sent { turn: 1 }, cx);
        s.edit_comments(cx, |l| l[0].chat = Some(chat_id));
    });
    fs::write(api.join("f"), "x\ny\na\nb\nc\n").unwrap();
    on(cx, &|s, _, cx| s.refresh_diff(cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).comment(1)).is_some_and(|c| c.place() == "api/f:4")
    })
    .await;
    on(cx, &|s, _, cx| s.review_turn_done(&ws, &chat, cx));
    assert_eq!(comment(cx).status, Status::Sent { turn: 1 });
    // Its lines rewritten: Outdated, where it was.
    fs::write(api.join("f"), "x\ny\na\nB\nc\n").unwrap();
    on(cx, &|s, _, cx| s.review_turn_done(&ws, &chat, cx));
    assert_eq!(
        (comment(cx).status, comment(cx).place()),
        (Status::Outdated, "api/f:4".to_string())
    );
    // Re-queued, it quotes the lines as they are now.
    on(cx, &|s, _, cx| s.requeue_outdated(cx));
    assert_eq!(
        (comment(cx).status, comment(cx).snapshot),
        (Status::Pending, "B".to_string())
    );

    // r on a pending thread doesn't hide it: nothing would bring it back.
    on(cx, &|s, w, cx| s.jump_to_comment(1, w, cx));
    on(cx, &|s, _, cx| s.resolve_thread(cx));
    assert_eq!(comment(cx).status, Status::Pending);

    // A PR review fails, then the same dialog's Retry posts it, at the PR's head commit.
    let bin = home.join("gh-bin");
    fs::create_dir_all(&bin).unwrap();
    let (log, fail) = (bin.join("log"), bin.join("fail"));
    let gh = bin.join("gh");
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s|' \"$@\" >> {log}\necho >> {log}\n[ -e {fail} ] && {{ echo 'gh: offline' >&2; exit 1; }}\necho https://github.com/o/api/pull/7#pullrequestreview-5\n",
            log = log.display(),
            fail = fail.display()
        ),
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&gh)
        .status()
        .unwrap();
    let _gh = GhGuard::set(&gh);
    fs::write(&fail, "").unwrap();
    on(cx, &|s, w, cx| {
        s.active_sync()
            .unwrap()
            .update(cx, |g, _| g.set_pr("api", 7, true));
        s.open_share(1, w, cx);
        s.review.share.as_mut().unwrap().review = true;
        s.post_share(w, cx);
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (shark.read(cx).review.share.as_ref()).is_some_and(|s| s.error.is_some())
    })
    .await;
    fs::remove_file(&fail).unwrap();
    on(cx, &|s, w, cx| s.post_share(w, cx));
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).review.share.is_none()
    })
    .await;
    assert!((comment(cx).github).is_some_and(|u| u.ends_with("#pullrequestreview-5")));
    let argv = fs::read_to_string(&log).unwrap();
    assert_eq!(argv.lines().count(), 2, "{argv}");
    assert!(
        argv.lines().all(|l| l.contains("|-f|commit_id=h|")),
        "{argv}"
    );

    // Shared but still pending: the sheet sends it to the chat, and doesn't post it again.
    fs::write(&log, "").unwrap();
    // (Busy, so it queues: no agent process to outlive the test.)
    cx.update(|cx| chat.update(cx, |c, _| c.busy = true));
    on(cx, &|s, w, cx| s.send_review(&[1], "", true, w, cx));
    for _ in 0..10 {
        cx.run_until_parked();
        cx.background_executor
            .timer(Duration::from_millis(20))
            .await;
    }
    assert_eq!(comment(cx).status, Status::Queued);
    assert_eq!(fs::read_to_string(&log).unwrap(), "");

    // A composer with text, then c elsewhere: the draft is added, not lost.
    use crate::diff_view::{DiffLineRange, DiffSide};
    let pick = |cx: &mut TestAppContext, line: usize| {
        on(cx, &|s, w, cx| {
            let r = DiffLineRange::new("api/f", DiffSide::Modified, line, line);
            s.diff.update(cx, |d, cx| d.set_selected_lines(Some(r), cx));
            s.comment_dialog(w, cx);
        })
    };
    pick(cx, 1);
    cx.update_window(window, |_, w, cx| w.input("kept", cx))
        .unwrap();
    pick(cx, 2);
    cx.update(|cx| {
        let s = shark.read(cx);
        assert!(s.ws_comments().iter().any(|c| c.text == "kept"));
        assert_eq!(s.review.composer.as_ref().unwrap().draft.id, 0);
    });
    on(cx, &|s, w, cx| s.cancel_composer(w, cx));
    // A thread on a file gone from the diff is listed below it, unless the diff failed.
    let gone: review::Comment = serde_json::from_str(
        r#"{"id":50,"path":"api/gone","start":[false,1],"end":[false,1],"text":"g"}"#,
    )
    .unwrap();
    on(cx, &|s, _, cx| {
        let gone = gone.clone();
        s.edit_comments(cx, |l| l.push(gone));
        assert!(s.render_orphans(cx).is_some());
        s.diff_error = Some("git failed".into());
        assert!(s.render_orphans(cx).is_none());
        s.diff_error = None;
    });
    fs::remove_dir_all(&home).ok();
}

/// Both mouse buttons open the header's branch menu, which still dismisses and copies.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_menu(cx: &mut TestAppContext) {
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
                size(px(1400.), px(900.)),
            ))),
            ..Default::default()
        },
    );
    let on = on_app(window, shark.clone());
    on(cx, &|s, w, cx| {
        s.create_workspace(
            "branch-menu".into(),
            git::on_branch(&repos, "branch-menu"),
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
    for right in [false, true] {
        for copy in [false, true] {
            cx.update_window(window, |_, w, cx| {
                if right {
                    w.right_click("branch", cx);
                } else {
                    w.click("branch", cx);
                }
            })
            .unwrap();
            cx.update(|cx| assert!(shark.read(cx).menu.is_some(), "right-click: {right}"));
            if copy {
                cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(String::new())));
                press_in(window)(cx, &["c", "enter"]);
                assert_eq!(
                    cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text())),
                    Some("branch-menu".into())
                );
            } else {
                press_in(window)(cx, &["escape"]);
            }
            cx.update(|cx| assert!(shark.read(cx).menu.is_none()));
        }
    }
    fs::remove_dir_all(&home).ok();
}

/// Change base: only the picked repo moves; the state, the root CLAUDE.md, the git button and
/// the branch menu's hint all follow.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn change_base(cx: &mut TestAppContext) {
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
    // api on its own branch.
    let mut picked = git::on_branch(&repos, "rb");
    picked[0].1 = "kd/x".into();
    on(cx, &|s, w, cx| {
        s.create_workspace("rb".into(), picked.clone(), vec![], None, w, cx)
    });
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.is_some()
    })
    .await;
    let api = home.join("shark/workspaces/rb/api");
    git::git(&api, &["update-ref", "refs/remotes/origin/release", "HEAD"]).unwrap();
    assert!(
        (git::remote_branches(&api).iter()).any(|(_, b)| b == "origin/release"),
        "a fetched origin branch is offered"
    );
    let old = cx.update(|cx| shark.read(cx).ws("rb").unwrap().repos[0].base.clone());
    on(cx, &|s, w, cx| {
        let moves = [("api".into(), old.clone(), "origin/release".into())];
        s.set_base("rb", &moves, false, w, cx)
    });
    cx.update(|cx| {
        let s = shark.read(cx);
        let ws = s.ws("rb").unwrap();
        assert_eq!(ws.repos[0].base, "origin/release");
        assert_eq!(ws.repos[1].base, old, "web keeps its base");
        assert_eq!(menus::base_hint(ws), "per repo");
        let (names, bases): (Vec<String>, Vec<String>) = (s.syncs["rb"].read(cx).branches())
            .into_iter()
            .map(|b| (b.name, b.base))
            .unzip();
        assert_eq!(names, ["kd/x", "rb"], "each repo's own branch");
        assert_eq!(bases, ["origin/release".to_string(), old.clone()]);
    });
    let md = fs::read_to_string(home.join("shark/workspaces/rb/CLAUDE.md")).unwrap();
    assert!(md.contains("base `origin/release`"), "{md}");
    fs::remove_dir_all(&home).ok();
}

/// Stacks, end to end on real git: alt-enter on a branch in the New workspace dialog bases the
/// new branch on it; New workspace on top and Insert workspace below from the branch menu; the
/// stack rows open their workspace; a merged parent moves the base and asks the agent to
/// restack after the recorded parent tip.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stacked_workspaces(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let quiet = quiet_claude();
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _claude = ClaudeGuard::set(&quiet);
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let g = |dir: &std::path::Path, args: &[&str]| {
        git::git(
            dir,
            &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
        )
        .unwrap()
    };
    for r in &repos {
        let origin = home.join("origin").join(format!("{}.git", r.name));
        g(
            &home,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &origin.to_string_lossy(),
            ],
        );
        g(
            &r.path,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        g(&r.path, &["push", "-q", "origin", "main"]);
        g(&r.path, &["remote", "set-head", "origin", "main"]);
    }
    let api = repos[0].path.clone();
    g(&api, &["checkout", "-qb", "logic"]);
    g(&api, &["commit", "-q", "--allow-empty", "-m", "logic"]);
    g(&api, &["push", "-q", "origin", "logic"]);
    g(&api, &["checkout", "-q", "main"]);
    let logic = g(&api, &["rev-parse", "logic"]);
    let open = serde_json::json!([{
        "number": 1, "title": "Logic", "author": {"login": "me"}, "headRefName": "logic",
        "baseRefName": "main", "url": "https://github.com/o/api/pull/1", "state": "OPEN",
        "isDraft": false, "isCrossRepository": false,
    }]);
    let data = home.join("open.json");
    fs::write(&data, open.to_string()).unwrap();
    let fake = home.join("gh");
    fs::write(&fake, format!("#!/bin/sh\ncat '{}'\n", data.display())).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let _gh = GhGuard::set(&fake);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(1400.), px(900.)),
            ))),
            ..Default::default()
        },
    );
    let press = press_in(window);
    let on = on_app(window, shark.clone());
    let input = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        cx.run_until_parked();
    };
    let click_branch = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("branch", cx)
        })
        .unwrap();
        cx.run_until_parked();
    };
    let ws = |cx: &mut TestAppContext, name: &str| {
        cx.update(|cx| shark.read(cx).ws(name).cloned().unwrap())
    };
    let head = |name: &str| {
        g(
            &home.join("shark/workspaces").join(name).join("api"),
            &["rev-parse", "HEAD"],
        )
    };

    // alt-enter on a branch: api's new branch starts on `logic` and targets it; web, unticked,
    // isn't in the workspace.
    press(cx, &["cmd-n"]);
    input(cx, "ui");
    press(cx, &["tab", "cmd-b"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_none()
    })
    .await;
    input(cx, "logic");
    press(cx, &["alt-enter"]);
    let chosen = cx
        .update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.find(("repo-branch", 0usize)).label().unwrap().to_string()
        })
        .unwrap();
    assert_eq!(chosen, "api branch: new · on logic");
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("ui")
    })
    .await;
    let ui = ws(cx, "ui");
    assert_eq!(
        (ui.repos.len(), ui.repos[0].base.as_str()),
        (1, "origin/logic")
    );
    assert_eq!(head("ui"), logic);
    assert_eq!(g(&api, &["config", &git::parent_tip_key("ui")]), logic);
    // The header's stack: logic (#1, open) under ui.
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        let st = menus::stacks(s, s.ws("ui").unwrap(), cx);
        st.len() == 1 && st[0].me == 1 && st[0].nodes[0].pr.as_ref().is_some_and(|p| p.number == 1)
    })
    .await;
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(menus::branch_label(s, s.ws("ui").unwrap(), cx), "logic ← ui");
    });

    // New workspace on top, from the branch menu, once ui is pushed.
    let wt = home.join("shark/workspaces/ui/api");
    g(&wt, &["commit", "-q", "--allow-empty", "-m", "ui"]);
    g(&wt, &["push", "-q", "-u", "origin", "ui"]);
    let ui_head = head("ui");
    click_branch(cx);
    press(cx, &["n", "enter"]);
    input(cx, "wiring");
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("wiring")
    })
    .await;
    assert_eq!(ws(cx, "wiring").repos[0].base, "origin/ui");
    assert_eq!(head("wiring"), ui_head);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        let st = menus::stacks(s, s.ws("wiring").unwrap(), cx);
        let names: Vec<&str> = st
            .iter()
            .flat_map(|s| s.nodes.iter().map(|n| n.branch.as_str()))
            .collect();
        names == ["logic", "ui", "wiring"] && st[0].me == 2
    })
    .await;

    // A stack row opens its workspace; "Branch menu…" opens the menu from the keyboard.
    cx.update_window(window, |_, w, cx| w.dispatch_action(Box::new(crate::BranchMenu), cx))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(shark.read(cx).menu.is_some()));
    press(cx, &["u", "enter"]);
    cx.update(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("ui")));

    // Insert workspace below ui: based on logic, pushed, and ui moves on top of it.
    click_branch(cx);
    press(cx, &["i", "enter"]);
    input(cx, "models");
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        s.active.as_deref() == Some("models")
            && s.ws("ui").unwrap().repos[0].base == "origin/models"
    })
    .await;
    assert_eq!(ws(cx, "models").repos[0].base, "origin/logic");
    assert_eq!(head("models"), logic);
    let origin = home.join("origin/api.git");
    assert_eq!(
        g(&origin, &["rev-parse", "models"]),
        logic,
        "pushed for ui's PR to target"
    );
    let md = fs::read_to_string(home.join("shark/workspaces/ui/CLAUDE.md")).unwrap();
    assert!(md.contains("base `origin/models`"), "{md}");

    // ui's PR merged: wiring moves onto its base and the agent restacks after ui's tip.
    let ix = cx.update(|cx| {
        shark
            .read(cx)
            .state
            .workspaces
            .iter()
            .position(|w| w.name == "wiring")
            .unwrap()
    });
    on(cx, &|s, w, cx| s.open_workspace(ix, w, cx));
    let sync = cx.update(|cx| shark.read(cx).syncs["wiring"].clone());
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        (sync.read(cx).repos()).is_some_and(|r| r[0].stacked() == Some("ui"))
    })
    .await;
    let tip = ui_head.clone();
    cx.update(|cx| sync.update(cx, |g, _| g.set_merged_parent("api", 2, &tip, "logic")));
    cx.update(|cx| {
        let a = sync.read(cx).action.clone().unwrap();
        assert_eq!(
            (a.label.as_str(), a.kind),
            ("Parent merged · rebase onto logic", sync::Kind::Restack)
        );
    });
    on(cx, &|s, w, cx| s.with_sync(w, cx, GitSync::primary));
    assert_eq!(ws(cx, "wiring").repos[0].base, "origin/logic");
    let chat = cx.update(|cx| shark.read(cx).active_chat().cloned().unwrap());
    let sent = cx.update(|cx| chat.read(cx).sent_texts());
    let prompt = sent.last().expect("the agent was asked");
    assert!(prompt.contains("Restack `wiring`"), "{prompt}");
    assert!(
        prompt.contains(&format!("api: onto `origin/logic`, after `{ui_head}`")),
        "{prompt}"
    );
    fs::remove_dir_all(&home).ok();
}

/// alt-enter refuses what can't be a PR base (a branch not on origin, a fork's PR); enter
/// (check out, review) replaces a stacked choice; in a two-repo workspace only the stacked repo
/// targets the parent, and the header names that parent.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn branch_from_refusals_and_mixed_repos(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let g = |dir: &std::path::Path, args: &[&str]| {
        git::git(
            dir,
            &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
        )
        .unwrap()
    };
    for r in &repos {
        let origin = home.join("origin").join(format!("{}.git", r.name));
        g(
            &home,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &origin.to_string_lossy(),
            ],
        );
        g(
            &r.path,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        g(&r.path, &["push", "-q", "origin", "main"]);
        g(&r.path, &["remote", "set-head", "origin", "main"]);
    }
    let api = repos[0].path.clone();
    g(&api, &["branch", "local-only"]);
    g(&api, &["branch", "logic"]);
    g(&api, &["push", "-q", "origin", "logic"]);
    let pr = |n: u64, head: &str, fork: bool| {
        serde_json::json!({
            "number": n, "title": format!("PR {n}"), "author": {"login": "c"}, "headRefName": head,
            "baseRefName": "main", "url": format!("https://github.com/o/api/pull/{n}"),
            "state": "OPEN", "isDraft": false, "isCrossRepository": fork,
        })
    };
    let data = home.join("open.json");
    fs::write(
        &data,
        serde_json::json!([pr(3, "logic", true), pr(4, "logic", false)]).to_string(),
    )
    .unwrap();
    let fake = home.join("gh");
    fs::write(&fake, format!("#!/bin/sh\ncat '{}'\n", data.display())).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let _gh = GhGuard::set(&fake);
    let (window, shark) = boot(
        cx,
        State {
            repos: repos.clone(),
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let press = press_in(window);
    let input = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window, |_, w, cx| w.input(text, cx))
            .unwrap();
        cx.run_until_parked();
    };
    let chosen = |cx: &mut TestAppContext, row: usize| {
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.find(("repo-branch", row)).label().unwrap().to_string()
        })
        .unwrap()
    };
    press(cx, &["cmd-n"]);
    input(cx, "mixed");
    press(cx, &["tab", "cmd-b"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_none()
    })
    .await;
    input(cx, "local-only");
    press(cx, &["alt-enter", "tab"]);
    assert_eq!(chosen(cx, 0), "api branch: new", "not on origin");
    press(cx, &["cmd-p"]);
    cx.wait_for(window, Duration::from_secs(10), |w, _| {
        w.try_find("source-status").is_none()
    })
    .await;
    input(cx, "PR 3");
    press(cx, &["alt-enter", "tab"]);
    assert_eq!(chosen(cx, 0), "api branch: new", "a fork's PR");
    press(cx, &["cmd-p"]);
    input(cx, "PR 4");
    press(cx, &["alt-enter"]);
    assert_eq!(chosen(cx, 0), "api branch: new · on logic");
    // Checking out (row 0 is the new branch) drops it; branching from the PR again restores it.
    press(cx, &["tab", "cmd-b", "enter"]);
    assert_eq!(chosen(cx, 0), "api branch: new");
    press(cx, &["cmd-p"]);
    input(cx, "PR 4");
    // Back on the repo list, on api: tick web too.
    press(cx, &["alt-enter", "down", "space"]);
    assert_eq!(chosen(cx, 1), "web branch: new");
    press(cx, &["cmd-enter"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        shark.read(cx).active.as_deref() == Some("mixed")
    })
    .await;
    let ws = cx.update(|cx| shark.read(cx).ws("mixed").cloned().unwrap());
    let bases: Vec<&str> = ws.repos.iter().map(|r| r.base.as_str()).collect();
    assert_eq!(bases, ["origin/logic", "origin/main"]);
    cx.wait_for(window, Duration::from_secs(10), |_, cx| {
        let s = shark.read(cx);
        let st = menus::stacks(s, s.ws("mixed").unwrap(), cx);
        st.len() == 1
            && st[0].repo == "api"
            && st[0].nodes[0].pr.as_ref().is_some_and(|p| p.number == 4)
    })
    .await;
    // web on main beside the stacked api: the header names the parent.
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(menus::branch_label(s, s.ws("mixed").unwrap(), cx), "logic ← mixed");
    });
    fs::remove_dir_all(&home).ok();
}
