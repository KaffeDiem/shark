//! Teams: a goal's plan runs in the background, on real repos, with turns played in.
use super::*;
use crate::chat::Chat;
use crate::harness::Ev;
use crate::state::{Agent, GoalState, Limits, Row, RowState, Team, Why};

pub(super) const PLAN: &str = "# Paginate\n\nProse.\n\n## Tasks\n\
- T1 @coder [api]: Add cursor pagination\n\
\x20\x20Keep page-size working.\n\
- T2 @coder [api] after T1: Index saved items\n";

pub(super) fn agent(slug: &str) -> Agent {
    Agent {
        slug: slug.into(),
        name: slug.to_uppercase(),
        instructions: format!("You are {slug}."),
        ..Default::default()
    }
}

/// One repository, a lead and two agents in a team that skips the allowance check (the tests
/// have no usage to read) and runs `running` chats at once.
pub(super) fn team_state(home: &std::path::Path, running: u8) -> State {
    State {
        repos: repos(home, &["api"]),
        agents: vec![agent("lead"), agent("coder"), agent("designer")],
        teams: vec![Team {
            slug: "growth".into(),
            name: "Growth".into(),
            lead: "lead".into(),
            agents: vec!["coder".into(), "designer".into()],
            repos: vec!["api".into()],
            limits: Limits {
                running,
                floor: 0,
                ..Default::default()
            },
            ..Default::default()
        }],
        chat_agents_migrated: true,
        ..Default::default()
    }
}

pub(super) struct Fixture {
    pub(super) window: AnyWindowHandle,
    pub(super) shark: Entity<Shark>,
}

impl Fixture {
    pub(super) fn on<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) -> R,
    ) -> R {
        let out = cx
            .update_window(self.window, |_, w, cx| {
                self.shark.update(cx, |s, cx| f(s, w, cx))
            })
            .unwrap();
        cx.run_until_parked();
        out
    }

    /// The goal's task name, once its worktree exists.
    pub(super) async fn goal(&self, cx: &mut TestAppContext) -> String {
        let shark = self.shark.clone();
        until(cx, self.window, move |_, cx| {
            shark.read(cx).state.tasks.iter().any(|t| t.goal.is_some())
        })
        .await;
        cx.run_until_parked();
        self.on(cx, |s, _, _| {
            let goal = s.state.tasks.iter().find(|t| t.goal.is_some()).unwrap();
            goal.name.clone()
        })
    }

    pub(super) fn row(&self, cx: &mut TestAppContext, goal: &str, key: &str) -> Row {
        self.on(cx, |s, _, _| {
            let rows = &s.goal(goal).unwrap().rows;
            rows.iter().find(|r| r.key == key).unwrap().clone()
        })
    }

    /// Waits for a row's task to exist and returns its chat.
    pub(super) async fn row_chat(
        &self,
        cx: &mut TestAppContext,
        goal: &str,
        key: &str,
    ) -> Entity<Chat> {
        let (shark, g, k) = (self.shark.clone(), goal.to_string(), key.to_string());
        until(cx, self.window, move |_, cx| {
            let s = shark.read(cx);
            let row = s
                .goal(&g)
                .and_then(|goal| goal.rows.iter().find(|r| r.key == k));
            row.is_some_and(|r| r.chat.is_some() || r.state == RowState::Failed)
        })
        .await;
        cx.run_until_parked();
        let row = self.row(cx, goal, key);
        assert_eq!(row.error, None, "{key} started");
        self.on(cx, |s, _, _| s.chats[row.chat.as_ref().unwrap()].clone())
    }

    /// Plays a turn's end into `chat`: an optional reply, then the result.
    pub(super) fn end_turn(
        &self,
        cx: &mut TestAppContext,
        chat: &Entity<Chat>,
        reply: &str,
        secs: u64,
    ) {
        let chat = chat.clone();
        self.on(cx, |_, _, cx| {
            chat.update(cx, |c, cx| {
                if !reply.is_empty() {
                    c.play(Ev::Text(reply.into()), cx);
                }
                let (is_error, text) = (false, String::new());
                c.play(
                    Ev::Result {
                        is_error,
                        text,
                        secs,
                    },
                    cx,
                );
            })
        });
    }

    /// Waits for the background git check that follows a row's turn.
    pub(super) async fn settled(
        &self,
        cx: &mut TestAppContext,
        goal: &str,
        key: &str,
        state: RowState,
    ) {
        let (shark, g, k) = (self.shark.clone(), goal.to_string(), key.to_string());
        until(cx, self.window, move |_, cx| {
            let s = shark.read(cx);
            let row = s
                .goal(&g)
                .and_then(|goal| goal.rows.iter().find(|r| r.key == k));
            row.is_some_and(|r| r.state == state)
        })
        .await;
        cx.run_until_parked();
    }

    /// Gives the team a goal and has its lead submit `plan`.
    pub(super) async fn planned(
        &self,
        cx: &mut TestAppContext,
        plan: &str,
    ) -> (String, Entity<Chat>) {
        self.on(cx, |s, w, cx| {
            s.give_goal("growth", "Paginate the Saved list", w, cx)
        });
        let goal = self.goal(cx).await;
        let lead = self.on(cx, |s, _, _| {
            let id = s.task(&goal).unwrap().chats[0].id.clone();
            s.chats[&id].clone()
        });
        let (chat, text) = (lead.clone(), plan.to_string());
        self.on(cx, |_, _, cx| {
            chat.update(cx, |c, cx| {
                let id = "plan-1".to_string();
                c.play(Ev::Plan { id, text }, cx)
            })
        });
        self.end_turn(cx, &lead, "", 5);
        (goal, lead)
    }
}

pub(super) fn start(cx: &mut TestAppContext, running: u8) -> (Fixture, PathBuf) {
    let home = test_home();
    crate::chat::ui_tests::fake_claude();
    let (window, shark) = boot(cx, team_state(&home, running), WindowOptions::default());
    cx.run_until_parked();
    (Fixture { window, shark }, home)
}

