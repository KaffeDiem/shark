//! Real-renderer captures of the agent UI, for design review rather than assertions.
//!
//!   SHARK_RENDER_AGENTS_ONLY=1 SHARK_RENDER_DIR=/tmp/agent-shots \
//!     cargo test --features test-support --test rendering
use super::*;
use crate::state::{Agent, AgentLink, TaskRepo};

type Setup = fn(&mut Shark, &mut Window, &mut Context<Shark>);

fn draft_issue() -> crate::linear::Snapshot {
    crate::linear::Snapshot {
        org: "acme".into(),
        id: "issue-412".into(),
        identifier: "ENG-412".into(),
        title: "Skills can't be added".into(),
        description: "Keep the selected skill when changing repositories.".into(),
        ..Default::default()
    }
}

fn avatar_identities(s: &mut Shark) {
    use crate::state::AgentIcon;
    s.state.agents[0].icon = AgentIcon::Symbol {
        name: "palette".into(),
        tint: Some("blue".into()),
    };
    let photo = image::RgbaImage::from_fn(96, 96, |x, y| {
        image::Rgba([80 + x as u8, 110 + y as u8, 160, 255])
    });
    s.state.agents[1].icon = crate::agent_icon::save_image("planner", &photo).unwrap();
    s.state.agents.push(Agent {
        slug: "reviewer".into(),
        name: "Reviewer".into(),
        icon: AgentIcon::Emoji {
            value: "🐧".into()
        },
        ..Default::default()
    });
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api", "web"]);
    fs::create_dir_all(repos[1].path.join("src")).unwrap();
    fs::write(
        repos[1].path.join("src/button.ts"),
        "export const Button = {};\n",
    )
    .unwrap();
    fs::write(
        repos[1].path.join("src/card.ts"),
        "export const Card = {};\n",
    )
    .unwrap();
    commit_all(&repos[1].path, "Composer screenshot files");
    let key = crate::files::fingerprint(repos[1].path.as_os_str().as_encoded_bytes());
    crate::state::write(&crate::state::support_dir().join("prs").join(format!("{key:016x}-recent.json")),
        br#"[{"number":89,"title":"Preserve selected skills","author":{"login":"ada"},"headRefName":"fix-skills","baseRefName":"main","url":"https://github.com/acme/web/pull/89","state":"OPEN","isDraft":false}]"#).unwrap();
    let link = |slug: &str, name: &str| AgentLink {
        slug: slug.into(),
        name: name.into(),
        ..Default::default()
    };
    // Loading gives each task a first chat on its agent.
    let task = |name: &str, title: &str, agent: Option<AgentLink>, repo: &str| Task {
        name: name.into(),
        title: title.into(),
        branch: format!("designer/{name}"),
        agents: agent.into_iter().collect(),
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
        ("agent-icon-picker-symbols", |s, w, cx| {
            s.pick_agent_icon("designer", w, cx);
        }),
        ("agent-icon-picker-emoji", |s, w, cx| {
            s.state.agents[0].icon = crate::state::AgentIcon::Emoji {
                value: "🐧".into()
            };
            s.pick_agent_icon("designer", w, cx);
        }),
        ("agent-icon-picker-image", |s, w, cx| {
            let image = image::RgbaImage::from_fn(120, 180, |x, y| {
                image::Rgba([x as u8, y as u8, 140, 255])
            });
            let path = crate::state::support_dir().join("crop-preview.png");
            image.save(&path).unwrap();
            s.pick_agent_icon("designer", w, cx);
            s.icon_picker
                .as_ref()
                .unwrap()
                .picker
                .clone()
                .update(cx, |p, cx| p.load_path(&path, false, w, cx));
        }),
        ("agent-identities-symbol", |s, w, cx| {
            avatar_identities(s);
            s.open_agent("designer", w, cx);
        }),
        ("agent-identities-photo", |s, w, cx| {
            avatar_identities(s);
            s.open_agent("planner", w, cx);
        }),
        ("agent-identities-emoji", |s, w, cx| {
            avatar_identities(s);
            s.open_agent("reviewer", w, cx);
        }),
        // Home's composer with an agent picked: `prefill_agent_task` leaves `@slug ` typed, so
        // text goes in after it.
        ("draft-file-completion", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home
                .prompt
                .update(cx, |p, cx| p.insert("Review @", w, cx));
        }),
        ("draft-pr-completion", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home
                .prompt
                .update(cx, |p, cx| p.insert("Review #89", w, cx));
        }),
        ("draft-issue-pr", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.draft.issues.push(draft_issue());
            s.home.draft.repos[0].pr_label = Some("#89 Preserve selected skills · Open".into());
            s.home.draft.repos[0].branch =
                super::super::task_draft::BranchSpec::Pr(git::PullRequest {
                    number: 89,
                    url: "https://github.com/acme/web/pull/89".into(),
                    base: "main".into(),
                });
            s.home.prompt.update(cx, |p, cx| p.insert("Keep the selected skill when changing repositories.\nCheck the keyboard flow too.", w, cx));
        }),
        ("draft-issue-plan", |s, w, cx| {
            s.enter_home(w, cx);
            s.home.draft.issues.push(draft_issue());
            s.focus_draft(w, cx);
        }),
        ("draft-model-override", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.draft.model = Some("sonnet".into());
            s.home.draft.effort = Some("medium".into());
            s.home.prompt.update(cx, |p, cx| {
                p.insert("Tighten the onboarding spacing", w, cx)
            });
        }),
        ("draft-composer-narrow", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.draft.issues.push(draft_issue());
            s.home.draft.files = vec![
                PathBuf::from("/tmp/design-review-notes.md"),
                PathBuf::from("/tmp/long-composer-layout-reference.png"),
            ];
            s.home.prompt.update(cx, |p, cx| {
                p.insert("Keep every attachment reachable in a narrow window", w, cx)
            });
        }),
        ("draft-composer-narrow-edited", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.draft.model = Some("long-custom-model-name".into());
            s.home.draft.issues.push(draft_issue());
        }),
        ("draft-removed-default", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.draft.repos.clear();
            s.home.draft.repos_edited = true;
        }),
        ("draft-starting", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.prompt.update(cx, |p, cx| {
                p.insert("Tighten the onboarding spacing", w, cx)
            });
            s.home.draft.starting = true;
            s.home
                .composer
                .update(cx, |c, cx| c.set_read_only(true, cx));
        }),
        ("draft-no-repositories", |s, w, cx| {
            s.state.repos.clear();
            s.prefill_agent_task("planner", w, cx);
        }),
        ("draft-repository-settings", |s, w, cx| {
            s.state.repos.clear();
            s.prefill_agent_task("planner", w, cx);
            crate::settings::open_repos(&s.state, w, cx);
        }),
        ("draft-branch-fetch-failure", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx);
            s.home.prompt.update(cx, |p, cx| {
                p.insert("Keep these notes if refreshing fails", w, cx)
            });
            s.pick_draft_branch(Some(("web".into(), false)), w, cx);
        }),
        ("home-ask", |s, w, cx| {
            s.enter_home(w, cx);
            s.toggle_ask_mode(w, cx);
            s.home.prompt.update(cx, |p, cx| {
                p.set_value("How do I attach a repository?", w, cx);
                p.focus(w, cx);
            });
        }),
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
        // The agent page is its settings header only.
        ("agent-page", |s, w, cx| s.open_agent("designer", w, cx)),
        ("rail", |s, w, cx| {
            s.rail = true;
            s.open_agent("designer", w, cx);
        }),
        ("agent-settings", |s, w, cx| s.edit_agent("designer", w, cx)),
        ("agent-add-skills", |s, w, cx| {
            s.open_agent("designer", w, cx);
            s.add_agent_skills("designer", w, cx);
        }),
        ("agent-skill-preview", |s, w, cx| {
            s.open_agent("designer", w, cx);
            let source = crate::state::support_dir().join("preview-skills");
            fs::create_dir_all(source.join(".claude-plugin")).unwrap();
            for name in ["review", "spacing"] {
                let skill = source.join(name);
                fs::create_dir_all(&skill).unwrap();
                fs::write(skill.join("SKILL.md"), format!(
                    "---\nname: {name}\ndescription: Review a change for correctness and unnecessary complexity.\ncompatibility: Requires git.\n---\n{}",
                    "# Review\n\nRead the changed code and its callers.\n\n1. Identify behavior changes.\n2. Check edge cases and tests.\n3. Report actionable findings with file references.\n\n".repeat(8),
                )).unwrap();
            }
            let panel = s.add_agent_skills("designer", w, cx).unwrap();
            panel.update(cx, |p, cx| {
                p.source.update(cx, |input, cx| {
                    input.set_value(source.to_string_lossy().into_owned(), w, cx)
                });
                p.inspect(w, cx);
            });
        }),
        ("agent-rename", |s, w, cx| s.rename_agent("designer", w, cx)),
        ("home", |s, w, cx| s.enter_home(w, cx)),
        ("home-agent", |s, w, cx| {
            s.prefill_agent_task("designer", w, cx)
        }),
        ("home-picked", |s, w, cx| {
            s.enter_home(w, cx);
            s.home
                .prompt
                .update(cx, |p, cx| p.insert("@designer Tighten spacing", w, cx));
        }),
        ("home-mention", |s, w, cx| {
            s.enter_home(w, cx);
            s.home.prompt.update(cx, |p, cx| {
                p.insert("@d", w, cx);
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
                                size(
                                    px(
                                        if matches!(
                                            case,
                                            "draft-composer-narrow"
                                                | "draft-composer-narrow-edited"
                                        ) {
                                            800.
                                        } else {
                                            1280.
                                        },
                                    ),
                                    px(800.),
                                ),
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
            if case == "agent-page" {
                cx.update_window(window, |_, w, _| {
                    assert!(w.try_find("agent-settings").is_some());
                    assert!(
                        w.try_find("home-composer").is_none() && w.try_find("home-list").is_none()
                    );
                })
                .unwrap();
            }
            if matches!(case, "draft-file-completion" | "draft-pr-completion") {
                cx.update(|cx| {
                    assert!(
                        shark.read(cx).home.composer.read(cx).has_menu(),
                        "{case} must show its completion menu"
                    )
                });
            }
            let screenshot = cx.capture_screenshot(window).expect("real agent renderer");
            if case == "agent-icon-picker-image" {
                let (bounds, viewport) = cx
                    .update_window(window, |_, w, _| {
                        (w.find("agent-icon-crop").bounds(), w.viewport_size().width)
                    })
                    .unwrap();
                let scale = screenshot.width() as f32 / f32::from(viewport);
                let pixel = |x: Pixels, y: Pixels| {
                    screenshot
                        .get_pixel((f32::from(x) * scale) as u32, (f32::from(y) * scale) as u32)
                        .0
                };
                let corner = pixel(bounds.left() + px(2.), bounds.top() + px(2.));
                let outside = pixel(bounds.left() - px(2.), bounds.top() + px(2.));
                assert!(
                    corner.iter().zip(outside).all(|(a, b)| a.abs_diff(b) <= 3),
                    "crop preview must have circular transparent corners: {corner:?} vs {outside:?}"
                );
                assert_ne!(pixel(bounds.center().x, bounds.center().y), outside);
            }
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
