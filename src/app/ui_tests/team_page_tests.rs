//! A team's page: the New team dialog, the composer, the views, pause, limits and the palette.
use super::team_page::TeamView;
use super::team_tests::{Fixture, PLAN, start, team_state};
use super::*;
use crate::state::{Goal, GoalState, State};

fn dialog_open(cx: &mut TestAppContext, f: &Fixture) -> bool {
    cx.update_window(f.window, |_, w, cx| w.has_active_dialog(cx))
        .unwrap()
}

fn press(cx: &mut TestAppContext, f: &Fixture, keys: &[&str]) {
    press_in(f.window)(cx, keys)
}

fn click(cx: &mut TestAppContext, f: &Fixture, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(f.window, |_, w, cx| w.click(id, cx))
        .unwrap();
    cx.run_until_parked();
}

fn view(cx: &mut TestAppContext, f: &Fixture) -> Option<TeamView> {
    f.on(cx, |s, _, _| s.team_page.as_ref().map(|p| p.view))
}

fn exists(cx: &mut TestAppContext, f: &Fixture, id: impl Into<ElementId>) -> bool {
    let id = id.into();
    cx.update_window(f.window, |_, w, _| !w.find_all(id).is_empty())
        .unwrap()
}

/// Boots with the fixture's agents and repository but no team.
fn no_team(cx: &mut TestAppContext) -> Fixture {
    let home = test_home();
    crate::chat::ui_tests::fake_claude();
    let state = State {
        teams: vec![],
        ..team_state(&home, 1)
    };
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.run_until_parked();
    Fixture { window, shark }
}

#[gpui_kit::test]
async fn the_new_team_dialog_makes_a_team_and_shows_its_page(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let f = no_team(cx);
    f.on(cx, |s, w, cx| s.new_team_dialog(w, cx));
    assert!(dialog_open(cx, &f));

    // A refused team keeps the dialog and says why.
    cx.update_window(f.window, |_, w, cx| w.input("Platform", cx))
        .unwrap();
    click(cx, &f, ("new-team-repo", 0usize));
    click(cx, &f, "team-create");
    assert!(dialog_open(cx, &f), "no repository, no team");
    assert!(exists(cx, &f, "new-team-error"));
    assert!(f.on(cx, |s, _, _| s.state.teams.is_empty()));

    // Any agent can lead: the first is picked, the others join, the repository is ticked.
    click(cx, &f, ("new-team-repo", 0usize));
    click(cx, &f, ("new-team-lead", 1usize));
    click(cx, &f, "team-create");
    assert!(!dialog_open(cx, &f));
    f.on(cx, |s, _, _| {
        let t = &s.state.teams[0];
        assert_eq!((t.name.as_str(), t.lead.as_str()), ("Platform", "coder"));
        assert_eq!(t.agents, ["designer"]);
        assert_eq!(t.repos, ["api"]);
        assert_eq!(s.team_shown(), Some("platform"));
    });
}

#[gpui_kit::test]
async fn a_goal_from_the_composer_leaves_the_active_task_alone(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| s.open_team("growth", w, cx));
    assert!(
        f.on(cx, |s, w, cx| s.team_page_focused(w, cx)),
        "keys go to the page"
    );
    assert!(
        exists(cx, &f, "team-empty"),
        "the empty state names the next action"
    );

    // Shift-enter adds a line; enter gives the goal and clears the composer.
    cx.update_window(f.window, |_, w, cx| w.input("Paginate", cx))
        .unwrap();
    press(cx, &f, &["shift-enter"]);
    cx.update_window(f.window, |_, w, cx| w.input("the Saved list", cx))
        .unwrap();
    press(cx, &f, &["enter"]);
    let goal = f.goal(cx).await;
    f.on(cx, |s, _, cx| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(
            (g.team.as_str(), g.brief.as_str()),
            ("growth", "Paginate\nthe Saved list")
        );
        assert_eq!(s.active, None, "giving a goal opens nothing");
        assert_eq!(s.team_shown(), Some("growth"));
        assert_eq!(
            s.team_page
                .as_ref()
                .unwrap()
                .composer
                .read(cx)
                .value()
                .as_ref(),
            ""
        );
    });
}

#[gpui_kit::test]
async fn one_two_three_switch_the_views(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.on(cx, |s, w, cx| s.open_team("growth", w, cx));
    assert_eq!(view(cx, &f), Some(TeamView::Goals));
    assert!(exists(cx, &f, ("team-line", 0usize)), "the goal is listed");
    assert!(f.on(cx, |s, w, cx| s.home.focus.is_focused(w)
        && s.team_page_focused(w, cx)));

    press(cx, &f, &["2"]);
    assert_eq!(view(cx, &f), Some(TeamView::Agents));
    for agent in ["lead", "coder", "designer"] {
        assert!(
            exists(cx, &f, format!("team-agent-{agent}")),
            "{agent} has a card"
        );
    }
    press(cx, &f, &["3"]);
    assert_eq!(view(cx, &f), Some(TeamView::Activity));
    assert!(
        exists(cx, &f, ("team-event", 0usize)),
        "the goal's events are listed"
    );
    press(cx, &f, &["1"]);
    assert_eq!(view(cx, &f), Some(TeamView::Goals));

    // Enter on the cursor opens the goal's task.
    press(cx, &f, &["enter"]);
    f.on(cx, |s, _, _| {
        assert_eq!(s.active.as_deref(), Some(goal.as_str()))
    });
}