#[gpui_kit::test]
async fn an_approved_plan_runs_in_the_background(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, lead) = f.planned(cx, PLAN).await;

    // The lead's first message carries the team note; the plan waits for approval.
    f.on(cx, |s, _, cx| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.state, GoalState::Review);
        assert_eq!(g.problems, Vec::<String>::new());
        assert_eq!(g.draft.len(), 2);
        assert!(g.rows.is_empty(), "nothing runs before approval");
        assert_eq!(
            s.attention_of(&goal, cx),
            Attn::Needs(crate::transcript::PLAN_TO_APPROVE.into())
        );
        let c = lead.read(cx);
        assert!(c.meta.plan, "the lead plans");
        assert!(c.ws.context().keys().any(|k| k == "team:growth"));
        assert_eq!(s.active, None, "giving a goal opens nothing");
    });

    // Leaving plan mode is refused; approving starts the plan and sends the lead nothing.
    let chat = lead.clone();
    f.on(cx, |_, w, cx| {
        chat.update(cx, |c, cx| {
            c.set_plan_mode(false, cx);
            assert!(c.meta.plan, "locked in plan mode");
            c.approve_plan(w, cx);
        })
    });
    f.on(cx, |s, _, cx| {
        let c = lead.read(cx);
        assert!(c.meta.plan, "still planning after approval");
        assert_eq!(c.sent_texts().len(), 1, "approval sends nothing");
        assert_eq!(c.plan.read(cx).approved, Some(0));
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.state, GoalState::Running);
        assert!(g.draft.is_empty());
        let states: Vec<_> = g.rows.iter().map(|r| r.state).collect();
        assert_eq!(
            states,
            [RowState::Started, RowState::Queued],
            "T2 waits for T1"
        );
        assert_eq!(s.active, None, "starting a task shows nothing");
        assert!(s.home_shown());
    });

    // T1 works, leaves changes, and is told to commit them locally.
    let t1 = f.row_chat(cx, &goal, "T1").await;
    let (t1_name, t1_dir) = f.on(cx, |s, _, cx| {
        let row = s.row_task(&f.row_of(s, &goal, "T1")).unwrap();
        assert_eq!(row.part.as_ref().unwrap().key, "T1");
        assert_eq!(
            row.last_interacted_at, None,
            "Shark's start isn't an interaction"
        );
        assert_eq!(s.state.last_repos, Vec::<String>::new());
        let c = t1.read(cx);
        assert!(c.busy && !c.meta.plan);
        assert_eq!(c.meta.agent.as_deref(), Some("coder"));
        let first = &c.sent_texts()[0];
        assert!(first.starts_with("Add cursor pagination\n\nKeep page-size working."));
        assert!(c.ws.context().keys().any(|k| k == &format!("goal:{goal}")));
        (row.name.clone(), row.dir().join("api"))
    });
    fs::write(t1_dir.join("page.rs"), "cursor").unwrap();
    f.end_turn(cx, &t1, "Added the cursor.", 30);
    f.settled(cx, &goal, "T1", RowState::Committing).await;
    f.on(cx, |s, _, cx| {
        let row = f.row_of(s, &goal, "T1");
        assert_eq!(row.result.as_deref(), Some("Added the cursor."));
        let sent = t1.read(cx).sent_texts();
        let commit = sent.last().unwrap();
        assert!(
            commit.contains("Commit its changes locally in: api"),
            "{commit}"
        );
        assert!(commit.contains("Do not push"));
        assert_eq!(s.goal(&goal).unwrap().secs, 35, "the lead's turn and T1's");
    });

    // The commit turn's reply must not replace the result; a clean tree with a commit finishes.
    commit_all(&t1_dir, "cursor");
    f.end_turn(cx, &t1, "Committed abc123.", 4);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    assert_eq!(
        f.row(cx, &goal, "T1").result.as_deref(),
        Some("Added the cursor.")
    );

    // T2 starts from T1's local branch, which is also what its PR targets.
    let t2 = f.row_chat(cx, &goal, "T2").await;
    let t2_dir = f.on(cx, |s, _, cx| {
        let parent = s.task(&t1_name).unwrap().branch.clone();
        let task = s.row_task(&f.row_of(s, &goal, "T2")).unwrap();
        assert_eq!(
            task.repos[0].base, parent,
            "the parent's branch is the base"
        );
        assert_eq!(
            git::short(&task.repos[0].base),
            parent,
            "and the PR's --base"
        );
        let wt = task.dir().join("api");
        assert!(wt.join("page.rs").exists(), "T1's commit is underneath");
        assert_eq!(
            task.part.as_ref().unwrap().after,
            [("T1".to_string(), "Added the cursor.".to_string())]
        );
        let notes = t2.read(cx).ws.context();
        assert!(notes["after:T1"].text.contains("Added the cursor."));
        assert_eq!(s.active, None);
        wt
    });

    // An interrupted turn leaves the row stuck, and the goal says so once.
    let chat = t2.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| c.play(Ev::Interrupted, cx))
    });
    f.end_turn(cx, &t2, "", 3);
    f.on(cx, |s, _, cx| {
        let row = f.row_of(s, &goal, "T2");
        assert_eq!(row.state, RowState::Failed);
        assert_eq!(row.error.as_deref(), Some("Interrupted"));
        assert_eq!(s.attention_of(&goal, cx), Attn::Needs(team::STUCK.into()));
        assert_eq!(
            s.row_status(&row, cx),
            team::RowStatus::Stuck("Interrupted".into())
        );
    });

    // Retry sends the agent on; a rejected commit leaves it stuck again.
    f.on(cx, |s, w, cx| s.retry_row(&goal, "T2", w, cx));
    assert_eq!(f.row(cx, &goal, "T2").state, RowState::Started);
    fs::write(t2_dir.join("index.sql"), "create index").unwrap();
    f.end_turn(cx, &t2, "Indexed.", 10);
    f.settled(cx, &goal, "T2", RowState::Committing).await;
    f.end_turn(cx, &t2, "The hook rejected it.", 2);
    f.settled(cx, &goal, "T2", RowState::Failed).await;
    let row = f.row(cx, &goal, "T2");
    assert_eq!(row.error.as_deref(), Some("Commit failed"));
    assert_eq!(row.result.as_deref(), Some("Indexed."));

    // Retrying a failed commit asks for the commit again; once it lands the goal is ready and
    // the lead writes the summary.
    f.on(cx, |s, w, cx| s.retry_row(&goal, "T2", w, cx));
    assert_eq!(f.row(cx, &goal, "T2").state, RowState::Committing);
    commit_all(&t2_dir, "index");
    f.end_turn(cx, &t2, "Committed.", 2);
    f.settled(cx, &goal, "T2", RowState::Finished).await;
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Ready);
        let sent = lead.read(cx).sent_texts();
        assert_eq!(sent.len(), 2, "one more turn for the lead: the summary");
        assert!(sent[1].starts_with("Every task of the plan is finished."));
        assert!(lead.read(cx).meta.plan);
    });
    f.end_turn(cx, &lead, "Both tasks landed.", 6);
    f.on(cx, |s, _, _| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.summary.as_deref(), Some("Both tasks landed."));
        assert_eq!(crate::team::progress(g), (2, 2));
    });
    let log = crate::team::activity("growth");
    let what: Vec<_> = log.iter().map(|a| a.what.as_str()).collect();
    for expected in [
        "gave the goal",
        "submitted a plan",
        "started",
        "finished",
        "wrote the summary",
    ] {
        assert!(what.contains(&expected), "{expected} not in {what:?}");
    }
}

