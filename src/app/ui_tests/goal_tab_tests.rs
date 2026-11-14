//! The Goal tab, the goal chip and the plan rows of a goal's plan pane.
use super::team_tests::{Fixture, PLAN, start};
use super::*;
use crate::state::{GoalState, RowState, Why};
use gpui_kit::test::TestQueryExt as _;

const THREE: &str = "# Paginate\n\nProse.\n\n## Tasks\n\
- T1 @coder [api]: Add cursor pagination\n\
- T2 @coder [api] after T1: Index saved items\n\
- T3 @designer [api]: Paging states\n";

impl Fixture {
    fn frame(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }

    fn has(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) -> bool {
        self.frame(cx);
        let id = id.into();
        cx.update_window(self.window, |_, w, _| w.try_find(id).is_some())
            .unwrap()
    }

    fn click(&self, cx: &mut TestAppContext, id: impl Into<ElementId>) {
        self.frame(cx);
        let id = id.into();
        // A toast from the lead's finished turn can sit over the header buttons.
        cx.update_window(self.window, |_, w, cx| w.clear_notifications(cx))
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        self.frame(cx);
        cx.update_window(self.window, |_, w, cx| w.click(id, cx))
            .unwrap();
        cx.run_until_parked();
    }

    fn keys(&self, cx: &mut TestAppContext, keys: &[&str]) {
        press_in(self.window)(cx, keys);
    }

    /// Approves the draft and opens the goal task on its Goal tab.
    fn started(&self, cx: &mut TestAppContext, goal: &str) {
        self.on(cx, |s, w, cx| {
            s.approve_goal(goal, w, cx);
            s.open_task_named(goal, w, cx);
        });
        self.frame(cx);
    }

    fn goal_state(&self, cx: &mut TestAppContext, goal: &str, set: GoalState) {
        self.on(cx, |s, _, cx| {
            s.task_mut(goal).unwrap().goal.as_mut().unwrap().state = set;
            cx.notify();
        });
    }

    fn rows(&self, cx: &mut TestAppContext, goal: &str) -> Vec<(String, RowState, String)> {
        self.on(cx, |s, _, _| {
            (s.goal(goal).unwrap().rows.iter())
                .map(|r| (r.key.clone(), r.state, r.agent.clone()))
                .collect()
        })
    }
}

/// A goal opens on its Goal tab, first in the strip and never closed; the rows list by status
/// and Retry, Start now, Reassign and Remove act on the goal.
#[gpui_kit::test]
async fn goal_tab_lists_rows_and_acts_on_them(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let (goal, _lead) = f.planned(cx, THREE).await;
    f.started(cx, &goal);
    f.on(cx, |s, _, _| {
        assert_eq!(s.tab, Tab::Goal, "a goal opens on its plan")
    });
    assert!(f.has(cx, "goal-tab"), "the strip has the Goal tab");

    // Running first, then queued in plan order.
    f.on(cx, |s, _, cx| {
        let order: Vec<_> = (s.goal_list(&goal, cx).into_iter())
            .map(|(r, _)| r.key)
            .collect();
        assert_eq!(order, ["T1", "T2", "T3"]);
        assert_eq!(s.tab, Tab::Goal);
    });
    for key in ["T1", "T2", "T3"] {
        assert!(f.has(cx, format!("goal-row-{key}")), "{key}");
    }
    assert!(!f.has(cx, "goal-chip"), "a goal has no chip");

    // The Goal tab has no close button: closing the tab leaves it and its chats.
    f.keys(cx, &["cmd-w"]);
    f.on(cx, |s, _, _| {
        assert_eq!(s.active.as_deref(), Some(goal.as_str()));
        assert_eq!(s.active_task().unwrap().chats.len(), 1);
    });

    // Reassign from the keyboard: a lists the team, the first row is the cursor's.
    f.keys(cx, &["down", "a"]);
    f.keys(cx, &["down", "enter"]);
    let rows = f.rows(cx, &goal);
    assert_eq!(rows[1].0, "T2");
    assert_eq!(rows[1].2, "designer", "the cursor was on T2");

    // Start now, by the row's own button: T3 runs although the limit is one.
    f.click(cx, "goal-s-T3");
    assert_eq!(f.rows(cx, &goal)[2].1, RowState::Started);

    // Stuck rows come first and Retry puts them back in the queue.
    f.on(cx, |s, _, cx| {
        let row = &mut s.task_mut(&goal).unwrap().goal.as_mut().unwrap().rows[1];
        (row.state, row.error) = (RowState::Failed, Some("Setup failed".into()));
        cx.notify();
    });
    f.on(cx, |s, _, cx| {
        let first = s.goal_list(&goal, cx).remove(0);
        assert_eq!(first.0.key, "T2", "Needs you comes first");
    });
    f.click(cx, "goal-r-T2");
    assert_eq!(f.rows(cx, &goal)[1].1, RowState::Queued);

    // Remove from plan, by key: a queued row goes and T1 and T3 stay.
    f.on(cx, |s, w, cx| s.goal_focus.focus(w, cx));
    f.keys(cx, &["x"]);
    let keys: Vec<_> = f.rows(cx, &goal).into_iter().map(|r| r.0).collect();
    assert_eq!(keys, ["T1", "T3"]);
}

