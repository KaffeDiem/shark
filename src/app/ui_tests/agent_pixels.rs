//! Real-renderer captures of the agent UI, for design review rather than assertions.
//!
//!   SHARK_RENDER_AGENTS_ONLY=1 SHARK_RENDER_DIR=/tmp/agent-shots \
//!     cargo test --features test-support --test rendering
use super::*;
use crate::state::{Agent, AgentLink, TaskRecord, TaskRepo};

type Setup = fn(&mut Shark, &mut Window, &mut Context<Shark>);

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    let link = |slug: &str, name: &str| AgentLink {
        slug: slug.into(),
        name: name.into(),
        ..Default::default()
    };
    let task = |name: &str, title: &str, agent: Option<AgentLink>, repo: &str| Task {
        name: name.into(),
        title: title.into(),
        branch: format!("designer/{name}"),
        agent,
        repos: vec![TaskRepo {
            name: repo.into(),
            src: home.join(repo),
            base: "main".into(),
            branch: None,
        }],
        ..Default::default()
    };
    let designer = Some(link("designer", "Designer"));
    let tasks = vec![
        task(
            "onboarding",
            "Redesign onboarding flow",
            designer.clone(),
            "web",
        ),
        task(
            "tokens",
            "Move colors to theme tokens",
            designer.clone(),
            "web",
        ),
        task("empty-states", "Audit empty states", designer, "api"),
        task("sentry", "Triage Sentry issues", None, "api"),
        task("deps", "Update dependencies", None, "web"),
    ];
    let agents = vec![
        Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            icon: "palette".into(),
            instructions:
                "Always use Mobbin to ground your research.\n\nPrefer theme tokens over raw colors."
                    .into(),
            repos: vec!["web".into()],
            history: vec![
                TaskRecord {
                    title: "Settings page polish".into(),
                    at: 1_730_000_000_000,
                    merged: Some(true),
                    branch: "designer/settings".into(),
                    prs: vec![("web".into(), "https://github.com/acme/web/pull/12".into())],
                },
                TaskRecord {
                    title: "Dark mode contrast".into(),
                    at: 1_729_000_000_000,
                    merged: Some(false),
                    branch: "designer/contrast".into(),
                    prs: vec![],
                },
            ],
            ..Default::default()
        },
        Agent {
            slug: "test".into(),
            name: "test".into(),
            icon: "bot".into(),
            ..Default::default()
        },
    ];
    // Startup refreshes each agent task's CLAUDE.md, which needs the task folder.
    for task in &tasks {
        fs::create_dir_all(task.dir()).unwrap();
    }
    let cases: &[(&str, Setup)] = &[
        ("agent-tasks", |s, w, cx| s.open_agent("designer", w, cx)),
        ("agent-empty", |s, w, cx| s.open_agent("test", w, cx)),
        ("rail", |s, w, cx| {
            s.rail = true;
            s.open_agent("designer", w, cx);
        }),
        ("agent-collapsed", |s, w, cx| {
            s.open_agent("test", w, cx);
            s.set_agent_collapsed(0, true, cx);
        }),
        ("agent-settings", |s, w, cx| s.edit_agent("designer", w, cx)),
        ("agent-rename", |s, w, cx| s.rename_agent("designer", w, cx)),
        ("agent-menu", |s, w, cx| {
            s.open_agent("designer", w, cx);
            let entries = s.agent_menu("designer", cx);
            s.show_menu(point(px(120.), px(250.)), entries, w, cx);
        }),
        ("home", |s, w, cx| s.enter_home(w, cx)),
        ("home-agent", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx)
        }),
        ("home-picked", |s, w, cx| {
            s.enter_home(w, cx);
            s.home
                .prompt
                .update(cx, |p, cx| p.set_value("@designer Tighten spacing", w, cx));
        }),
        ("home-mention", |s, w, cx| {
            s.enter_home(w, cx);
            s.home.prompt.update(cx, |p, cx| {
                p.set_value("@d", w, cx);
                p.focus(w, cx)
            });
        }),
        ("new-agent", |s, w, cx| {
            s.enter_home(w, cx);
            s.new_agent_dialog(w, cx);
        }),
    ];
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for &(case, setup) in cases {
            let mut state = State {
                repos: repos.clone(),
                tasks: tasks.clone(),
                agents: agents.clone(),
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
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    setup(s, w, cx);
                    cx.notify();
                })
            })
            .unwrap();
            cx.run_until_parked();
            cx.update(|cx| cx.set_reduce_motion(true));
            for _ in 0..3 {
                cx.update_window(window, |_, w, cx| w.render_frame(cx))
                    .unwrap();
                cx.run_until_parked();
            }
            let screenshot = cx.capture_screenshot(window).expect("real agent renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!("{case}-{label}.png")))
                    .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    println!("agent pixels: captured {} cases", cases.len() * 2);
}