impl Fixture {
    pub(super) fn row_of(&self, s: &Shark, goal: &str, key: &str) -> Row {
        let rows = &s.goal(goal).unwrap().rows;
        rows.iter().find(|r| r.key == key).unwrap().clone()
    }
}

#[gpui_kit::test]
async fn limits_pause_and_stop_hold_the_plan(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    let plan =
        "## Tasks\n- T1 @coder [api]: One\n- T2 @designer [api]: Two\n- T3 @coder [api]: Three\n";
    let (goal, _lead) = f.planned(cx, plan).await;
    f.on(cx, |s, w, cx| {
        s.update_team("growth", w, cx, |t| t.limits.minutes = 1);
        s.approve_goal(&goal, w, cx);
    });
    // One at once: T2 waits for a slot, not for T1's result.
    f.on(cx, |s, _, cx| {
        let states: Vec<_> = (s.goal(&goal).unwrap().rows.iter())
            .map(|r| r.state)
            .collect();
        assert_eq!(
            states,
            [RowState::Started, RowState::Queued, RowState::Queued]
        );
        assert_eq!(s.team_running("growth", cx), 1);
    });
    let t1 = f.row_chat(cx, &goal, "T1").await;

    // A paused team starts nothing when the slot frees; the running turn finishes.
    f.on(cx, |s, w, cx| {
        s.update_team("growth", w, cx, |t| t.paused = true)
    });
    f.end_turn(cx, &t1, "Nothing to change.", 20);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    assert_eq!(f.row(cx, &goal, "T2").state, RowState::Queued);
    f.on(cx, |s, w, cx| {
        s.update_team("growth", w, cx, |t| t.paused = false)
    });
    let t2 = f.row_chat(cx, &goal, "T2").await;

    // The agent time runs out: the goal pauses, says why, and starts nothing.
    f.end_turn(cx, &t2, "Done.", 60);
    f.settled(cx, &goal, "T2", RowState::Finished).await;
    f.on(cx, |s, _, cx| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.state, GoalState::Paused(Why::Time));
        assert_eq!(g.rows[2].state, RowState::Queued);
        assert_eq!(s.attention_of(&goal, cx), Attn::Needs(team::LIMIT.into()));
    });
    // Keep paused dismisses the item; Add 1 h lets the plan go on.
    f.on(cx, |s, w, cx| s.keep_goal_paused(&goal, w, cx));
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Paused(Why::You));
        assert!(!matches!(s.attention_of(&goal, cx), Attn::Needs(_)));
    });
    f.on(cx, |s, w, cx| {
        s.add_goal_hour(&goal, w, cx);
        s.resume_goal(&goal, w, cx);
    });
    let t3 = f.row_chat(cx, &goal, "T3").await;

    // Stop interrupts the running chat and starts nothing more.
    f.on(cx, |s, _, cx| s.stop_goal(&goal, cx));
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Stopped);
        assert!(t3.read(cx).sent_texts().len() == 1);
    });
    let chat = t3.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| c.play(Ev::Interrupted, cx))
    });
    f.end_turn(cx, &t3, "", 1);
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().rows[2].state, RowState::Failed);
        assert!(
            !matches!(s.attention_of(&goal, cx), Attn::Needs(_)),
            "a stopped goal isn't stuck"
        );
    });
}

#[gpui_kit::test]
async fn a_restart_settles_rows_that_were_mid_turn(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let plan = "## Tasks\n- T1 @coder [api]: One\n- T2 @designer [api]: Two\n- T3 @coder [api] after T2: Three\n";
    let (goal, _lead) = f.planned(cx, plan).await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;
    let _t2 = f.row_chat(cx, &goal, "T2").await;
    // T1 reaches its commit turn and commits; T2 is still in its work turn.
    let t1_dir = f.on(cx, |s, _, _| {
        s.row_task(&f.row_of(s, &goal, "T1"))
            .unwrap()
            .dir()
            .join("api")
    });
    fs::write(t1_dir.join("one.rs"), "1").unwrap();
    f.end_turn(cx, &t1, "One.", 5);
    f.settled(cx, &goal, "T1", RowState::Committing).await;
    commit_all(&t1_dir, "one");

    // Quit and launch: the rows are as they were saved, and no turn runs.
    f.on(cx, |s, w, cx| {
        s.chats.clear();
        s.syncs.clear();
        s.settle_goals(w, cx);
    });
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    f.on(cx, |s, _, cx| {
        let rows = &s.goal(&goal).unwrap().rows;
        assert_eq!(rows[1].state, RowState::Failed);
        assert_eq!(
            rows[1].error.as_deref(),
            Some("Interrupted when Shark quit")
        );
        assert_eq!(
            rows[2].state,
            RowState::Queued,
            "nothing after a stuck row starts"
        );
        assert_eq!(s.attention_of(&goal, cx), Attn::Needs(team::STUCK.into()));
        // Their entities exist again, so a retried turn reports in.
        assert!(s.chats.contains_key(rows[1].chat.as_ref().unwrap()));
        assert!(s.syncs.contains_key(&rows[1].task.as_ref().unwrap().name));
    });
}