#[gpui_kit::test]
async fn pause_and_resume_toggle_the_team(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| s.open_team("growth", w, cx));
    let paused = |cx: &mut TestAppContext| f.on(cx, |s, _, _| s.team("growth").unwrap().paused);
    click(cx, &f, "team-pause");
    assert!(paused(cx));
    f.on(cx, |s, _, cx| {
        assert_eq!(
            super::team_look::team_look_of(s.team_status("growth", cx), cx.theme()).label,
            "Paused"
        )
    });
    click(cx, &f, "team-pause");
    assert!(!paused(cx));

    // The palette's Pause team acts on the page that shows.
    f.on(cx, |s, w, cx| s.pause_team(w, cx));
    assert!(paused(cx));
}

#[gpui_kit::test]
async fn the_limits_popover_edits_the_team(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| s.open_team("growth", w, cx));
    click(cx, &f, "team-limits-chip");
    assert!(exists(cx, &f, "team-limits"));

    click(cx, &f, ("limit-running", 1usize));
    click(cx, &f, ("limit-floor", 1usize));
    click(cx, &f, ("limit-minutes", 0usize));
    click(cx, &f, ("limit-tasks", 1usize));
    f.on(cx, |s, _, _| {
        let l = &s.team("growth").unwrap().limits;
        assert_eq!((l.running, l.floor, l.minutes, l.tasks), (2, 5, 150, 9));
    });
    click(cx, &f, ("limit-running", 0usize));
    f.on(cx, |s, _, _| {
        assert_eq!(s.team("growth").unwrap().limits.running, 1)
    });

    // Escape closes the popover first, and leaves the page where it is.
    press(cx, &f, &["escape"]);
    assert!(!exists(cx, &f, "team-limits"));
    f.on(cx, |s, _, _| assert_eq!(s.team_shown(), Some("growth")));
}

#[gpui_kit::test]
async fn the_approve_menu_and_agent_cards_change_the_team(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        s.open_team("growth", w, cx);
        assert_eq!(s.approve_menu("growth", cx).len(), 2);
        s.update_team("growth", w, cx, |t| t.approve_tasks = true);
        s.set_team_lead("growth", "coder", w, cx);
        assert_eq!(
            s.remove_team_agent("growth", "coder", w, cx),
            Err("Make another agent the lead first")
        );
        assert_eq!(s.remove_team_agent("growth", "lead", w, cx), Ok(()));
        let t = s.team("growth").unwrap();
        assert!(t.approve_tasks);
        assert_eq!(
            (t.lead.as_str(), t.agents.as_slice()),
            ("coder", ["designer".to_string()].as_slice())
        );
        assert_eq!(s.team_menu("growth", cx).len(), 5);
    });
}

#[gpui_kit::test]
async fn palette_commands_exist_and_dispatch(cx: &mut TestAppContext) {
    use super::palette::COMMANDS;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let run = |cx: &mut TestAppContext, label: &str| {
        let row = COMMANDS
            .iter()
            .find(|r| r.0 == label)
            .unwrap_or_else(|| panic!("no row {label}"));
        cx.update_window(f.window, |_, w, cx| {
            f.shark.update(cx, |s, cx| row.run(s, w, cx));
        })
        .unwrap();
        cx.run_until_parked();
    };
    run(cx, "New team…");
    assert!(dialog_open(cx, &f), "New team… opens the dialog");
    press(cx, &f, &["escape"]);
    assert!(!dialog_open(cx, &f));

    run(cx, "Open team…");
    assert!(dialog_open(cx, &f), "Open team… lists the teams");
    press(cx, &f, &["enter"]);
    f.on(cx, |s, _, _| assert_eq!(s.team_shown(), Some("growth")));

    run(cx, "Pause team");
    assert!(f.on(cx, |s, _, _| s.team("growth").unwrap().paused));
    run(cx, "Pause team");
    assert!(
        !f.on(cx, |s, _, _| s.team("growth").unwrap().paused),
        "a paused team resumes"
    );

    f.on(cx, |s, w, cx| s.enter_home(w, cx));
    run(cx, "Give a goal to…");
    press(cx, &f, &["enter"]);
    f.on(cx, |s, w, cx| {
        assert_eq!(s.team_shown(), Some("growth"));
        assert!(
            s.team_page
                .as_ref()
                .unwrap()
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(w)
        );
    });
}

