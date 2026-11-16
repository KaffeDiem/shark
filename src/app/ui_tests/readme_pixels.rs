//! The README's screenshots: demo data on the real renderer, without touching the screen.
//! `SHARK_RENDER_README=1 SHARK_RENDER_DIR=docs/images cargo test --features test-support --test rendering`
//! rewrites `readme-*.png`.
use super::*;
use crate::state::{Agent, TaskRepo};

const PATCH: &str = concat!(
    "diff --git a/api/internal/auth/login.go b/api/internal/auth/login.go\n",
    "--- a/api/internal/auth/login.go\n+++ b/api/internal/auth/login.go\n",
    "@@ -14,10 +14,20 @@ func (s *Server) Login(w http.ResponseWriter, r *http.Request) {\n",
    " \t\thttp.Error(w, err.Error(), http.StatusBadRequest)\n \t\treturn\n \t}\n",
    "+\tif !s.limiter.Allow(clientIP(r)) {\n",
    "+\t\tw.Header().Set(\"Retry-After\", \"60\")\n",
    "+\t\thttp.Error(w, \"too many attempts\", http.StatusTooManyRequests)\n",
    "+\t\treturn\n+\t}\n",
    " \tuser, err := s.users.ByEmail(r.Context(), req.Email)\n",
    " \tif err != nil || !user.CheckPassword(req.Password) {\n",
    " \t\thttp.Error(w, ErrInvalidCredentials.Error(), http.StatusUnauthorized)\n",
    " \t\treturn\n \t}\n",
    "-\twriteJSON(w, s.sessions.New(user))\n",
    "+\tsession, err := s.sessions.New(r.Context(), user)\n",
    "+\tif err != nil {\n",
    "+\t\thttp.Error(w, \"could not create session\", http.StatusInternalServerError)\n",
    "+\t\treturn\n+\t}\n",
    "+\twriteJSON(w, session)\n }\n",
    "diff --git a/ios/App/Login/LoginViewModel.swift b/ios/App/Login/LoginViewModel.swift\n",
    "--- a/ios/App/Login/LoginViewModel.swift\n+++ b/ios/App/Login/LoginViewModel.swift\n",
    "@@ -21,7 +21,10 @@ final class LoginViewModel: ObservableObject {\n",
    "     func submit() async {\n         do {\n             session = try await api.login(email, password)\n",
    "-        } catch {\n+        } catch APIError.rateLimited(let retryAfter) {\n",
    "+            message = \"Too many attempts. Try again in \\(retryAfter) seconds.\"\n",
    "+            retryAt = .now + .seconds(retryAfter)\n+        } catch {\n",
    "             message = error.localizedDescription\n         }\n     }\n",
    "diff --git a/ios/App/Networking/APIError.swift b/ios/App/Networking/APIError.swift\n",
    "--- a/ios/App/Networking/APIError.swift\n+++ b/ios/App/Networking/APIError.swift\n",
    "@@ -3,4 +3,5 @@ enum APIError: Error {\n     case invalidCredentials\n     case server(Int)\n",
    "+    case rateLimited(retryAfter: Int)\n     case transport(Error)\n }\n",
);