#[gpui_kit::test]
async fn an_unreadable_plan_is_sent_back_once_and_cannot_start(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let bad = "## Tasks\n- T1 @ghost [api]: One\nnot a task\n";
    let (goal, lead) = f.planned(cx, bad).await;
    f.on(cx, |s, w, cx| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.state, GoalState::Planning, "the lead is fixing it");
        assert!(g.asked_fix);
        let sent = lead.read(cx).sent_texts();
        assert!(sent[1].contains("Couldn't read: not a task"), "{}", sent[1]);
        assert!(sent[1].contains("@ghost is not an agent of this team"));
        s.approve_goal(&goal, w, cx);
        assert!(s.goal(&goal).unwrap().rows.is_empty(), "it can't start");
    });
    // The same mistakes again: it isn't asked twice, and the plan waits for the user.
    let (chat, text) = (lead.clone(), format!("{bad}\n"));
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| {
            let id = "plan-2".to_string();
            c.play(Ev::Plan { id, text }, cx)
        })
    });
    f.end_turn(cx, &lead, "", 1);
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Review);
        assert_eq!(lead.read(cx).sent_texts().len(), 2);
    });
    // Reassigning the row and dropping nothing else leaves the unreadable line in the way.
    f.on(cx, |s, w, cx| {
        s.reassign_draft_row(&goal, "T1", "coder", cx);
        let problems = &s.goal(&goal).unwrap().problems;
        assert_eq!(problems, &["Couldn't read: not a task"]);
        s.approve_goal(&goal, w, cx);
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Review);
    });
}

#[gpui_kit::test]
async fn stricter_approval_holds_plans_and_dependents(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    f.on(cx, |s, w, cx| {
        s.update_team("growth", w, cx, |t| t.approve_tasks = true)
    });
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));

    // Every task plans first, and its plan waits for you like the goal's did.
    let t1 = f.row_chat(cx, &goal, "T1").await;
    let name = f.on(cx, |s, _, cx| {
        assert!(t1.read(cx).meta.plan, "the task starts in plan mode");
        s.row_task(&f.row_of(s, &goal, "T1")).unwrap().name.clone()
    });
    let chat = t1.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| {
            let (id, text) = ("p".to_string(), "# Do it\n\nSteps.".to_string());
            c.play(Ev::Plan { id, text }, cx)
        })
    });
    f.end_turn(cx, &t1, "", 2);
    f.on(cx, |s, _, cx| {
        assert_eq!(
            f.row_of(s, &goal, "T1").state,
            RowState::Started,
            "not finished: it planned"
        );
        let plan = Attn::Needs(crate::transcript::PLAN_TO_APPROVE.into());
        assert_eq!(s.attention_of(&name, cx), plan);
        assert_eq!(
            s.goal_attention(&goal, cx),
            plan,
            "the goal shows its task's need"
        );
        assert_eq!(s.goal_needs_count(&goal, cx), 1, "counted once");
        assert_eq!(
            s.team_running("growth", cx),
            0,
            "a waiting plan holds no slot"
        );
    });

    // Approve and build in the task; its work then finishes with nothing to commit.
    let chat = t1.clone();
    f.on(cx, |_, w, cx| {
        chat.update(cx, |c, cx| c.approve_plan(w, cx))
    });
    f.on(cx, |_, _, cx| {
        assert!(t1.read(cx).busy && !t1.read(cx).meta.plan)
    });
    f.end_turn(cx, &t1, "Built.", 3);
    f.settled(cx, &goal, "T1", RowState::Finished).await;

    // T2 comes after T1 and waits until you have reviewed it.
    f.on(cx, |s, _, cx| {
        assert_eq!(f.row_of(s, &goal, "T2").state, RowState::Queued);
        assert_eq!(
            s.row_hold(&goal, "T2"),
            Some(crate::team::Hold::Review("T1".into()))
        );
        assert_eq!(s.attention_of(&goal, cx), Attn::Needs(team::REVIEW.into()));
    });
    f.on(cx, |s, w, cx| s.mark_reviewed(&goal, "T1", w, cx));
    f.row_chat(cx, &goal, "T2").await;
    assert_eq!(f.row(cx, &goal, "T2").state, RowState::Started);
}

#[gpui_kit::test]
async fn a_task_hands_off_inside_its_goal_and_a_failure_reaches_the_lead(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    // Coder hands off to designer, who is on the team and hands back: the chain ends there.
    f.on(cx, |s, _, _| {
        s.state.agents[1].next = Some("designer".into());
        s.state.agents[2].next = Some("coder".into());
    });
    let plan = "## Tasks\n- T1 @coder [api]: Build it\n- T2 @coder [api]: Break it\n";
    let (goal, lead) = f.planned(cx, plan).await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;
    let t2 = f.row_chat(cx, &goal, "T2").await;

    // The handoff reads the transcript from disk, as the manual one does.
    let (task, first) = f.on(cx, |s, _, cx| {
        let task = s.row_task(&f.row_of(s, &goal, "T1")).unwrap().clone();
        (task, t1.read(cx).meta.id.clone())
    });
    let path = crate::transcript::transcript_path(&first);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lines = [
        r#"{"type":"user","message":{"content":"Build it"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Built"}]}}"#,
        r#"{"type":"result","result":"Built","is_error":false}"#,
    ];
    fs::write(path, lines.join("\n")).unwrap();
    f.end_turn(cx, &t1, "Built.", 5);
    let (shark, g) = (f.shark.clone(), goal.clone());
    until(cx, f.window, move |_, cx| {
        let row = &shark.read(cx).goal(&g).unwrap().rows[0];
        row.chat.as_deref() != Some(first.as_str()) || row.state == RowState::Failed
    })
    .await;
    cx.run_until_parked();
    let second = f.on(cx, |s, _, cx| {
        let row = f.row_of(s, &goal, "T1");
        assert_eq!(row.error, None);
        assert_eq!(
            row.state,
            RowState::Started,
            "still at work, in other hands"
        );
        assert_eq!(row.result.as_deref(), Some("Built."));
        let chat = s.chats[row.chat.as_ref().unwrap()].clone();
        let c = chat.read(cx);
        assert_eq!(c.meta.agent.as_deref(), Some("designer"));
        let sent = c.sent_texts();
        assert!(
            sent[0].starts_with("Take over this task: Build it."),
            "{}",
            sent[0]
        );
        assert!(sent[0].contains("transcript.md"));
        assert_eq!(s.task(&task.name).unwrap().chats.len(), 2);
        assert_eq!(s.active, None, "nothing was shown");
        // The Agents view credits T1 to whoever has it now.
        let team = s.team("growth").unwrap();
        assert_eq!(s.agent_line(team, "designer", cx).text, "T1 running");
        assert_eq!(s.agent_line(team, "coder", cx).text, "T2 running");
        chat
    });
    // The designer names coder next, who already worked here: no loop, the row finishes.
    f.end_turn(cx, &second, "Polished.", 4);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    assert_eq!(f.row(cx, &goal, "T1").result.as_deref(), Some("Polished."));

    // A failed turn gives the lead a turn with the task's last reply.
    let chat = t2.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| {
            c.play(Ev::Text("The API is gone.".into()), cx);
            let (is_error, text, secs) = (true, "boom".to_string(), 1);
            c.play(
                Ev::Result {
                    is_error,
                    text,
                    secs,
                },
                cx,
            );
        })
    });
    f.on(cx, |s, _, cx| {
        let row = f.row_of(s, &goal, "T2");
        assert_eq!(row.state, RowState::Failed);
        assert_eq!(row.error.as_deref(), Some("The turn failed"));
        let sent = lead.read(cx).sent_texts();
        let told = sent.last().unwrap();
        assert!(told.starts_with("T2 is stuck"), "{told}");
        assert!(told.contains("The API is gone."));
    });
}