/// An open goal of the Growth team, with no plan rows.
fn open_goal(name: &str) -> Task {
    Task {
        name: name.into(),
        branch: name.into(),
        title: format!("Goal {name}"),
        goal: Some(Goal {
            team: "growth".into(),
            state: GoalState::Running,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[gpui_kit::test]
async fn escape_closes_the_limits_popover_then_leaves_the_page(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        s.state.tasks.push(open_goal("g1"));
        s.new_task(None, None, None, w, cx);
    });
    cx.run_until_parked();
    f.on(cx, |s, w, cx| s.open_team("growth", w, cx));
    click(cx, &f, "team-limits-chip");
    assert!(exists(cx, &f, "team-limits"));
    f.on(cx, |s, w, cx| s.home.focus.focus(w, cx));
    press(cx, &f, &["escape"]);
    assert!(!exists(cx, &f, "team-limits"));
    f.on(cx, |s, _, _| assert_eq!(s.team_shown(), Some("growth")));
    press(cx, &f, &["escape"]);
    f.on(cx, |s, _, _| {
        assert_eq!(s.team_shown(), None, "the second escape leaves the page");
        assert!(s.active.is_some());
    });
}

#[gpui_kit::test]
async fn a_refused_goal_keeps_the_composer_text(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        s.state.agents.retain(|a| a.slug != "lead");
        s.open_team("growth", w, cx);
    });
    cx.update_window(f.window, |_, w, cx| w.input("Paginate", cx))
        .unwrap();
    press(cx, &f, &["enter"]);
    f.on(cx, |s, _, cx| {
        let text = s.team_page.as_ref().unwrap().composer.read(cx).value();
        assert_eq!(
            text.as_ref(),
            "Paginate",
            "no lead, no goal, the text stays"
        );
        assert!(s.state.tasks.is_empty());
    });
}

#[gpui_kit::test]
async fn the_goals_cursor_is_one_line_for_render_enter_down_and_space(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        s.state.tasks.extend([open_goal("g1"), open_goal("g2")]);
        s.open_team("growth", w, cx);
        s.home.sel = Some("not-listed".into());
    });
    let names = f.on(cx, |s, _, cx| {
        let lines = s.team_lines(cx);
        assert_eq!(
            s.team_cursor(&lines),
            Some(0),
            "the first line is the cursor"
        );
        lines.iter().map(|l| l.task.clone()).collect::<Vec<_>>()
    });
    press(cx, &f, &["enter"]);
    f.on(cx, |s, _, _| {
        assert_eq!(s.active.as_deref(), Some(names[0].as_str()))
    });

    f.on(cx, |s, w, cx| {
        s.open_team("growth", w, cx);
        s.home.sel = Some("not-listed".into());
    });
    press(cx, &f, &["down"]);
    f.on(cx, |s, _, _| {
        assert_eq!(s.home.sel.as_deref(), Some(names[1].as_str()))
    });
    f.on(cx, |s, w, cx| {
        s.home.sel = Some("not-listed".into());
        s.home.focus.focus(w, cx);
    });
    press(cx, &f, &["space"]);
    f.on(cx, |s, _, _| {
        assert_eq!(
            s.peek.task.as_deref(),
            Some(names[0].as_str()),
            "space peeks the cursor"
        )
    });
}

#[gpui_kit::test]
async fn a_reopened_page_refreshes_the_activity_log(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        s.open_team("growth", w, cx);
        s.set_team_view(TeamView::Activity, w, cx);
    });
    let before = f.on(cx, |s, _, _| s.team_page.as_ref().unwrap().activity.len());
    crate::team::log(
        "growth",
        &crate::team::Activity {
            at: 1,
            who: "You".into(),
            what: "did a thing".into(),
            goal: String::new(),
            key: String::new(),
        },
    );
    f.on(cx, |s, w, cx| s.open_team("growth", w, cx));
    f.on(cx, |s, _, _| {
        assert_eq!(s.team_page.as_ref().unwrap().activity.len(), before + 1)
    });
}

#[gpui_kit::test]
async fn six_agent_cards_stay_inside_the_column(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        for i in 0..6 {
            let slug = format!("a{i}");
            s.state.agents.push(super::team_tests::agent(&slug));
            s.state.teams[0].agents.push(slug);
        }
        s.open_team("growth", w, cx);
        s.set_team_view(TeamView::Agents, w, cx);
    });
    cx.update_window(f.window, |_, w, cx| {
        w.render_frame(cx);
        let island = w.find("team-agents").bounds();
        for i in 0..6 {
            let card = w.find(format!("team-agent-a{i}")).bounds();
            assert!(
                card.left() >= island.left() && card.right() <= island.right(),
                "a{i}: {card:?} outside {island:?}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
async fn remove_from_team_and_delete_team_say_when_they_cannot_work(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let (_goal, _lead) = f.planned(cx, PLAN).await;
    f.on(cx, |s, _, cx| {
        let has = |s: &Shark, who: &str, cx: &Context<Shark>| {
            let team = s.team("growth").unwrap().clone();
            (s.team_agent_menu(&team, who, cx).iter())
                .any(|e| matches!(e, Entry::Item { label, .. } if label == "Remove from team"))
        };
        assert!(!has(s, "lead", cx), "the lead can't be removed");
        assert!(has(s, "coder", cx));
        s.state.teams[0].agents = vec!["coder".into()];
        assert!(!has(s, "coder", cx), "the last other agent stays");
    });
    f.on(cx, |s, w, cx| s.confirm_delete_team("growth", w, cx));
    assert!(!dialog_open(cx, &f), "an open goal blocks it before asking");
}
