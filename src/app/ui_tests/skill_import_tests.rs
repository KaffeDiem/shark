use super::*;
use crate::state::{Agent, AgentLink};

fn write_skill(dir: &std::path::Path, name: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Use for layout review.\n---\n# Layout\nRead references/spacing.md before reviewing.\n"),
    )
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn skill_import_skills_entry_and_cancelled_picker_leave_agent_unchanged(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    on_app(window, shark.clone())(cx, &|s, w, cx| s.open_agent("designer", w, cx));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("agent-skills", cx);
    })
    .unwrap();
    press_in(window)(cx, &["a", "enter"]);
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("skill-import").is_some());
        w.click("skill-browse", cx);
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(
            w.has_active_dialog(cx),
            "cancelling the picker leaves the importer open"
        );
        w.render_frame(cx);
        w.click("skill-cancel", cx);
        w.render_frame(cx);
        assert!(w.try_find("skill-import").is_none());
        assert!(
            w.try_find("agent-editor").is_some(),
            "the agent dialog stays under it"
        );
        assert!(!crate::state::agent_skills_dir("designer").exists());
        assert!(shark.read(cx).state.agents[0].instructions.is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn skill_import_copies_selected_resources_and_refreshes_only_its_agent(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let source = home.join("skill-source");
    let spacing = source.join("spacing");
    write_skill(&spacing, "spacing");
    write_skill(&source.join("unused"), "unused");
    // A long preview must not squeeze the selection controls out of the dialog.
    fs::write(
        spacing.join("SKILL.md"),
        format!(
            "---\nname: spacing\ndescription: {}\n---\n{}",
            "Review layout, spacing, typography and accessibility before making changes. "
                .repeat(6),
            "## Review\n\nRead references/spacing.md before reviewing.\n\n".repeat(40),
        ),
    )
    .unwrap();
    fs::create_dir_all(source.join(".claude-plugin")).unwrap();
    fs::write(source.join(".claude-plugin/plugin.json"), "{}").unwrap();
    fs::create_dir_all(spacing.join("references")).unwrap();
    fs::write(spacing.join("references/spacing.md"), "Use an 8 px grid.").unwrap();
    let agents: Vec<Agent> = ["designer", "writer"]
        .into_iter()
        .map(|slug| Agent {
            slug: slug.into(),
            name: slug.into(),
            ..Default::default()
        })
        .collect();
    let tasks: Vec<Task> = agents
        .iter()
        .map(|a| {
            git::create_empty_task(
                &tasks_dir().join(&a.slug),
                &a.slug,
                &a.name,
                Some(AgentLink {
                    slug: a.slug.clone(),
                    name: a.name.clone(),
                    ..Default::default()
                }),
            )
            .unwrap()
        })
        .collect();
    let (window, shark) = boot(
        cx,
        State {
            agents,
            tasks,
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let panel = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.open_agent("designer", w, cx);
                s.add_agent_skills("designer", w, cx).unwrap()
            })
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        panel.update(cx, |p, cx| {
            p.source.update(cx, |input, cx| {
                input.set_value(source.to_string_lossy().into_owned(), w, cx)
            });
            p.inspect(w, cx);
        });
    })
    .unwrap();
    until(cx, window, |_, cx| !panel.read(cx).busy).await;
    cx.update_window(window, |_, w, cx| {
        let p = panel.read(cx);
        assert!(p.error.is_none(), "{:?}", p.error);
        let imported = p.prepared.as_ref().expect("skills are previewed");
        assert_eq!(
            imported
                .skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["spacing", "unused"]
        );
        let selected = p.selected.clone();
        w.render_frame(cx);
        assert!(w.try_find("skill-instructions").is_some());
        assert!(
            w.find(("skill-choice", 0usize)).visible(),
            "skill selection remains visible with a long preview"
        );
        if !selected[0] {
            w.click(("skill-choice", 0usize), cx);
        }
        if selected[1] {
            w.click(("skill-choice", 1usize), cx);
        }
        w.render_frame(cx);
        let add_bounds = w.find("skill-add").bounds();
        let choices_top = w.find("skill-choices").bounds().top();
        w.scroll(
            ("skill-choice", 0usize),
            ScrollDelta::Pixels(point(px(0.), px(-200.))),
            cx,
        );
        w.render_frame(cx);
        assert!(
            w.find("skill-choices").bounds().top() < choices_top,
            "scrolling over the skill list moves the dialog body"
        );
        assert_eq!(w.find("skill-add").bounds(), add_bounds);
        let preview_top = w.find("skill-preview").bounds().top();
        let instructions_top = w.find("skill-instructions").bounds().top();
        w.scroll(
            "skill-preview-heading",
            ScrollDelta::Pixels(point(px(0.), px(-100.))),
            cx,
        );
        w.render_frame(cx);
        let moved = preview_top - w.find("skill-preview").bounds().top();
        assert!(moved > px(0.), "the preview scrolls with the dialog body");
        assert_eq!(
            instructions_top - w.find("skill-instructions").bounds().top(),
            moved,
            "preview instructions do not scroll independently"
        );
        assert_eq!(w.find("skill-add").bounds(), add_bounds);
        assert!(w.find("skill-add").visible());
        w.click("skill-add", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| !panel.read(cx).busy).await;
    cx.update_window(window, |_, w, cx| {
        assert!(panel.read(cx).error.is_none(), "{:?}", panel.read(cx).error);
        w.render_frame(cx);
        assert!(w.try_find("skill-import").is_none());
        assert!(w.try_find("agent-editor").is_some());
        let target = crate::state::agent_skills_dir("designer").join("spacing");
        assert_eq!(
            fs::read_to_string(target.join("references/spacing.md")).unwrap(),
            "Use an 8 px grid."
        );
        assert!(
            !crate::state::agent_skills_dir("designer")
                .join("unused")
                .exists()
        );
        for folder in [".agents/skills", ".claude/skills"] {
            assert_eq!(
                fs::read_link(tasks_dir().join("designer").join(folder).join("spacing")).unwrap(),
                target
            );
            assert!(
                !tasks_dir()
                    .join("writer")
                    .join(folder)
                    .join("spacing")
                    .exists()
            );
        }
        let editor = shark
            .read(cx)
            .agent_editor
            .clone()
            .unwrap()
            .upgrade()
            .unwrap();
        assert_eq!(editor.read(cx).skills, ["spacing"]);
        assert!(spacing.join("SKILL.md").exists(), "the source is kept");
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn skill_import_collision_preserves_existing_skill_and_keeps_error_visible(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let source = home.join("incoming/review");
    write_skill(&source, "review");
    let existing = crate::state::agent_skills_dir("designer").join("review");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("SKILL.md"), "My locally edited review skill").unwrap();
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                Point::default(),
                size(px(480.), px(400.)),
            ))),
            ..Default::default()
        },
    );
    let panel = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.add_agent_skills("designer", w, cx).unwrap())
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        panel.update(cx, |p, cx| {
            p.source.update(cx, |input, cx| {
                input.set_value(source.to_string_lossy().into_owned(), w, cx)
            });
            p.inspect(w, cx);
        });
    })
    .unwrap();
    until(cx, window, |_, cx| !panel.read(cx).busy).await;
    cx.update_window(window, |_, w, cx| {
        assert!(
            panel.read(cx).prepared.is_some(),
            "preview succeeds before conflict check"
        );
        let selected = panel.read(cx).selected[0];
        w.render_frame(cx);
        if !selected {
            w.click(("skill-choice", 0usize), cx);
        }
        w.render_frame(cx);
        w.click("skill-add", cx);
    })
    .unwrap();
    until(cx, window, |_, cx| !panel.read(cx).busy).await;
    cx.update_window(window, |_, w, cx| {
        assert!(panel.read(cx).error.is_some());
        w.render_frame(cx);
        assert!(w.find("skill-import-error").visible());
        assert!(w.find("skill-cancel").visible());
        assert!(w.find("skill-add").bounds().bottom() <= px(400.));
        assert!(w.has_active_dialog(cx), "failure remains inspectable");
        assert_eq!(
            fs::read_to_string(existing.join("SKILL.md")).unwrap(),
            "My locally edited review skill"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn skill_import_long_error_keeps_actions_visible(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let source = home.join("invalid-skill");
    write_skill(&source, &"invalid skill name ".repeat(100));
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                Point::default(),
                size(px(480.), px(400.)),
            ))),
            ..Default::default()
        },
    );
    let panel = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.add_agent_skills("designer", w, cx).unwrap())
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        panel.update(cx, |p, cx| {
            p.source.update(cx, |input, cx| {
                input.set_value(source.to_string_lossy().into_owned(), w, cx)
            });
            p.inspect(w, cx);
        });
    })
    .unwrap();
    until(cx, window, |_, cx| !panel.read(cx).busy).await;
    cx.update_window(window, |_, w, cx| {
        assert!(
            panel
                .read(cx)
                .error
                .as_ref()
                .unwrap()
                .contains("Invalid skill name")
        );
        w.render_frame(cx);
        for id in ["skill-import-error", "skill-cancel", "skill-add"] {
            let control = w.find(id);
            assert!(control.visible(), "{id} must remain visible");
            assert!(control.bounds().top() >= px(0.));
            assert!(
                control.bounds().bottom() <= px(400.),
                "{id} must fit in the window"
            );
        }
        let add_bounds = w.find("skill-add").bounds();
        w.scroll(
            "skill-import-error",
            ScrollDelta::Pixels(point(px(0.), px(-10000.))),
            cx,
        );
        w.render_frame(cx);
        assert!(
            w.find("skill-import-error-details").bounds().bottom()
                <= w.find("skill-import-error").bounds().bottom() + px(1.),
            "the end of the diagnostic remains reachable"
        );
        assert_eq!(w.find("skill-add").bounds(), add_bounds);
        w.click("skill-cancel", cx);
        assert!(!w.has_active_dialog(cx));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn skill_import_changing_source_requires_a_new_preview(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let source = home.join("incoming/review");
    write_skill(&source, "review");
    let (window, shark) = boot(
        cx,
        State {
            agents: vec![Agent {
                slug: "designer".into(),
                name: "Designer".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        WindowOptions::default(),
    );
    let panel = cx
        .update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| s.add_agent_skills("designer", w, cx).unwrap())
        })
        .unwrap();
    cx.update_window(window, |_, w, cx| {
        panel.update(cx, |p, cx| {
            p.source.update(cx, |input, cx| {
                input.set_value(source.to_string_lossy().into_owned(), w, cx)
            });
            p.inspect(w, cx);
        });
    })
    .unwrap();
    until(cx, window, |_, cx| !panel.read(cx).busy).await;
    cx.update_window(window, |_, w, cx| {
        assert!(panel.read(cx).prepared.is_some());
        panel
            .read(cx)
            .source
            .clone()
            .update(cx, |input, cx| input.set_value("/a/different/skill", w, cx));
        // Adding before the queued input-change event still cannot install the old preview.
        panel.update(cx, |p, cx| p.install(w, cx));
        assert!(panel.read(cx).prepared.is_none());
        assert!(
            !panel.read(cx).busy,
            "adding requires the current source to be inspected"
        );
        assert!(!crate::state::agent_skills_dir("designer").exists());
    })
    .unwrap();
}