fn task(home: &std::path::Path, name: &str, title: &str, agent: &str, repos: &[&str]) -> Task {
    let mut task = Task {
        name: name.into(),
        title: title.into(),
        branch: name.into(),
        repos: (repos.iter())
            .map(|r| TaskRepo {
                name: r.to_string(),
                src: home.join(r),
                base: "origin/main".into(),
                branch: None,
            })
            .collect(),
        chats: vec![ChatMeta {
            id: name.into(),
            agent: Some(agent.into()),
            title: title.into(),
            ..Default::default()
        }],
        diff_closed: false,
        ..Default::default()
    };
    task.normalize_tabs();
    task
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let agent = |slug: &str, name: &str, icon: &str| Agent {
        slug: slug.into(),
        name: name.into(),
        icon: icon.into(),
        instructions: format!("You are the {slug}."),
        ..Default::default()
    };
    let agents = vec![
        agent("coder", "Coder", "code"),
        agent("designer", "Designer", "palette"),
    ];
    let mut state = State {
        agents,
        repos: ["api", "web", "ios"]
            .map(|name| Repo {
                name: name.into(),
                path: home.join(name),
            })
            .to_vec(),
        sections: vec![Section {
            name: "Mobile".into(),
            icon: "folder".into(),
            overview: None,
            unsent: None,
            collapsed: false,
            tasks: vec!["login".into(), "push".into()],
        }],
        chat_agents_migrated: true,
        home_checklist_dismissed: true,
        palette_used: true,
        last_seen_version: env!("CARGO_PKG_VERSION").into(),
        ..Default::default()
    };
    state.appearance.mode = crate::state::Mode::Dark;
    state.save().unwrap();
    let (window, shark) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1280.), px(800.)),
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
    cx.run_until_parked();
    // After launch has settled, so no row is taken for one a quit interrupted.
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.state.tasks = vec![
                task(
                    &home,
                    "login",
                    "Rate-limit login and show Retry-After",
                    "coder",
                    &["api", "ios"],
                ),
                task(
                    &home,
                    "push",
                    "Push notification settings screen",
                    "designer",
                    &["ios"],
                ),
                task(
                    &home,
                    "copy",
                    "Shorten the onboarding copy",
                    "designer",
                    &["web"],
                ),
                task(
                    &home,
                    "flaky",
                    "Find the flaky checkout test",
                    "coder",
                    &["web"],
                ),
            ];
            let agents = s.state.agents.clone();
            for task in &mut s.state.tasks {
                task.link_agents(&agents);
            }
            let facts = [
                ("login", (38, 4), 3),
                ("push", (112, 9), 1),
                ("copy", (6, 6), 45),
                ("flaky", (0, 0), 200),
            ];
            for (name, diff, ago) in facts {
                let task = s.task(name).unwrap().clone();
                let repos = (task.repos.iter())
                    .map(|r| {
                        let mut repo = crate::sync::Repo::fresh(r, &task);
                        let local = repo.local.as_mut().unwrap();
                        local.head = "fixture".into();
                        local.branch = Some(task.branch.clone());
                        local.diff = Some(diff);
                        local.changed = u32::from(diff != (0, 0));
                        repo
                    })
                    .collect();
                s.attn.set_checked(name, repos);
                let at = std::time::SystemTime::now() - Duration::from_secs(ago * 60);
                s.home.activity.insert(name.into(), at);
            }
            s.unread.insert("copy".into());
            // One chat at work.
            let ws = s.task("push").unwrap().clone();
            let chat = cx.new(|cx| Chat::new(ws.clone(), ws.chats[0].clone(), false, w, cx));
            chat.update(cx, |c, _| {
                c.busy = true;
                c.started = Some(Instant::now() - Duration::from_secs(95));
            });
            s.chats.insert(ws.chats[0].id.clone(), chat);
            cx.notify();
        })
    })
    .unwrap();
    cx.run_until_parked();

    type Scene = fn(&mut Shark, &mut Window, &mut Context<Shark>);
    let scenes: [(&str, Scene); 3] = [
        ("home", |s, w, cx| s.enter_home(w, cx)),
        // A section's Overview after it started two tasks and messaged one.
        ("overview", |s, w, cx| {
            s.open_overview(0, w, cx);
            let Some(name) = s.active.clone() else {
                return;
            };
            s.state.overview_sent(&name);
            s.home.on = false;
            let title = |n: &str| s.task(n).unwrap().display_title().into_owned();
            let (login, push) = (title("login"), title("push"));
            if let Some(chat) = s.active_chat().cloned() {
                chat.update(cx, |c, cx| {
                    c.demo_overview([("login", &login), ("push", &push)], cx)
                });
            }
            cx.notify();
        }),
        ("diff", |s, w, cx| {
            let ws = s.task("login").unwrap().clone();
            let sync = cx.new(|cx| GitSync::new(ws.clone(), w, cx));
            sync.update(cx, |sync, _| {
                sync.set_on_branch();
                sync.set_merged_parent("api", 1, "head", "main");
                let action = sync.action.as_mut().unwrap();
                action.label = "Commit and push · api, ios".into();
                action.attention = false;
            });
            s.syncs.insert(ws.name.clone(), sync);
            s.active = Some(ws.name.clone());
            (s.home.on, s.tab) = (false, Tab::Feature(Feature::Diff));
            s.configure_differ(cx);
            s.differ.update(cx, |d, cx| {
                *d.show_tree_mut() = true;
                *d.diff_files_mut() = DiffFile::parse(PATCH).unwrap();
                *d.diff_fps_mut() = vec![1; d.diff_files().len()];
                *d.diff_loaded_mut() = true;
                let files = d.diff_files().clone();
                d.diff().update(cx, |state, cx| state.set_files(files, cx));
                d.rebuild_tree();
            });
            cx.notify();
        }),
    ];
    for (name, scene) in scenes {
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                scene(s, w, cx);
                let shown = (s.active.as_ref())
                    .and_then(|a| s.state.tasks.iter().position(|t| &t.name == a));
                if let Some(ix) = shown.filter(|_| !s.home.on) {
                    s.sel = SideRow::Task(ix);
                }
            })
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        let screenshot = cx.capture_screenshot(window).expect("real renderer");
        assert!(screenshot.width() > 0, "{name} rendered");
        if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir).unwrap();
            screenshot
                .save(dir.join(format!("readme-{name}.png")))
                .unwrap();
        }
    }
    cx.update_window(window, |_, w, _| w.remove_window())
        .unwrap();
    cx.run_until_parked();
}
