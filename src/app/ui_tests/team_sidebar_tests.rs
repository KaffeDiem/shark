//! Teams in the sidebar and on Home: team rows, goals as rows, a goal's tasks out of sight
//! until one needs you, and Home's agent picker giving a goal.
use super::team_tests::{start, team_state};
use super::*;
use crate::app::sidebar::SideRow;
use crate::attention::Attn;
use crate::state::{Goal, GoalState, Part, Row, RowState};
use gpui_kit::test::TestQueryExt;

fn task(name: &str) -> Task {
    Task {
        name: name.into(),
        branch: name.into(),
        ..Default::default()
    }
}

fn goal(name: &str, state: GoalState) -> Task {
    let rows = ["T1", "T2"].map(|key| Row {
        key: key.into(),
        state: if key == "T1" {
            RowState::Finished
        } else {
            RowState::Queued
        },
        // T1 ran in the task `<goal>-t2` (the one the tests make, where it exists).
        task: (key == "T1").then(|| crate::state::TaskRef {
            name: format!("{name}-t2"),
            instance_id: String::new(),
        }),
        ..Default::default()
    });
    Task {
        title: format!("Goal {name}"),
        goal: Some(Goal {
            team: "growth".into(),
            state,
            rows: rows.to_vec(),
            ..Default::default()
        }),
        ..task(name)
    }
}

fn part(name: &str, of: &str) -> Task {
    Task {
        part: Some(Part {
            goal: of.into(),
            goal_title: format!("Goal {of}"),
            key: "T2".into(),
            after: vec![],
        }),
        ..task(name)
    }
}

/// Tasks 0 plain, 1 goal `g1` (running), 2 its task `g1-t2`, 3 goal `g2` (done), 4 its task.
fn booted(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Shark>) {
    let home = test_home();
    let mut state = team_state(&home, 3);
    // Paused, so the engine starts none of the queued rows.
    state.teams[0].paused = true;
    state.tasks = vec![
        task("plain"),
        goal("g1", GoalState::Running),
        part("g1-t2", "g1"),
        goal("g2", GoalState::Done),
        part("g2-t1", "g2"),
    ];
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    // Loading gave each task an instance id; the row points at its task by it.
    on_app(window, shark.clone())(cx, &|s, _, _| {
        let id = s.state.tasks[2].instance_id.clone();
        let goal = s.state.tasks[1].goal.as_mut().unwrap();
        goal.rows[0].task.as_mut().unwrap().instance_id = id;
    });
    (window, shark)
}

/// A task of `g1` waits on you: a repository it couldn't attach.
fn need_you(s: &mut Shark, name: &str) {
    let results = s.attach_results.entry(name.into()).or_default();
    results.failed.push(("api".into(), "no space".into()));
}