/// Has the lead submit plan version `id` and end its turn.
fn submit(f: &Fixture, cx: &mut TestAppContext, lead: &Entity<Chat>, id: &str, plan: &str) {
    let (chat, id, text) = (lead.clone(), id.to_string(), plan.to_string());
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| c.play(Ev::Plan { id, text }, cx))
    });
    f.end_turn(cx, lead, "", 1);
}

#[gpui_kit::test]
async fn a_later_plan_keeps_queued_rows_and_the_users_edits(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, lead) = f.planned(cx, PLAN).await;

    // An edit to the draft survives a prose reply from the lead: the plan wasn't resubmitted.
    f.on(cx, |s, _, cx| {
        s.reassign_draft_row(&goal, "T1", "designer", cx)
    });
    let chat = lead.clone();
    f.on(cx, |_, w, cx| {
        chat.update(cx, |c, cx| _ = c.ask("Why two tasks?", w, cx))
    });
    f.end_turn(cx, &lead, "Because of the index.", 1);
    f.on(cx, |s, w, cx| {
        assert_eq!(
            s.goal(&goal).unwrap().draft[0].agent,
            "designer",
            "the edit stays"
        );
        s.reassign_draft_row(&goal, "T1", "coder", cx);
        s.approve_goal(&goal, w, cx);
    });
    let t1 = f.row_chat(cx, &goal, "T1").await;

    // A plan without tasks can't be approved, and approving anyway keeps the queued row.
    submit(&f, cx, &lead, "plan-2", "# Notes\n\nNothing to change.\n");
    f.on(cx, |s, w, cx| {
        let g = s.goal(&goal).unwrap();
        assert!(!g.problems.is_empty(), "no tasks is a problem");
        s.approve_goal(&goal, w, cx);
        let keys: Vec<_> = (s.goal(&goal).unwrap().rows.iter())
            .map(|r| (r.key.clone(), r.state))
            .collect();
        assert_eq!(
            keys,
            [
                ("T1".into(), RowState::Started),
                ("T2".into(), RowState::Queued)
            ]
        );
    });

    // The lead rewrites a failed task: the new brief replaces it and it starts over.
    let chat = t1.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| {
            let (is_error, text, secs) = (true, "boom".to_string(), 1);
            c.play(
                Ev::Result {
                    is_error,
                    text,
                    secs,
                },
                cx,
            );
        })
    });
    assert_eq!(f.row(cx, &goal, "T1").state, RowState::Failed);
    f.end_turn(cx, &lead, "", 1);
    let again = "## Tasks\n- T1 @designer [api]: Add cursor pagination another way\n- T2 @coder [api] after T1: Index saved items\n";
    submit(&f, cx, &lead, "plan-3", again);
    f.on(cx, |s, w, cx| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.problems, Vec::<String>::new());
        assert_eq!(
            g.draft.len(),
            2,
            "a changed failed row is something to approve"
        );
        s.approve_goal(&goal, w, cx);
    });
    let retried = f.row_chat(cx, &goal, "T1").await;
    f.on(cx, |s, _, cx| {
        let row = f.row_of(s, &goal, "T1");
        assert_eq!(
            (row.state, row.agent.as_str()),
            (RowState::Started, "designer")
        );
        assert_ne!(retried.read(cx).meta.id, t1.read(cx).meta.id, "a new task");
    });
}

#[gpui_kit::test]
async fn stop_and_pause_hold_at_the_edges(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let plan = "## Tasks\n- T1 @coder [api]: One\n- T2 @designer [api]: Two\n";
    let (goal, _lead) = f.planned(cx, plan).await;

    // Stopped while its worktree is still being made: the task must not go to work.
    f.on(cx, |s, w, cx| {
        s.approve_goal(&goal, w, cx);
        s.stop_goal(&goal, cx);
    });
    let (shark, g) = (f.shark.clone(), goal.clone());
    until(cx, f.window, move |_, cx| {
        let s = shark.read(cx);
        (s.goal(&g).unwrap().rows.iter()).all(|r| r.task.is_some() || r.state == RowState::Queued)
            && s.creating.is_empty()
    })
    .await;
    cx.run_until_parked();
    f.on(cx, |s, w, cx| {
        for row in &s.goal(&goal).unwrap().rows {
            assert_eq!(
                row.state,
                RowState::Failed,
                "{} isn't left at work",
                row.key
            );
        }
        assert!(!matches!(s.attention_of(&goal, cx), Attn::Needs(_)));
        // Nothing restarts in a stopped goal.
        s.retry_row(&goal, "T1", w, cx);
        assert_eq!(s.goal(&goal).unwrap().rows[0].state, RowState::Failed);
    });

    // Resumed and retried, then paused: the running turn finishes, and with every task done the
    // goal is ready though it was paused.
    let t1 = f.on(cx, |s, _, _| {
        s.chats[f.row_of(s, &goal, "T1").chat.as_ref().unwrap()].clone()
    });
    let chat = t1.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| c.play(Ev::Interrupted, cx))
    });
    f.end_turn(cx, &t1, "", 1);
    f.on(cx, |s, w, cx| {
        s.resume_goal(&goal, w, cx);
        s.remove_row(&goal, "T2", w, cx);
        s.retry_row(&goal, "T1", w, cx);
    });
    assert_eq!(f.row(cx, &goal, "T1").state, RowState::Started);
    f.on(cx, |s, w, cx| s.pause_goal(&goal, w, cx));
    f.end_turn(cx, &t1, "Done.", 2);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    assert_eq!(
        f.on(cx, |s, _, _| s.goal(&goal).unwrap().state),
        GoalState::Ready
    );
}

