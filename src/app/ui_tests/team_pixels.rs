//! The team page, the Goal tab and Home with goals on the real renderer, in both modes.
//! `SHARK_RENDER_TEAMS_ONLY=1 SHARK_RENDER_DIR=<dir> cargo test --features test-support --test rendering`
//! saves a PNG per scene.
use super::team_page::TeamView;
use super::*;
use crate::state::{AgentLink, Goal, GoalState, Row, RowState, Team, Why};

fn row(key: &str, title: &str, agent: &str, after: &[&str], state: RowState) -> Row {
    Row {
        key: key.into(),
        title: title.into(),
        brief: "Keep the existing behavior working.".into(),
        agent: agent.into(),
        repos: vec!["api".into()],
        after: after.iter().map(|k| k.to_string()).collect(),
        state,
        error: (state == RowState::Failed).then(|| "Commit failed".into()),
        ..Default::default()
    }
}

fn goal_task(name: &str, title: &str, goal: Goal) -> Task {
    let mut task = Task {
        name: name.into(),
        title: title.into(),
        branch: name.into(),
        chats: vec![ChatMeta {
            id: name.into(),
            agent: Some("lead".into()),
            plan: true,
            ..Default::default()
        }],
        goal: Some(goal),
        ..Default::default()
    };
    task.normalize_tabs();
    task
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let agents = ["lead", "coder", "designer"].map(|slug| crate::state::Agent {
        slug: slug.into(),
        name: format!("{}{}", &slug[..1].to_uppercase(), &slug[1..]),
        instructions: format!("You are the {slug}."),
        ..Default::default()
    });
    let roster: Vec<AgentLink> = agents.iter().map(AgentLink::from).collect();
    let goal = |state, rows: Vec<Row>| {
        Goal {
        team: "growth".into(),
        brief: "Paginate the Saved list so it loads fast with thousands of items.\nKeep the page-size parameter.\nShip the index with it.\nDocument the cursor format.".into(),
        roster: roster.clone(),
        max_tasks: 8,
        rows,
        state,
        secs: 72 * 60,
        ..Default::default()
    }
    };
    let running = goal(
        GoalState::Running,
        vec![
            row(
                "T1",
                "Add cursor pagination to GET /saved",
                "coder",
                &[],
                RowState::Finished,
            ),
            row(
                "T2",
                "Index saved_items by user and created_at",
                "coder",
                &["T1"],
                RowState::Started,
            ),
            row(
                "T3",
                "Paging states for the Saved list",
                "designer",
                &["T2"],
                RowState::Queued,
            ),
            row(
                "T4",
                "Document the cursor format",
                "designer",
                &[],
                RowState::Failed,
            ),
            row(
                "T5",
                "Remove the old offset parameter",
                "coder",
                &[],
                RowState::Skipped,
            ),
        ],
    );
    let review = Goal {
        draft: vec![row(
            "T1",
            "Audit the export",
            "coder",
            &[],
            RowState::Queued,
        )],
        ..goal(GoalState::Review, vec![])
    };
    let paused = goal(
        GoalState::Paused(Why::Time),
        vec![row(
            "T1",
            "Rename the plan tiers",
            "coder",
            &[],
            RowState::Queued,
        )],
    );
    let done = Goal {
        summary: Some("Both tasks merged. Nothing is left open.".into()),
        ..goal(
            GoalState::Done,
            vec![row(
                "T1",
                "Fix the login redirect",
                "coder",
                &[],
                RowState::Finished,
            )],
        )
    };
    for what in [
        "gave the goal",
        "approved the plan: 5 tasks",
        "started",
        "finished",
    ] {
        crate::team::log(
            "growth",
            &crate::team::Activity {
                at: interaction::now_ms(),
                who: if what.starts_with("st") || what == "finished" {
                    "Coder"
                } else {
                    "You"
                }
                .into(),
                what: what.into(),
                goal: "paginate".into(),
                key: if what.starts_with("st") || what == "finished" {
                    "T1"
                } else {
                    ""
                }
                .into(),
            },
        );
    }
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        let mut state = State {
            agents: agents.to_vec(),
            teams: vec![Team {
                slug: "growth".into(),
                name: "Growth".into(),
                icon: "users".into(),
                lead: "lead".into(),
                agents: vec!["coder".into(), "designer".into()],
                repos: vec!["api".into()],
                ..Default::default()
            }],
            tasks: vec![
                goal_task("paginate", "Paginate the Saved list", running.clone()),
                goal_task("export", "Audit the CSV export", review.clone()),
                goal_task("tiers", "Rename the plan tiers", paused.clone()),
                goal_task("login", "Fix the login redirect", done.clone()),
            ],
            chat_agents_migrated: true,
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
                            size(px(1100.), px(760.)),
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
        type Scene = fn(&mut Shark, &mut Window, &mut Context<Shark>);
        let scenes: [(&str, Scene); 5] = [
            ("team-goals", |s, w, cx| s.open_team("growth", w, cx)),
            ("team-agents", |s, w, cx| {
                s.set_team_view(TeamView::Agents, w, cx)
            }),
            ("team-activity", |s, w, cx| {
                s.set_team_view(TeamView::Activity, w, cx)
            }),
            ("home", |s, w, cx| s.enter_home(w, cx)),
            ("goal-tab", |s, _, cx| {
                // Shown without opening it: the fixture has no worktrees for git to read.
                s.active = Some("paginate".into());
                (s.team_page, s.home.on, s.tab) = (None, false, Tab::Goal);
                cx.notify();
            }),
        ];
        for (name, scene) in scenes {
            cx.update_window(window, |_, w, cx| shark.update(cx, |s, cx| scene(s, w, cx)))
                .unwrap();
            cx.run_until_parked();
            let screenshot = cx.capture_screenshot(window).expect("real renderer");
            assert!(screenshot.width() > 0, "{name} rendered");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!("teams-{mode_name}-{name}.png")))
                    .unwrap();
            }
        }
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
        cx.run_until_parked();
    }
}
