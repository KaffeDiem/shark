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
            next: Some("planner".into()),
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
            slug: "planner".into(),
            name: "Planner".into(),
            icon: "bot".into(),
            ..Default::default()
        },
    ];
    // Startup refreshes each agent task's CLAUDE.md, which needs the task folder.
    for task in &tasks {
        fs::create_dir_all(task.dir()).unwrap();
    }
    let cases: &[(&str, Setup)] = &[
        ("agent-next-menu", |s, w, cx| {
            s.open_agent("designer", w, cx);
            s.agent_next_menu("designer", point(px(650.), px(120.)), w, cx);
        }),
        ("handoff-bar", |s, w, cx| {
            s.open_task_now(0, w, cx);
            s.active_chat().unwrap().clone().update(cx, |c, cx| {
                c.play(crate::harness::Ev::User("Design onboarding".into()), cx);
                c.play(
                    crate::harness::Ev::Text(
                        "Use three steps, with a skip link on step two.".into(),
                    ),
                    cx,
                );
                c.busy = false;
                cx.notify();
            });
        }),
        ("handoff-quote", |s, w, cx| s.open_task_now(0, w, cx)),
        ("agent-tasks", |s, w, cx| s.open_agent("designer", w, cx)),
        ("agent-empty", |s, w, cx| s.open_agent("planner", w, cx)),
        ("rail", |s, w, cx| {
            s.rail = true;
            s.open_agent("designer", w, cx);
        }),
        ("agent-collapsed", |s, w, cx| {
            s.open_agent("planner", w, cx);
            s.set_agent_collapsed(0, true, cx);
        }),
        ("agent-settings", |s, w, cx| s.edit_agent("designer", w, cx)),
        ("agent-add-skills", |s, w, cx| {
            s.open_agent("designer", w, cx);
            s.add_agent_skills("designer", w, cx);
        }),
        ("agent-skill-preview", |s, w, cx| {
            s.open_agent("designer", w, cx);
            let source = crate::state::support_dir().join("preview-skills/review");
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("SKILL.md"), "---\nname: review\ndescription: Review a change for correctness and unnecessary complexity.\ncompatibility: Requires git.\n---\n# Review\n\nRead the changed code and its callers.\n\n1. Identify behavior changes.\n2. Check edge cases and tests.\n3. Report actionable findings with file references.\n").unwrap();
            let panel = s.add_agent_skills("designer", w, cx).unwrap();
            panel.update(cx, |p, cx| {
                p.source.update(cx, |input, cx| {
                    input.set_value(source.to_string_lossy().into_owned(), w, cx)
                });
                p.inspect(w, cx);
            });
        }),
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
            if case == "handoff-quote" {
                state.tasks[0].migrate();
                state.tasks[0].chats[0].title = "Onboarding ideas".into();
                state.tasks[0].agent = Some(AgentLink::from(&state.agents[1]));
                let id = state.tasks[0].next_chat_id();
                let mut meta = state.new_chat(&state.tasks[0], id);
                meta.handoff = Some(crate::state::Handoff {
                    from: "Designer".into(),
                    title: "Onboarding ideas".into(),
                    text: "Three-step flow, skip on step 2.\nUse Card and Stepper components.\nKeep keyboard navigation available throughout.".into(),
                });
                let at = state.tasks[0].push_chat(meta);
                state.tasks[0].active_chat = at;
            }
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