#[gpui_kit::test]
async fn an_idle_lead_and_a_held_handoff_are_visible(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 1);
    f.on(cx, |s, w, cx| {
        s.state.agents[1].next = Some("designer".into());
        s.update_team("growth", w, cx, |t| {
            t.paused_agents = vec!["designer".into()]
        });
        s.give_goal("growth", "Paginate", w, cx);
    });
    let goal = f.goal(cx).await;
    let lead = f.on(cx, |s, _, _| {
        s.chats[&s.task(&goal).unwrap().chats[0].id].clone()
    });

    // The lead ends its turn without a plan: the goal would say Planning forever.
    f.end_turn(cx, &lead, "Which list do you mean?", 1);
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Planning);
        assert_eq!(
            s.attention_of(&goal, cx),
            Attn::Needs(team::LEAD_IDLE.into())
        );
    });
    let chat = lead.clone();
    f.on(cx, |_, w, cx| {
        chat.update(cx, |c, cx| _ = c.ask("The Saved list.", w, cx))
    });
    f.on(cx, |s, _, cx| {
        assert!(!matches!(s.attention_of(&goal, cx), Attn::Needs(_)))
    });
    let plan = "## Tasks\n- T1 @coder [api]: One\n- T2 @coder [api]: Two\n";
    submit(&f, cx, &lead, "plan-1", plan);
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;

    // The next agent is paused in this team: the handoff waits, and the row keeps its slot.
    f.end_turn(cx, &t1, "Built.", 1);
    f.on(cx, |s, _, cx| {
        let rows = &s.goal(&goal).unwrap().rows;
        assert_eq!(
            rows[0].handoff.as_deref(),
            Some("designer"),
            "held, not started"
        );
        assert_eq!(rows[0].chat.as_deref(), Some(t1.read(cx).meta.id.as_str()));
        assert_eq!(
            s.team_running("growth", cx),
            1,
            "a row in transition holds its slot"
        );
        assert_eq!(rows[1].state, RowState::Queued, "one at once");
    });
}

#[gpui_kit::test]
async fn a_restart_asks_for_the_summary_again(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, lead) = f.planned(cx, "## Tasks\n- T1 @coder [api]: One\n").await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;

    // A work turn's git check that lands after Stop sends no commit request.
    let ws = f.on(cx, |s, _, _| {
        s.row_task(&f.row_of(s, &goal, "T1")).unwrap().clone()
    });
    f.on(cx, |s, w, cx| {
        s.stop_goal(&goal, cx);
        s.row_checked(&goal, "T1", &ws, (true, false), w, cx);
        assert_eq!(
            t1.read(cx).sent_texts().len(),
            1,
            "nothing sent into a stopped goal"
        );
        assert_eq!(
            s.goal(&goal).unwrap().rows[0].error.as_deref(),
            Some("Stopped")
        );
    });
    let chat = t1.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| c.play(Ev::Interrupted, cx))
    });
    f.end_turn(cx, &t1, "", 1);
    f.on(cx, |s, w, cx| {
        s.resume_goal(&goal, w, cx);
        s.retry_row(&goal, "T1", w, cx);
    });

    f.end_turn(cx, &t1, "Done.", 1);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    f.on(cx, |s, w, cx| {
        let g = s.goal(&goal).unwrap();
        assert_eq!(g.state, GoalState::Ready);
        assert!(g.summary_asked && lead.read(cx).busy);
        // Quit before the summary arrived.
        s.chats.clear();
        s.settle_goals(w, cx);
    });
    cx.run_until_parked();
    f.on(cx, |s, _, cx| {
        let again = s.chats[&s.task(&goal).unwrap().chats[0].id].clone();
        let sent = again.read(cx).sent_texts();
        assert!(
            sent.last()
                .is_some_and(|t| t.starts_with("Every task of the plan is finished.")),
            "{sent:?}"
        );
    });
}

#[gpui_kit::test]
async fn the_goal_tab_edits_the_running_plan_and_the_plan_pane_the_draft(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, lead) = f.planned(cx, PLAN).await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;
    // A revised plan waits while T1 runs.
    let revised = "## Tasks\n- T1 @coder [api]: Add cursor pagination\n\x20\x20Keep page-size working.\n- T2 @designer [api] after T1: Index saved items\n- T3 @coder [api]: Document it\n";
    submit(&f, cx, &lead, "plan-2", revised);
    f.on(cx, |s, w, cx| {
        assert_eq!(s.goal(&goal).unwrap().draft.len(), 3);
        // The plan pane edits what waits; the running plan stays as approved.
        s.reassign_draft_row(&goal, "T3", "designer", cx);
        s.remove_draft_row(&goal, "T2", cx);
        let g = s.goal(&goal).unwrap();
        let draft: Vec<_> = g
            .draft
            .iter()
            .map(|r| (r.key.as_str(), r.agent.as_str()))
            .collect();
        assert_eq!(draft, [("T1", "coder"), ("T3", "designer")]);
        assert_eq!(g.rows.len(), 2, "the approved plan is untouched");
        assert_eq!(g.rows[1].agent, "coder");
        // The Goal tab edits what runs: its rows are the approved ones.
        s.reassign_row(&goal, "T2", "designer", cx);
        s.remove_row(&goal, "T1", w, cx);
        let g = s.goal(&goal).unwrap();
        assert_eq!(
            g.rows[0].state,
            RowState::Skipped,
            "the running task is taken out"
        );
        assert_eq!(g.rows[1].agent, "designer");
        assert_eq!(g.draft.len(), 2, "the draft is untouched");
        assert!(t1.read(cx).sent_texts().len() == 1);
    });
}