#[gpui_kit::test]
fn teams_and_goals_are_rows_and_fold(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        use SideRow::*;
        // The team, its open goal, then the plain task. A done goal and every task of a goal
        // are out of the visible rows, in no section and not ungrouped.
        assert_eq!(s.side_rows(false), vec![Team(0), Task(1), Task(0)]);
        assert_eq!(
            s.ws_order(false),
            vec![1, 0],
            "cmd-1 and cmd-2 follow visible rows"
        );
        // Everything stays in the list that cmd-j, Home and the palette walk.
        assert_eq!(
            s.side_rows(true),
            vec![Team(0), Task(1), Task(2), Task(3), Task(4), Task(0)]
        );
        assert_eq!(s.ws_order(true), vec![1, 2, 3, 4, 0]);

        // The open task shows, a task of a goal under its goal.
        s.active = Some("g1-t2".into());
        assert_eq!(s.side_rows(false), vec![Team(0), Task(1), Task(2), Task(0)]);
        s.active = Some("g2".into());
        assert_eq!(s.side_rows(false), vec![Team(0), Task(1), Task(3), Task(0)]);
        s.active = None;

        // A goal can't move into a section.
        s.state.sections.push(crate::state::Section {
            name: "Work".into(),
            icon: "folder".into(),
            collapsed: false,
            tasks: vec![],
        });
        s.move_to_section("g1", Some(0), cx);
        assert!(s.state.sections[0].tasks.is_empty());
        assert_eq!(
            s.side_rows(false),
            vec![Team(0), Task(1), Section(0), Task(0)]
        );

        // Folding the team hides its goals and keeps the cursor on the team.
        s.sel = Task(1);
        s.set_team_collapsed(0, true, cx);
        assert!(s.state.teams[0].collapsed);
        assert_eq!(s.side_rows(false), vec![Team(0), Section(0), Task(0)]);
        assert_eq!(s.sel, Team(0));
        assert_eq!(s.ws_order(true).len(), 5, "folded goals stay reachable");
        s.set_team_collapsed(0, false, cx);
        assert_eq!(
            s.side_rows(false),
            vec![Team(0), Task(1), Section(0), Task(0)]
        );

        // The repository filter hides a goal's row, not its team.
        s.sidebar_repo_filter = Some(home::RepoFilter::Repo("api".into()));
        assert!(s.side_rows(false).contains(&Team(0)));
        assert!(
            !s.side_rows(false).contains(&Task(1)),
            "the goal row is hidden"
        );
        s.sidebar_repo_filter = None;
    });

    // The rows are drawn: the team with its status, the goal with its progress.
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find_by_label("Growth \u{b7} Paused").is_some());
        assert!(
            w.try_find_by_label("Goal g1 \u{b7} Running \u{b7} 1 of 2 tasks")
                .is_some()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_goals_task_reaches_home_and_cmd_j_when_it_needs_you(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    let on = on_app(window, shark.clone());
    on(cx, &|s, _, cx| {
        let names = |s: &Shark| {
            let rows = s.home_rows(cx);
            rows.iter()
                .map(|r| s.state.tasks[r.ix].name.clone())
                .collect::<Vec<_>>()
        };
        // Goals are Home rows; their tasks are not, while they need nothing.
        let mut shown = names(s);
        shown.sort();
        assert_eq!(shown, ["g1", "g2", "plain"]);
        assert_eq!(s.home_hidden(cx), (0, 0));

        need_you(s, "g1-t2");
        assert!(matches!(s.attention_of("g1-t2", cx), Attn::Needs(_)));
        let mut shown = names(s);
        shown.sort();
        assert_eq!(shown, ["g1", "g1-t2", "g2", "plain"]);
        // The goal shows what its task needs.
        let rows = s.home_rows(cx);
        let bucket = |name: &str| {
            rows.iter()
                .find(|r| s.state.tasks[r.ix].name == name)
                .unwrap()
                .bucket
        };
        assert_eq!(bucket("g1"), home::Bucket::Needs);
        assert_eq!(bucket("g1-t2"), home::Bucket::Needs);
        // Still out of the sidebar, still in the walk.
        assert!(!s.ws_order(false).contains(&2));
        assert!(s.ws_order(true).contains(&2));

        // The cursor on Home stays on a row that exists when the task stops needing you.
        s.home.sel = Some("g1-t2".into());
        s.attach_results.clear();
        s.fix_home_sel(cx);
        assert_ne!(s.home.sel.as_deref(), Some("g1-t2"));
        need_you(s, "g1-t2");
    });

    // cmd-j opens the hidden task.
    on(cx, &|s, w, cx| s.next_attention(w, cx));
    cx.read(|cx| assert_eq!(shark.read(cx).active.as_deref(), Some("g1-t2")));
}

#[gpui_kit::test]
fn the_cursor_survives_a_team_or_goal_coming_and_going(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (window, shark) = booted(cx);
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        use SideRow::*;
        // A second team appears above the cursor's plain task.
        s.sel = Task(0);
        s.state.teams.push(crate::state::Team {
            slug: "ops".into(),
            name: "Ops".into(),
            lead: "lead".into(),
            ..Default::default()
        });
        s.fix_sel();
        assert_eq!(s.sel, Task(0));
        assert_eq!(s.side_rows(false), vec![Team(0), Task(1), Team(1), Task(0)]);

        // A removed goal under the cursor hands it to the row that moved up into its place.
        s.sel = Task(1);
        let before = s.cursors(cx);
        s.state.tasks.remove(1);
        s.restore_cursors("g1", before, cx);
        assert_eq!(s.sel, Team(1), "the row that moved up into its place");

        // A removed team: the cursor lands on a row that exists. The finished goal left over
        // becomes an ordinary task, so it shows among the rest.
        s.sel = Team(1);
        s.state.teams.remove(1);
        s.fix_sel();
        assert!(s.side_rows(false).contains(&s.sel));
        s.sel = Team(0);
        s.delete_team("growth", cx).unwrap();
        assert!(s.state.teams.is_empty());
        assert!(s.side_rows(false).contains(&s.sel), "{:?}", s.sel);
        assert!(
            s.side_rows(false).iter().all(|r| !matches!(r, Team(_))),
            "no Teams rows without teams"
        );
    });
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn home_picker_with_a_team_gives_a_goal(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let press = press_in(f.window);
    f.on(cx, |s, w, cx| {
        s.enter_home(w, cx);
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("Paginate the Saved list", w, cx))
    });
    cx.update_window(f.window, |_, w, cx| {
        w.render_frame(cx);
        w.click("home-agent-picker", cx);
        w.input("Growth", cx);
    })
    .unwrap();
    press(cx, &["enter"]);
    f.on(cx, |s, _, cx| {
        assert_eq!(s.home.draft_team.as_deref(), Some("growth"));
        assert_eq!(s.home.draft_agent, None);
        assert_eq!(s.home.prompt.read(cx).value(), "Paginate the Saved list");
        // Only the team chip stays: no repositories, model or attachments to leave dead.
        assert_eq!(
            s.draft_chip_stops(),
            vec![super::super::draft_view::DraftStop::Agent]
        );
        assert_eq!(s.draft_stops().len(), 1);
    });
    cx.update_window(f.window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find_by_label("Team Growth").is_some());
        assert!(w.try_find_by_label("Add files").is_none());
    })
    .unwrap();

    // Enter gives the goal. The open task is untouched, and the prompt and the pick clear.
    let before = f.on(cx, |s, _, _| s.active.clone());
    press(cx, &["enter"]);
    let goal = f.goal(cx).await;
    f.on(cx, |s, _, cx| {
        assert_eq!(s.active, before, "a goal opens nothing");
        assert_eq!(s.goal(&goal).unwrap().team, "growth");
        assert_eq!(s.home.draft_team, None);
        assert_eq!(s.home.prompt.read(cx).value(), "");
        assert_eq!(s.state.tasks.len(), 1);
    });

    // Picking an agent drops the team; so does typing an @agent over it.
    f.on(cx, |s, w, cx| {
        s.set_home_team("growth", w, cx);
        assert!(s.draft_for_team());
        s.set_home_agent(Some("coder"), w, cx);
        assert_eq!(s.home.draft_team, None);
        assert_eq!(s.home.draft_agent.as_deref(), Some("coder"));
    });
}

#[gpui_kit::test]
async fn a_deleted_team_frees_homes_prompt(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    f.on(cx, |s, w, cx| {
        s.enter_home(w, cx);
        s.set_home_team("growth", w, cx);
        assert!(s.draft_for_team());
        s.delete_team("growth", cx).unwrap();
        assert!(!s.draft_for_team(), "the team is gone, the prompt is free");
        s.home
            .prompt
            .update(cx, |p, cx| p.set_value("Fix the typo", w, cx));
        s.focus_draft(w, cx);
    });
    cx.update_window(f.window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.try_find_by_label("Team Growth").is_none(),
            "the team chip is gone"
        );
    })
    .unwrap();
    f.on(cx, |s, w, cx| s.submit_prompt(w, cx));
    f.on(cx, |s, _, _| {
        assert!(
            !s.creating.is_empty() || !s.state.tasks.is_empty(),
            "an ordinary task starts"
        )
    });
}