/// The paused banner: Add 1 h carries on, Keep paused leaves the goal to Resume, and the header
/// pauses.
#[gpui_kit::test]
async fn goal_tab_paused_banner_actions(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.started(cx, &goal);
    assert!(!f.has(cx, "goal-banner"));

    f.click(cx, "goal-pause");
    f.on(cx, |s, _, _| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Paused(Why::You));
    });
    assert!(f.has(cx, "goal-banner"), "paused by you");
    f.click(cx, "goal-banner-resume");
    f.on(cx, |s, _, _| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Running)
    });

    f.goal_state(cx, &goal, GoalState::Paused(Why::Time));
    assert!(f.has(cx, "goal-add-hour") && f.has(cx, "goal-keep-paused"));
    f.click(cx, "goal-keep-paused");
    f.on(cx, |s, _, _| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Paused(Why::You));
    });
    f.goal_state(cx, &goal, GoalState::Paused(Why::Time));
    f.click(cx, "goal-add-hour");
    f.on(cx, |s, _, _| {
        let g = s.goal(&goal).unwrap();
        assert_eq!((g.state, g.extra_minutes), (GoalState::Running, 60));
    });
}

/// Under the stricter approval a finished task with nothing to merge still waits for its
/// review: Mark reviewed on the tab lets the task after it start.
#[gpui_kit::test]
async fn goal_tab_mark_reviewed_starts_the_next_task(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.started(cx, &goal);
    let t1 = f.row_chat(cx, &goal, "T1").await;
    // T1 began as a plain task; the stricter approval holds the task after it.
    f.on(cx, |s, _, _| s.state.teams[0].approve_tasks = true);
    f.end_turn(cx, &t1, "Nothing to change.", 5);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    assert_eq!(
        f.rows(cx, &goal)[1].1,
        RowState::Queued,
        "T2 waits for the review"
    );
    f.click(cx, "goal-m-T1");
    f.row_chat(cx, &goal, "T2").await;
    f.on(cx, |s, _, _| {
        assert!(s.goal(&goal).unwrap().rows[0].reviewed);
    });
}

/// A started task carries the chip; it opens the goal on its Goal tab.
#[gpui_kit::test]
async fn goal_chip_opens_the_goal(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.started(cx, &goal);
    f.row_chat(cx, &goal, "T1").await;
    let t1 = f.on(cx, |s, w, cx| {
        let name = s.row_task(&f.row_of(s, &goal, "T1")).unwrap().name.clone();
        s.open_task_named(&name, w, cx);
        name
    });
    assert!(f.has(cx, "goal-chip"));
    cx.update_window(f.window, |_, w, _| {
        let label = w.find("goal-chip").label().unwrap().to_string();
        assert!(
            label.starts_with("Open goal: Growth › ") && label.ends_with(" › T1"),
            "{label}"
        );
    })
    .unwrap();
    f.click(cx, "goal-chip");
    f.on(cx, |s, _, _| {
        assert_eq!(s.active.as_deref(), Some(goal.as_str()));
        assert_eq!(s.tab, Tab::Goal);
        assert_ne!(goal, t1);
    });
}