#[gpui_kit::test]
async fn a_goals_menu_offers_no_section(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    f.on(cx, |s, w, cx| {
        s.approve_goal(&goal, w, cx);
        blank(s, w, cx);
    });
    let _t1 = f.row_chat(cx, &goal, "T1").await;
    f.on(cx, |s, _, cx| {
        let labels = |task: &Task| -> Vec<String> {
            (super::menus::ws_menu(s, task, cx).iter())
                .filter_map(|e| match e {
                    Entry::Sub { label, .. } => Some(label.to_string()),
                    _ => None,
                })
                .collect()
        };
        let moves = |task: &Task| labels(task).iter().any(|l| l == "Move to section");
        let part = s.row_task(&f.row_of(s, &goal, "T1")).unwrap();
        assert!(!moves(s.task(&goal).unwrap()), "a goal sits under its team");
        assert!(!moves(part), "and so do its tasks");
        let plain = s
            .state
            .tasks
            .iter()
            .find(|t| t.goal.is_none() && t.part.is_none());
        assert!(moves(plain.unwrap()), "an ordinary task still moves");
    });
}

/// An ordinary task beside the goal.
fn blank(s: &mut Shark, w: &mut Window, cx: &mut Context<Shark>) {
    s.new_task(None, None, None, w, cx);
}

#[gpui_kit::test]
async fn a_goal_never_runs_without_a_signal(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    f.on(cx, |s, w, cx| {
        s.update_team("growth", w, cx, |t| t.limits.tasks = 1)
    });
    let (goal, lead) = f.planned(cx, PLAN).await;
    // The lead was asked once to fix the plan; it answers without a new one.
    f.end_turn(cx, &lead, "It needs both tasks.", 1);

    // Over the task limit: it can't start. Raising the limit clears the problem at once.
    f.on(cx, |s, w, cx| {
        let problems = &s.goal(&goal).unwrap().problems;
        assert!(
            problems
                .iter()
                .any(|p| p.contains("2 tasks; the limit is 1")),
            "{problems:?}"
        );
        s.approve_goal(&goal, w, cx);
        assert!(s.goal(&goal).unwrap().rows.is_empty());
        s.update_team("growth", w, cx, |t| t.limits.tasks = 8);
        let g = s.goal(&goal).unwrap();
        assert_eq!((g.max_tasks, g.problems.len()), (8, 0), "{:?}", g.problems);
    });

    // Stopped before approval: approving doesn't bring it back, and resuming returns it to
    // the plan that waits, not to a run of nothing.
    f.on(cx, |s, w, cx| {
        s.stop_goal(&goal, cx);
        s.approve_goal(&goal, w, cx);
        let g = s.goal(&goal).unwrap();
        assert_eq!(
            g.state,
            GoalState::Stopped,
            "approve doesn't revive a stopped goal"
        );
        assert!(g.rows.is_empty());
        s.resume_goal(&goal, w, cx);
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Review);
        s.approve_goal(&goal, w, cx);
    });
    let t1 = f.row_chat(cx, &goal, "T1").await;

    // A chat that goes away mid-turn leaves its row stuck, not running forever.
    f.on(cx, |s, w, cx| {
        let id = t1.read(cx).meta.id.clone();
        let name = s.row_task(&f.row_of(s, &goal, "T1")).unwrap().name.clone();
        let ws = s.task_mut(&name).unwrap();
        ws.chats[0].id = format!("{id}-replaced");
        s.advance_goals(w, cx);
        let row = f.row_of(s, &goal, "T1");
        assert_eq!(row.state, RowState::Failed);
        assert_eq!(row.error.as_deref(), Some("Its chat was closed"));
        assert_eq!(s.team_running("growth", cx), 0);
    });

    // Removing every row of the running plan leaves a goal that asks for a new plan.
    f.on(cx, |s, w, cx| {
        s.remove_row(&goal, "T1", w, cx);
        s.remove_row(&goal, "T2", w, cx);
        let g = s.goal(&goal).unwrap();
        assert_eq!(
            g.rows
                .iter()
                .filter(|r| r.state != RowState::Skipped)
                .count(),
            0
        );
        assert_eq!(g.state, GoalState::Planning);
        assert!(!lead.read(cx).busy);
        assert_eq!(
            s.attention_of(&goal, cx),
            Attn::Needs(team::LEAD_IDLE.into())
        );
    });
}

#[gpui_kit::test]
async fn shark_driven_turns_stay_quiet_and_a_question_counts_once(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, "## Tasks\n- T1 @coder [api]: One\n").await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;
    let (name, dir) = f.on(cx, |s, _, _| {
        let task = s.row_task(&f.row_of(s, &goal, "T1")).unwrap();
        (task.name.clone(), task.dir().join("api"))
    });
    let baseline = f.on(cx, |s, _, cx| {
        s.refresh_attention(cx);
        crate::attention::BADGE.load(SeqCst)
    });

    // A question in a goal's task is one item: its task's, not its goal's as well.
    let chat = t1.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| {
            let input = serde_json::json!({"questions": [{"id": "db", "header": "DB",
                "question": "Which DB?", "isOther": true,
                "options": [{"label": "Postgres", "description": "SQL"}]}]});
            let asked = crate::harness::Questions::new("q".into(), "call-q".into(), input, true);
            c.play(Ev::Questions(asked), cx)
        })
    });
    f.on(cx, |s, _, cx| {
        let question = Attn::Needs(crate::transcript::QUESTION.into());
        assert_eq!(s.attention_of(&name, cx), question);
        assert_eq!(s.goal_attention(&goal, cx), question, "the goal shows it");
        assert!(
            !matches!(s.attention_of(&goal, cx), Attn::Needs(_)),
            "but doesn't own it"
        );
        assert_eq!(s.goal_needs_count(&goal, cx), 1);
        s.refresh_attention(cx);
        assert_eq!(
            crate::attention::BADGE.load(SeqCst),
            baseline + 1,
            "counted once"
        );
    });

    // The work turn and the commit turn end without marking the task unread: Shark asked for
    // them, and the goal reports when its plan is done.
    fs::write(dir.join("one.rs"), "1").unwrap();
    f.end_turn(cx, &t1, "One.", 1);
    f.settled(cx, &goal, "T1", RowState::Committing).await;
    f.on(cx, |s, _, cx| {
        assert_eq!(
            s.attention_of(&name, cx),
            Attn::None,
            "a driven turn isn't unread"
        );
        s.refresh_attention(cx);
        assert_eq!(crate::attention::BADGE.load(SeqCst), baseline);
    });
    // A commit turn that commits nothing is stuck, and Retry asks for the commit again.
    fs::remove_file(dir.join("one.rs")).unwrap();
    f.end_turn(cx, &t1, "Committed.", 1);
    f.settled(cx, &goal, "T1", RowState::Failed).await;
    f.on(cx, |s, w, cx| {
        let row = f.row_of(s, &goal, "T1");
        assert_eq!(row.error.as_deref(), Some("Commit failed"));
        assert!(row.commit_failed);
        assert_eq!(row.result.as_deref(), Some("One."));
        s.retry_row(&goal, "T1", w, cx);
        assert_eq!(f.row_of(s, &goal, "T1").state, RowState::Committing);
        assert!(
            t1.read(cx)
                .sent_texts()
                .last()
                .unwrap()
                .contains("Commit its changes locally")
        );
    });
    // An interrupted commit turn is a failed commit too.
    let chat = t1.clone();
    f.on(cx, |_, _, cx| {
        chat.update(cx, |c, cx| c.play(Ev::Interrupted, cx))
    });
    f.end_turn(cx, &t1, "", 1);
    f.on(cx, |s, w, cx| {
        assert!(f.row_of(s, &goal, "T1").commit_failed);
        s.retry_row(&goal, "T1", w, cx);
    });
    fs::write(dir.join("one.rs"), "1").unwrap();
    commit_all(&dir, "one");
    f.end_turn(cx, &t1, "Committed.", 1);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    // Done: the goal is the one thing left unseen.
    f.on(cx, |s, _, cx| {
        assert_eq!(s.goal(&goal).unwrap().state, GoalState::Ready);
        assert_eq!(s.attention_of(&name, cx), Attn::None);
        assert_eq!(s.attention_of(&goal, cx), Attn::Unread);
    });
}

#[gpui_kit::test]
async fn starts_fail_with_a_reason_and_the_floor_holds_them(cx: &mut TestAppContext) {
    use crate::usage::{Allowance, Provider};
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, _home) = start(cx, 3);
    let (goal, _lead) = f.planned(cx, PLAN).await;
    let allow = |s: &mut Shark, left: Option<f64>| {
        s.usage[Provider::Claude.index()].allowances = (left.into_iter())
            .map(|left| Allowance {
                label: "5h".into(),
                remaining: Some(left),
                resets_at: Some(crate::usage::now() + 3600.),
                window: Some(18000.),
            })
            .collect();
    };
    // Below the floor nothing starts and the row says why; with no reading it is unknown.
    f.on(cx, |s, w, cx| {
        s.update_team("growth", w, cx, |t| t.limits.floor = 20);
        allow(s, Some(10.));
        s.approve_goal(&goal, w, cx);
        assert_eq!(f.row_of(s, &goal, "T1").state, RowState::Queued);
        assert_eq!(
            s.row_hold(&goal, "T1"),
            Some(crate::team::Hold::Allowance(10))
        );
        assert!(s.allowance_reset("coder").is_some());
        allow(s, None);
        s.advance_goals(w, cx);
        assert_eq!(s.row_hold(&goal, "T1"), Some(crate::team::Hold::Unknown));
        assert_eq!(f.row_of(s, &goal, "T1").state, RowState::Queued);
        assert!(s.goals_running(), "usage keeps polling for it");
    });
    // An agent deleted since the plan was approved: the row is stuck with the reason, and
    // nothing was shown.
    f.on(cx, |s, w, cx| {
        let coder = s.state.agents.remove(1);
        allow(s, Some(50.));
        s.advance_goals(w, cx);
        let row = f.row_of(s, &goal, "T1");
        assert_eq!(row.state, RowState::Failed);
        assert_eq!(row.error.as_deref(), Some("Agent @coder was deleted"));
        assert_eq!(s.attention_of(&goal, cx), Attn::Needs(team::STUCK.into()));
        assert_eq!(s.active, None);
        // Back again: Retry queues it and it starts.
        s.state.agents.insert(1, coder);
        s.retry_row(&goal, "T1", w, cx);
    });
    f.row_chat(cx, &goal, "T1").await;
}

#[gpui_kit::test]
async fn deleting_a_team_or_goal_leaves_ordinary_tasks(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (f, home) = start(cx, 3);
    let (goal, lead) = f.planned(cx, "## Tasks\n- T1 @coder [api]: One\n").await;
    f.on(cx, |s, w, cx| s.approve_goal(&goal, w, cx));
    let t1 = f.row_chat(cx, &goal, "T1").await;
    let (part, branch) = f.on(cx, |s, _, _| {
        let name = s.row_task(&f.row_of(s, &goal, "T1")).unwrap().name.clone();
        (name, s.task(&goal).unwrap().branch.clone())
    });
    // An open goal keeps its team.
    f.on(cx, |s, _, cx| {
        let refused = s.delete_team("growth", cx);
        assert_eq!(refused, Err("Stop or finish the team's goals first".into()));
        assert_eq!(s.state.teams.len(), 1);
    });
    f.end_turn(cx, &t1, "Nothing to change.", 1);
    f.settled(cx, &goal, "T1", RowState::Finished).await;
    // The lead writes its summary: a goal whose lead is mid-turn can't be deleted.
    f.end_turn(cx, &lead, "One task, nothing to merge.", 1);

    // Deleting the goal keeps the task it started, as an ordinary task, and takes the
    // lead's empty branch with it, so the name is free again.
    let repo = home.join("src/api");
    let listed = |b: &str| git::git(&repo, &["branch", "--list", b]).unwrap();
    assert!(!listed(&branch).is_empty());
    f.on(cx, |s, w, cx| s.remove_task(&goal, false, w, cx));
    let shark = f.shark.clone();
    let gone = goal.clone();
    until(cx, f.window, move |_, cx| {
        shark.read(cx).task(&gone).is_none()
    })
    .await;
    cx.run_until_parked();
    assert!(
        listed(&branch).is_empty(),
        "the goal's empty branch is deleted"
    );
    f.on(cx, |s, _, cx| {
        let task = s.task(&part).unwrap();
        assert!(task.part.is_none(), "an ordinary task now");
        assert!(
            s.side_rows(false)
                .iter()
                .any(|r| { matches!(r, SideRow::Task(ix) if s.state.tasks[*ix].name == part) })
        );
        // With no open goal the team goes, and its log with it.
        assert!(!crate::team::activity("growth").is_empty());
        s.delete_team("growth", cx).unwrap();
        assert!(s.state.teams.is_empty());
        assert!(crate::team::activity("growth").is_empty());
    });
}