/// A goal's plan pane lists the draft as rows: reassigning and removing change `goal.draft`,
/// and the footer offers Approve and start.
#[gpui_kit::test]
async fn goal_plan_pane_edits_the_draft(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.on(cx, |s, w, cx| {
        s.open_task_named(&goal, w, cx);
        s.select_chat(0, w, cx);
    });
    assert!(
        f.has(cx, ("plan-row-agent", 1usize)),
        "the plan's rows show"
    );
    cx.update_window(f.window, |_, w, _| {
        assert!(w.try_find_by_label("Approve and start").is_some());
        assert!(w.try_find_by_label("Build in new chat").is_none());
        assert!(w.try_find_by_label("Approve and build").is_none());
    })
    .unwrap();

    f.click(cx, ("plan-row-agent", 1usize));
    f.keys(cx, &["down", "enter"]);
    f.on(cx, |s, _, _| {
        let agents: Vec<_> = (s.goal(&goal).unwrap().draft.iter())
            .map(|r| r.agent.clone())
            .collect();
        assert_eq!(agents, ["coder", "designer"]);
    });
    f.click(cx, ("plan-row-remove", 0usize));
    f.on(cx, |s, _, _| {
        let draft = &s.goal(&goal).unwrap().draft;
        assert_eq!(draft.len(), 1);
        assert_eq!((draft[0].key.as_str(), draft[0].after.len()), ("T2", 0));
    });
}

/// An ordinary chat's plan pane still builds.
#[gpui_kit::test]
async fn plain_plan_pane_still_offers_build(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let repos = f.on(cx, |s, _, _| s.state.repos.clone());
    f.on(cx, |s, w, cx| {
        s.create_task_for_test(
            "plain".into(),
            git::on_branch(&repos, "plain"),
            vec![],
            None,
            w,
            cx,
        )
    });
    let shark = f.shark.clone();
    until(cx, f.window, move |_, cx| shark.read(cx).active.is_some()).await;
    f.on(cx, |s, _, cx| {
        let chat = s.active_chat().unwrap().clone();
        chat.update(cx, |c, cx| {
            c.set_plan_mode(true, cx);
            c.play(
                crate::harness::Ev::Plan {
                    id: "p".into(),
                    text: PLAN.into(),
                },
                cx,
            );
        });
    });
    f.frame(cx);
    cx.update_window(f.window, |_, w, _| {
        assert!(w.try_find_by_label("Approve and build").is_some());
        assert!(w.try_find_by_label("Build in new chat").is_some());
        assert!(w.try_find_by_label("Approve and start").is_none());
    })
    .unwrap();
}

/// Space on a started row draws the card on the tab; escape closes it and the list has focus.
#[gpui_kit::test]
async fn space_peeks_the_cursor_row_on_the_goal_tab(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let (goal, _lead) = f.planned(cx, THREE).await;
    f.started(cx, &goal);
    f.row_chat(cx, &goal, "T1").await;
    f.on(cx, |s, w, cx| s.goal_focus.focus(w, cx));
    f.keys(cx, &["space"]);
    assert!(f.has(cx, "task-peek"), "the card is drawn");
    f.on(cx, |s, w, cx| s.peek.focus.clone().unwrap().focus(w, cx));
    f.keys(cx, &["escape"]);
    assert!(!f.has(cx, "task-peek"));
    f.on(cx, |s, w, _| {
        assert!(s.goal_focus.is_focused(w), "the list has the keys again")
    });
}

/// With Vim on, the reassign key is not the start of a chord.
#[gpui_kit::test]
async fn reassign_works_with_vim_on(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let (goal, _lead) = f.planned(cx, THREE).await;
    f.started(cx, &goal);
    f.on(cx, |s, w, cx| {
        s.state.vim = true;
        s.goal_focus.focus(w, cx);
        cx.notify();
    });
    f.frame(cx);
    f.keys(cx, &["down", "a"]);
    f.keys(cx, &["down", "enter"]);
    assert_eq!(f.rows(cx, &goal)[1].2, "designer");
}
