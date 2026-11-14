//! Teams and goals: Shark runs the plan the user approved. Every start goes through
//! `advance_goals`, so pause and the limits have one gate. Nothing here opens a task, takes
//! focus or records interaction: the user is looking at something else.
use super::*;
use crate::chat::Outcome;
use crate::state::{AgentLink, Goal, GoalState, Part, Row, RowState, TaskRef, Team, Why};
use crate::team::{self, Activity};

/// How often goals are looked at when no turn, approval or setting change does it: an allowance
/// that reset, a setup script that failed.
pub(super) const TICK: std::time::Duration = std::time::Duration::from_secs(20);
const COMMIT_FAILED: &str = "Commit failed";
const QUIT: &str = "Interrupted when Shark quit";
const UNSENT: &str = "The first message wasn't sent";
const STOPPED: &str = "Stopped";
const CLOSED: &str = "Its chat was closed";
/// A goal still planning whose lead ended its turn without a plan, and a task of the stricter
/// approval whose agent did: each waits for a reply nothing else would show.
pub(crate) const LEAD_IDLE: &str = "Reply to the lead";
pub(crate) const AGENT_IDLE: &str = "Reply to the agent";
pub(crate) const STUCK: &str = "Stuck";
pub(crate) const LIMIT: &str = "Limit reached";
pub(crate) const REVIEW: &str = "Review to continue";

const RETRY: &str = "Continue this task from where it stopped. When it is complete, end your turn.";
const SUMMARY: &str = "Every task of the plan is finished. Write a short closing summary for the user: \
what was done per task, anything left open, and what to check before merging. Do not submit a new plan.";

/// What a goal's page shows for a row: `RowState` with the task's attention and PR state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RowStatus {
    Queued,
    Running,
    /// The label of what waits for the user.
    Needs(String),
    /// Finished and committed; yours to review.
    Ready,
    /// Merged, or finished with nothing to merge.
    Done,
    /// Why it stopped.
    Stuck(String),
    Skipped,
}

/// A team's status, derived from its goals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TeamStatus {
    Idle,
    Running(usize),
    Needs(usize),
    Paused,
}

/// A task for Shark to create in the background.
struct NewTask {
    name: String,
    branch: String,
    title: String,
    agent: AgentLink,
    repos: Vec<Repo>,
    /// `(repo, branch)`: start from that local branch instead of the trunk.
    local_parents: Vec<(String, String)>,
    plan: bool,
    goal: Option<Goal>,
    part: Option<Part>,
    first: String,
}

/// The task `spawn_task` made, and whether its first message went out.
struct Spawned {
    task: TaskRef,
    chat: String,
    sent: bool,
}

/// Whether any worktree has uncommitted changes, and whether any branch has commits of its own.
fn worktree_state(ws: &Task) -> (bool, bool) {
    let dir = ws.dir();
    let mut state = (false, false);
    for r in &ws.repos {
        let wt = dir.join(&r.name);
        // Unreadable counts as dirty: the commit turn, or its check, reports what is wrong.
        state.0 |= git::dirty(&wt).unwrap_or(true);
        // Commits on a detached HEAD or another branch aren't on the branch the next task starts from.
        let on = format!("refs/heads/{}", ws.branch_of(r));
        state.0 |= git::git(&wt, &["symbolic-ref", "--quiet", "HEAD"]).ok() != Some(on);
        let range = format!("{}..HEAD", r.base);
        state.1 |= git::git(&wt, &["rev-list", "--count", &range]).is_ok_and(|n| n != "0");
    }
    state
}

/// `source` as a slug, or `fallback` when nothing of it is left.
fn slug_or(source: &str, fallback: &str) -> String {
    match crate::state::slug(source) {
        s if s.is_empty() => fallback.to_string(),
        s => s,
    }
}

/// Where a goal with no task left in its plan stands: with a plan to approve, or back with its
/// lead. `None` while it has tasks.
fn unplanned(goal: &Goal) -> Option<GoalState> {
    let left = goal.rows.iter().any(|r| r.state != RowState::Skipped);
    (!left).then_some(match goal.draft.is_empty() {
        true => GoalState::Planning,
        false => GoalState::Review,
    })
}

/// What a row's agent is told first: its title, then its brief.
fn first_message(row: &Row) -> String {
    format!("{}\n\n{}", row.title, row.brief).trim().to_string()
}

fn attn_rank(a: &Attn) -> u8 {
    match a {
        Attn::None => 0,
        Attn::Unread => 1,
        Attn::Needs(_) => 2,
    }
}

impl Shark {
    pub(crate) fn team(&self, slug: &str) -> Option<&Team> {
        self.state.teams.iter().find(|t| t.slug == slug)
    }

    pub(crate) fn goal(&self, name: &str) -> Option<&Goal> {
        self.task(name)?.goal.as_ref()
    }

    fn goal_mut(&mut self, name: &str) -> Option<&mut Goal> {
        self.task_mut(name)?.goal.as_mut()
    }

    fn row(&self, goal: &str, key: &str) -> Option<&Row> {
        self.goal(goal)?.rows.iter().find(|r| r.key == key)
    }

    fn row_mut(&mut self, goal: &str, key: &str) -> Option<&mut Row> {
        (self.goal_mut(goal)?.rows.iter_mut()).find(|r| r.key == key)
    }

    /// The team's goals with their tasks, in creation order.
    pub(crate) fn team_goals(&self, slug: &str) -> impl Iterator<Item = (&Task, &Goal)> {
        (self.state.tasks.iter())
            .filter_map(move |t| Some((t, t.goal.as_ref().filter(|g| g.team == slug)?)))
    }

    /// The live task a started row points at: names are reused, so the instance must match.
    pub(crate) fn row_task(&self, row: &Row) -> Option<&Task> {
        let at = row.task.as_ref()?;
        self.task(&at.name)
            .filter(|t| t.instance_id == at.instance_id)
    }

    /// Saves a goal whose rows or state changed. Its chats read neither, so they aren't told.
    fn goal_changed(&mut self, _name: &str, cx: &mut Context<Self>) {
        let _ = self.state.save();
        self.refresh_attention(cx);
    }

    /// Saves a goal whose plan changed and hands it to its loaded chats: the lead's plan pane
    /// shows the draft and its problems.
    fn plan_changed(&mut self, name: &str, cx: &mut Context<Self>) {
        let _ = self.state.save();
        self.publish_task(name, cx);
    }

    /// Logs what a row's agent did.
    fn log_row(&self, goal: &str, key: &str, what: impl Into<String>) {
        let who = self.row(goal, key).map(|r| self.agent_name(&r.agent));
        self.log_goal(goal, key, &who.unwrap_or_default(), what);
    }

    fn log_goal(&self, goal: &str, key: &str, who: &str, what: impl Into<String>) {
        let Some(team) = self.goal(goal).map(|g| g.team.clone()) else {
            return;
        };
        let event = Activity {
            at: interaction::now_ms(),
            who: who.into(),
            what: what.into(),
            goal: goal.into(),
            key: key.into(),
        };
        team::log(&team, &event);
    }

    pub(crate) fn agent_name(&self, slug: &str) -> String {
        self.agent(slug)
            .map_or(slug.to_string(), |a| a.name.clone())
    }

    /// A new team. The lead needs repositories to read and at least one agent to plan for.
    pub(crate) fn create_team(
        &mut self,
        name: &str,
        lead: &str,
        agents: Vec<String>,
        repos: Vec<String>,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Name the team".into());
        }
        if self.agent(lead).is_none() {
            return Err("Pick a lead".into());
        }
        let mut others: Vec<String> = vec![];
        for a in agents {
            if a != lead && self.agent(&a).is_some() && !others.contains(&a) {
                others.push(a);
            }
        }
        if others.is_empty() {
            return Err("A team needs an agent besides its lead".into());
        }
        let repos: Vec<String> = (repos.into_iter())
            .filter(|n| self.state.repos.iter().any(|r| &r.name == n))
            .collect();
        if repos.is_empty() {
            return Err("Pick at least one repository".into());
        }
        let base = slug_or(name, "team");
        let slug = (1..)
            .map(|n| match n {
                1 => base.clone(),
                n => format!("{base}-{n}"),
            })
            .find(|s| self.team(s).is_none())
            .unwrap();
        self.state.teams.push(Team {
            slug: slug.clone(),
            name: name.into(),
            icon: "users".into(),
            lead: lead.into(),
            agents: others,
            repos,
            ..Default::default()
        });
        if let Err(e) = self.state.save() {
            self.state.teams.pop();
            return Err(format!("Couldn't save the team: {e}"));
        }
        Ok(slug)
    }

    /// Changes a team's settings. Open goals follow its task limit, and whatever the change
    /// allows starts.
    pub(crate) fn update_team(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Team),
    ) {
        let Some(team) = self.state.teams.iter_mut().find(|t| t.slug == slug) else {
            return;
        };
        f(team);
        let max = team.limits.tasks;
        // Its open goals follow: the lead's note names the limit, and what kept a plan from
        // being approved (too many tasks, an agent not on the team) may no longer hold.
        let goals: Vec<String> = (self.team_goals(slug))
            .filter(|(_, g)| g.state.open())
            .map(|(t, _)| t.name.clone())
            .collect();
        for name in &goals {
            self.goal_mut(name).unwrap().max_tasks = max;
            self.recheck(name);
        }
        let _ = self.state.save();
        for name in goals {
            self.publish_task(&name, cx);
        }
        self.advance_goals(window, cx);
        cx.notify();
    }

    /// Makes `agent` the lead; the old lead stays on the team.
    pub(crate) fn set_team_lead(
        &mut self,
        slug: &str,
        agent: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = agent.to_string();
        self.update_team(slug, window, cx, |t| {
            if t.lead == agent {
                return;
            }
            t.agents.retain(|a| *a != agent);
            let old = std::mem::replace(&mut t.lead, agent);
            t.agents.insert(0, old);
        });
    }

    /// Takes `agent` off the team. The lead stays, and so does the last other agent.
    pub(crate) fn remove_team_agent(
        &mut self,
        slug: &str,
        agent: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let team = self.team(slug).ok_or("The team is no longer available")?;
        if team.lead == agent {
            return Err("Make another agent the lead first");
        }
        if team.agents.iter().filter(|a| *a != agent).count() == 0 {
            return Err("A team needs an agent besides its lead");
        }
        let agent = agent.to_string();
        self.update_team(slug, window, cx, |t| {
            t.agents.retain(|a| *a != agent);
            t.paused_agents.retain(|a| *a != agent);
        });
        Ok(())
    }

    /// Deletes a team whose goals are all finished or stopped. Its goals and their tasks stay,
    /// as ordinary tasks.
    pub(crate) fn delete_team(&mut self, slug: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.team_goals(slug).any(|(_, g)| g.state.open()) {
            return Err("Stop or finish the team's goals first".into());
        }
        let goals: Vec<String> = self.team_goals(slug).map(|(t, _)| t.name.clone()).collect();
        for task in &mut self.state.tasks {
            if goals.contains(&task.name) {
                task.goal = None;
            }
            if task.part.as_ref().is_some_and(|p| goals.contains(&p.goal)) {
                task.part = None;
            }
        }
        // The cursor follows its team by slug: the indexes shift.
        let before = self.cursors(cx);
        self.state.teams.retain(|t| t.slug != slug);
        self.restore_cursors("", before, cx);
        // The log goes only once the team is gone from disk too.
        if self.state.save().is_ok() {
            team::forget(slug);
        }
        for name in goals {
            self.publish_task(&name, cx);
        }
        self.fix_sel();
        cx.notify();
        Ok(())
    }

    /// A free task name and its branch, `<prefix>-<sea name>`.
    fn new_task_name(&self, prefix: &str) -> (String, String) {
        let name = |sea: &str| format!("{prefix}-{sea}").replace('/', "-");
        let sea = crate::state::task_name(interaction::now_ms(), |sea| {
            let name = name(sea);
            self.creating.contains_key(&name)
                || self.task(&name).is_some()
                || tasks_dir().join(&name).exists()
                || crate::transcript::transcript_path(&name).exists()
        });
        (name(&sea), format!("{prefix}-{sea}"))
    }

    /// Creates `new` off the UI thread and sends its first message, without showing it. `done`
    /// hears how it went.
    fn spawn_task(
        &mut self,
        new: NewTask,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<Spawned, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        // Nothing is saved this run, so the worktrees would be orphaned on restart.
        if self.state.read_only {
            let e = "Can't create tasks: state.json couldn't be read".to_string();
            return done(self, Err(e), window, cx);
        }
        self.creating
            .insert(new.name.clone(), Some(new.agent.slug.clone()));
        let pending = new.name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let NewTask {
                name,
                branch,
                title,
                agent,
                repos,
                local_parents,
                plan,
                goal,
                part,
                first,
            } = new;
            let created = (cx.background_executor())
                .spawn(async move {
                    let dir = tasks_dir().join(&name);
                    git::create_row_task(
                        &dir,
                        &name,
                        &branch,
                        &repos,
                        &local_parents,
                        &title,
                        Some(agent),
                    )
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.creating.remove(&pending);
                let result = created.and_then(|mut ws| {
                    ws.goal = goal;
                    ws.part = part;
                    ws.chats[0].plan = plan;
                    s.adopt_task(ws, &first, window, cx)
                });
                done(s, result, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves a created task and starts its first chat. Unlike a task the user starts, it isn't
    /// opened, focused or recorded as interacted with.
    fn adopt_task(
        &mut self,
        mut ws: Task,
        first: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Spawned, String> {
        let agent = ws.agents.first().map(|a| a.slug.clone());
        let (model, effort) = self.state.defaults_for(&ws, agent.as_deref());
        ws.fill_models(&model, effort.as_ref());
        self.state.tasks.push(ws.clone());
        if let Err(e) = self.state.save() {
            self.state.tasks.pop();
            let cleanup = git::remove_task(&ws, &ws.dir(), false).err();
            let suffix = cleanup.map_or(String::new(), |e| format!("; cleanup: {e}"));
            return Err(format!("Couldn't save the new task: {e}{suffix}"));
        }
        if let Some(agent) = ws.agents.first() {
            // Settings may have changed while the worktrees were created.
            let docs = self
                .agent(&agent.slug)
                .is_some_and(|a| a.name != agent.name || a.instructions != agent.instructions);
            if let Err(e) = self.sync_agent(&agent.slug, docs, cx) {
                notify_err(window, cx, e);
            }
        }
        // A goal is titled like any task; a row keeps the title its lead gave it.
        if ws.goal.is_some() {
            self.auto_title(&ws.name, first, cx);
        }
        let ws = self.task(&ws.name).cloned().unwrap_or(ws);
        self.ensure_sync(&ws, window, cx);
        if self.active.as_deref() != Some(&ws.name)
            && let Some(sync) = self.syncs.get(&ws.name)
        {
            sync.update(cx, |g, cx| g.set_active(false, cx));
        }
        let chat = self.ensure_chat(&ws, &ws.chats[0], window, cx);
        let then = Some((first.to_string(), chat.clone()));
        let sent = self.run_setup(&ws, None, then, window, cx)
            || chat.update(cx, |c, cx| c.ask(first, window, cx));
        self.refresh_home(Some(&ws.name), cx);
        Ok(Spawned {
            task: TaskRef {
                name: ws.name.clone(),
                instance_id: ws.instance_id.clone(),
            },
            chat: ws.chats[0].id.clone(),
            sent,
        })
    }

    /// Flow 2: the team's lead starts planning `brief` in a task of its own, in plan mode.
    pub(crate) fn give_goal(
        &mut self,
        slug: &str,
        brief: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let brief = brief.trim();
        if brief.is_empty() {
            return;
        }
        let Some(team) = self.team(slug).cloned() else {
            return notify_err(window, cx, "The team is no longer available");
        };
        let Some(lead) = self.agent(&team.lead).map(AgentLink::from) else {
            return notify_err(
                window,
                cx,
                "The team's lead was deleted. Make another agent the lead.",
            );
        };
        let repos: Vec<Repo> = (team.repos.iter())
            .filter_map(|n| self.state.repos.iter().find(|r| &r.name == n).cloned())
            .collect();
        if repos.is_empty() {
            return notify_err(window, cx, "The team has no repository to read");
        }
        let roster: Vec<AgentLink> = (team.members())
            .filter_map(|s| self.agent(s).map(AgentLink::from))
            .collect();
        let title = crate::chat::title_of(brief);
        let source = slug_or(&title, "goal");
        let (name, branch) = self.new_task_name(&format!("{}/{source}", team.slug));
        let goal = Goal {
            team: team.slug.clone(),
            brief: brief.into(),
            roster,
            max_tasks: team.limits.tasks,
            ..Default::default()
        };
        window.push_notification(format!("{} is planning…", lead.name), cx);
        let new = NewTask {
            name,
            branch,
            title,
            agent: lead,
            repos,
            local_parents: vec![],
            plan: true,
            goal: Some(goal),
            part: None,
            first: brief.into(),
        };
        self.spawn_task(new, window, cx, |s, result, window, cx| match result {
            Ok(made) => {
                s.log_goal(&made.task.name, "", "You", "gave the goal");
                if !made.sent {
                    notify_err(
                        window,
                        cx,
                        "The lead didn't get the goal; open it to see why",
                    );
                }
            }
            Err(e) => notify_err(window, cx, e),
        });
    }

    /// The tightest allowance of `agent`'s harness; `None` when nothing is known.
    fn allowance(&self, agent: &str) -> Option<&crate::usage::Allowance> {
        use crate::usage::Provider;
        let model = (self.agent(agent).and_then(|a| a.model.clone()))
            .unwrap_or_else(|| self.state.new_model());
        let provider = match crate::codex::is_codex(Some(&model)) {
            true => Provider::Codex,
            false => Provider::Claude,
        };
        self.usage[provider.index()].limiting(crate::usage::now())
    }

    /// What is left of the plan allowance of `agent`'s harness, in percent.
    fn allowance_of(&self, agent: &str) -> Option<u8> {
        Some(self.allowance(agent)?.remaining?.clamp(0., 100.) as u8)
    }

    /// Why a queued row of `goal` waits, for the goal's page: a row it comes after, a review,
    /// its agent's pause or the allowance.
    pub(crate) fn row_hold(&self, goal: &str, key: &str) -> Option<team::Hold> {
        let g = self.goal(goal)?;
        let team = self.team(&g.team)?;
        let row = g.rows.iter().find(|r| r.key == key)?;
        team::hold(g, team, row, &|a: &str| self.allowance_of(a))
    }

    /// When the allowance `agent`'s harness waits for resets, in Unix seconds.
    pub(crate) fn allowance_reset(&self, agent: &str) -> Option<f64> {
        self.allowance(agent)?.resets_at
    }

    /// Whether a live row's chat ended a turn in plan mode: the user is next, with a plan to
    /// approve or a reply to give.
    fn row_waits(&self, row: &Row, cx: &App) -> bool {
        let setup = (row.task.as_ref()).is_some_and(|t| self.setting_up.contains(&t.name));
        !setup
            && (row.chat.as_ref().and_then(|id| self.chats.get(id))).is_some_and(|c| {
                let c = c.read(cx);
                !c.busy && c.meta.plan
            })
    }

    /// Whether a row holds one of the team's slots: it is live and not waiting for the user.
    /// A row between turns (its task being made, git being asked, a handoff on its way) keeps
    /// its slot, or another start would take it.
    fn row_running(&self, row: &Row, cx: &App) -> bool {
        row.state.live() && !self.row_waits(row, cx)
    }

    /// The team's rows that hold a slot. Not `running_chats`: that counts background shells, so
    /// a dev server would hold one.
    pub(crate) fn team_running(&self, slug: &str, cx: &App) -> usize {
        (self.team_goals(slug).flat_map(|(_, g)| &g.rows))
            .filter(|r| self.row_running(r, cx))
            .count()
    }

    /// Whether `chat`'s turn in task `name` is one Shark asked for: the work or commit turn of a
    /// plan row. Those end without a notice of their own; the goal reports.
    pub(crate) fn goal_drives(&self, name: &str, chat: &str) -> bool {
        let Some(part) = self.task(name).and_then(|t| t.part.as_ref()) else {
            return false;
        };
        (self.row(&part.goal, &part.key))
            .is_some_and(|r| r.state.live() && r.chat.as_deref() == Some(chat))
    }

    /// A goal is at work: usage keeps polling behind another window.
    pub(crate) fn goals_running(&self) -> bool {
        (self.state.tasks.iter()).any(|t| {
            t.goal
                .as_ref()
                .is_some_and(|g| g.state == GoalState::Running)
        })
    }

    /// The one gate for everything Shark starts. Called when a turn ends, a plan is approved, a
    /// setting changes, and on the attention tick.
    pub(crate) fn advance_goals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let goals: Vec<String> = (self.state.tasks.iter())
            .filter(|t| t.goal.is_some())
            .map(|t| t.name.clone())
            .collect();
        for name in goals {
            self.advance_goal(&name, window, cx);
        }
    }

    fn advance_goal(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(goal) = self.goal(name) else { return };
        // A goal on its way out starts nothing.
        if self.archiving.contains(name) {
            return;
        }
        // The lead was mid-turn when the last task finished: the summary is asked for now.
        if goal.state == GoalState::Ready && !goal.summary_asked {
            if self.ask_lead(name, SUMMARY, window, cx) {
                self.goal_mut(name).unwrap().summary_asked = true;
                let _ = self.state.save();
            }
            return;
        }
        if !matches!(goal.state, GoalState::Running | GoalState::Paused(_)) {
            return;
        }
        // Paused or not, and before the time limit: a plan that is done has nothing left to hold.
        if team::all_settled(goal) {
            self.goal_mut(name).unwrap().state = GoalState::Ready;
            self.log_goal(name, "", "Shark", "finished every task");
            self.goal_changed(name, cx);
            // One notice for the goal; its tasks' turns ended quietly (`Shark::turn_done`).
            self.unseen(name);
            self.notify(name, None, "Every task is finished: ready for review", cx);
            return self.advance_goal(name, window, cx);
        }
        if goal.state != GoalState::Running {
            return;
        }
        let Some(team) = self.team(&goal.team).cloned() else {
            return;
        };
        // A task whose setup failed, or whose agent couldn't start, never got its first message.
        let unsent: Vec<String> = (goal.rows.iter())
            .filter(|r| r.state == RowState::Started && r.handoff.is_none())
            .filter(|r| {
                let Some(task) = self.row_task(r) else {
                    return false;
                };
                let chat = r.chat.as_ref().and_then(|id| self.chats.get(id));
                !self.setting_up.contains(&task.name)
                    && chat.is_some_and(|c| {
                        let c = c.read(cx);
                        !c.busy && c.last_outcome.is_none() && c.meta.session_id.is_none()
                    })
            })
            .map(|r| r.key.clone())
            .collect();
        for key in &unsent {
            self.fail_row(name, key, UNSENT);
        }
        // A chat closed mid-turn reports no end: its row would hold a slot forever.
        let goal = self.goal(name).unwrap();
        let closed: Vec<String> = (goal.rows.iter())
            .filter(|r| r.state.live())
            .filter(|r| {
                let chats = self.row_task(r).map(|t| &t.chats);
                (r.chat.as_ref().zip(chats))
                    .is_some_and(|(id, chats)| !chats.iter().any(|c| &c.id == id))
            })
            .map(|r| r.key.clone())
            .collect();
        for key in &closed {
            self.fail_row(name, key, CLOSED);
        }
        if !unsent.is_empty() || !closed.is_empty() {
            self.goal_changed(name, cx);
        }
        let goal = self.goal(name).unwrap();
        if goal.secs >= team::time_limit(goal, &team) {
            self.goal_mut(name).unwrap().state = GoalState::Paused(Why::Time);
            self.log_goal(name, "", "Shark", "paused the goal: its agent time is used");
            return self.goal_changed(name, cx);
        }
        if team.paused {
            return;
        }
        // A handoff continues in the slot its row holds, so the running limit doesn't apply;
        // the next agent's pause and allowance do, as for any start.
        let handoffs: Vec<String> = (goal.rows.iter())
            .filter(|r| r.state == RowState::Started && r.chat.is_some())
            .filter(|r| {
                (r.handoff.as_deref()).is_some_and(|to| {
                    team::agent_hold(&team, to, &|a: &str| self.allowance_of(a)).is_none()
                })
            })
            .map(|r| r.key.clone())
            .collect();
        for key in handoffs {
            self.hand_off_row(name, &key, window, cx);
        }
        let goal = self.goal(name).unwrap();
        let running = self.team_running(&team.slug, cx);
        let starts = team::next_starts(goal, &team, running, |a| self.allowance_of(a));
        for key in starts {
            self.start_row(name, &key, window, cx);
        }
    }

    fn fail_row(&mut self, goal: &str, key: &str, why: &str) {
        if let Some(row) = self.row_mut(goal, key) {
            row.state = RowState::Failed;
            row.error = Some(why.into());
            (row.handoff, row.commit_failed) = (None, false);
        }
        self.log_row(goal, key, format!("is stuck: {why}"));
    }

    /// The row's commit turn didn't leave a commit: Retry asks for the commit again.
    fn fail_commit(&mut self, goal: &str, key: &str) {
        self.fail_row(goal, key, COMMIT_FAILED);
        if let Some(row) = self.row_mut(goal, key) {
            row.commit_failed = true;
        }
    }

    /// Stops what a task's chats are doing.
    fn stop_task(&mut self, name: &str, cx: &mut Context<Self>) {
        for chat in self.started_chats(name) {
            chat.update(cx, |c, cx| c.stop(cx));
        }
    }

    /// Starts a queued row as a task of its own: its branch from the trunk, or from the branch
    /// of the row it comes after in the same repository.
    fn start_row(&mut self, goal: &str, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.task(goal) else { return };
        let Some(g) = &task.goal else { return };
        let Some(row) = g.rows.iter().find(|r| r.key == key) else {
            return;
        };
        let plan = self.team(&g.team).is_some_and(|t| t.approve_tasks);
        let new = (|| {
            let agent = (self.agent(&row.agent).map(AgentLink::from))
                .ok_or_else(|| format!("Agent @{} was deleted", row.agent))?;
            let repos = (row.repos.iter())
                .map(|n| {
                    (self.state.repos.iter().find(|r| &r.name == n).cloned())
                        .ok_or_else(|| format!("Repository `{n}` is no longer registered"))
                })
                .collect::<Result<Vec<Repo>, String>>()?;
            let mut local_parents = vec![];
            for repo in &row.repos {
                let Some(parent) = team::base_row(&g.rows, row, repo) else {
                    continue;
                };
                let branch = self.row_task(parent).and_then(|t| {
                    let r = t.repos.iter().find(|r| &r.name == repo)?;
                    Some(t.branch_of(r).to_string())
                });
                let branch = branch.ok_or_else(|| format!("The task of {} is gone", parent.key))?;
                local_parents.push((repo.clone(), branch));
            }
            let after = (row.after.iter())
                .filter_map(|k| g.rows.iter().find(|r| &r.key == k))
                // A removed row's work isn't underneath, so its report says nothing true.
                .filter(|p| p.state == RowState::Finished)
                .filter_map(|p| Some((p.key.clone(), p.result.clone()?)))
                .collect();
            let source = slug_or(&row.title, "task");
            let (name, branch) = self.new_task_name(&format!("{}/{source}", agent.slug));
            Ok::<_, String>(NewTask {
                name,
                branch,
                title: crate::chat::title_of(&row.title),
                agent,
                repos,
                local_parents,
                plan,
                goal: None,
                part: Some(Part {
                    goal: goal.into(),
                    goal_title: task.display_title().into_owned(),
                    key: key.into(),
                    after,
                }),
                first: first_message(row),
            })
        })();
        let new = match new {
            Ok(new) => new,
            Err(e) => {
                self.fail_row(goal, key, &e);
                return self.goal_changed(goal, cx);
            }
        };
        // Started before the worktrees exist, so the running limit counts it at once.
        let row = self.row_mut(goal, key).unwrap();
        row.state = RowState::Started;
        row.error = None;
        let _ = self.state.save();
        let (goal, key) = (goal.to_string(), key.to_string());
        self.spawn_task(new, window, cx, move |s, result, _, cx| {
            match result {
                Ok(made) => {
                    let task = made.task.name.clone();
                    if let Some(row) = s.row_mut(&goal, &key) {
                        row.task = Some(made.task);
                        row.chat = Some(made.chat);
                    }
                    // Stopped or removed while its worktree was made: it doesn't go to work.
                    // ponytail: a first message waiting on setup scripts still goes out when they
                    // end; cancelling it needs `run_setup` to ask before it sends.
                    let wanted = s
                        .row(&goal, &key)
                        .is_some_and(|r| r.state == RowState::Started)
                        && s.goal(&goal).is_some_and(|g| g.state != GoalState::Stopped);
                    if !wanted {
                        s.stop_task(&task, cx);
                    } else if !made.sent {
                        s.fail_row(&goal, &key, UNSENT);
                    } else {
                        s.log_row(&goal, &key, "started");
                    }
                }
                Err(e) => s.fail_row(&goal, &key, &e),
            }
            s.goal_changed(&goal, cx);
        });
    }

    /// Phase 4: the next agent takes the row over in a new chat of the same task, with the
    /// transcript of the chat before it attached. Unlike `hand_off` it stays in the background
    /// and sends the first message itself.
    fn hand_off_row(&mut self, goal: &str, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.row(goal, key).cloned() else {
            return;
        };
        let (Some(to), Some(task)) = (row.handoff.clone(), self.row_task(&row).cloned()) else {
            return;
        };
        let from = (row.chat.as_ref()).and_then(|id| task.chats.iter().find(|c| &c.id == id));
        let (Some(from), true) = (from.cloned(), self.agent(&to).is_some()) else {
            self.fail_row(goal, key, "Handoff failed: the agent or chat is gone");
            return self.goal_changed(goal, cx);
        };
        // No chat while the handoff is on its way: a second tick doesn't hand it off twice, and
        // the row keeps its slot.
        self.row_mut(goal, key).unwrap().chat = None;
        let title = match from.title.trim() {
            "" => task.display_title().to_string(),
            t => t.to_string(),
        };
        let (dir, fast, source) = (task.dir(), from.fast, from.id.clone());
        let written = (cx.background_executor())
            .spawn(async move { handoff::write_transcript(&dir, &from.id, &title) });
        let (goal, key, name) = (goal.to_string(), key.to_string(), task.name.clone());
        let instance = task.instance_id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let written = written.await;
            this.update_in(cx, |s, window, cx| {
                // Stopped or removed while the transcript was written: nobody takes over.
                let wanted = (s.row(&goal, &key))
                    .is_some_and(|r| r.state == RowState::Started && r.handoff.is_some())
                    && s.goal(&goal).is_some_and(|g| g.state != GoalState::Stopped);
                if !wanted {
                    return;
                }
                let sent = written.and_then(|path| {
                    let live = |t: &&mut Task| t.instance_id == instance;
                    let ws = s.task_mut(&name).filter(live).ok_or("the task is gone")?;
                    let id = ws.next_chat_id();
                    let ws = s.task(&name).unwrap();
                    let meta = ChatMeta {
                        fast,
                        ..s.state.new_chat(ws, id.clone(), Some(to.clone()))
                    };
                    s.task_mut(&name).unwrap().push_chat(meta.clone());
                    let _ = s.relink_agents(&name, cx);
                    let ws = s.task(&name).cloned().unwrap();
                    let chat = s.ensure_chat(&ws, &meta, window, cx);
                    let first = format!(
                        "Take over this task: {}. The transcript of the agent before you is attached; continue from where it stopped.\n{}",
                        row.title,
                        crate::complete::mention(&path)
                    );
                    match chat.update(cx, |c, cx| c.ask(&first, window, cx)) {
                        true => Ok(id),
                        false => Err("the first message wasn't sent".to_string()),
                    }
                });
                match sent {
                    Ok(id) => {
                        if let Some(row) = s.row_mut(&goal, &key) {
                            (row.chat, row.handoff) = (Some(id), None);
                        }
                        let who = s.agent_name(&to);
                        s.log_goal(&goal, &key, &who, "took over");
                    }
                    // Retry goes to the agent that had it.
                    Err(e) => {
                        s.fail_row(&goal, &key, &format!("Handoff failed: {e}"));
                        if let Some(row) = s.row_mut(&goal, &key) {
                            row.chat = Some(source);
                        }
                    }
                }
                s.goal_changed(&goal, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The first chat of a goal's task is its lead's.
    fn lead_chat(
        &mut self,
        goal: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<Chat>> {
        let ws = self.task(goal)?.clone();
        let meta = ws.chats.first()?.clone();
        Some(self.ensure_chat(&ws, &meta, window, cx))
    }

    /// Gives the lead a turn, unless it is in one. Whether it went out.
    fn ask_lead(
        &mut self,
        goal: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chat) = self.lead_chat(goal, window, cx) else {
            return false;
        };
        chat.update(cx, |c, cx| !c.busy && c.ask(text, window, cx))
    }

    /// Recomputes what keeps the plan from being approved: what reading it turned up, then what
    /// `team::check` says of it as it would run.
    fn recheck(&mut self, name: &str) {
        let Some(task) = self.task(name) else { return };
        let Some(goal) = &task.goal else { return };
        let Some(team) = self.team(&goal.team) else {
            return;
        };
        let repos: Vec<String> = task.repos.iter().map(|r| r.name.clone()).collect();
        let limit = usize::from(team.limits.tasks);
        let mut problems = goal.unread.clone();
        for p in team::check(&team::proposed(goal), &team.agents, &repos, limit) {
            if !problems.contains(&p) {
                problems.push(p);
            }
        }
        self.goal_mut(name).unwrap().problems = problems;
    }

    /// Reads the lead's latest plan version into the draft, once per version: a later turn
    /// without a new one leaves the draft and the user's edits alone. Whether it read one.
    fn read_plan(&mut self, name: &str, chat: &Entity<Chat>, cx: &App) -> bool {
        let plan = {
            let p = chat.read(cx).plan.read(cx);
            let pending = (p.latest()).filter(|_| p.approved != p.versions.len().checked_sub(1));
            pending.map(|v| v.text.clone())
        };
        let Some(task) = self.task(name) else {
            return false;
        };
        let (Some(plan), Some(goal)) = (plan, &task.goal) else {
            return false;
        };
        // By what the plan says, not which version of which chat it is: the lead's chat can
        // be closed and another take its place.
        let read = crate::files::fingerprint(&plan);
        if goal.parsed == read {
            return false;
        }
        let repos: Vec<String> = task.repos.iter().map(|r| r.name.clone()).collect();
        let parsed = team::parse(&plan, &repos);
        let goal = self.goal_mut(name).unwrap();
        (goal.unread, goal.parsed) = (parsed.problems(), read);
        goal.draft = parsed.rows;
        self.recheck(name);
        true
    }

    /// A chat's turn ended in a goal's task, or in a task of a goal.
    pub(crate) fn goal_turn_done(
        &mut self,
        name: &str,
        chat: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.task(name) else { return };
        if task.goal.is_some() {
            self.lead_turn_done(name, chat, window, cx);
        } else if task.part.is_some() {
            self.part_turn_done(name, chat, window, cx);
        }
    }

    /// The lead submitted a plan, or wrote the closing summary.
    fn lead_turn_done(
        &mut self,
        name: &str,
        chat_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lead = self.task(name).and_then(|t| t.chats.first()).map(|c| &c.id);
        let Some(chat) = self
            .chats
            .get(chat_id)
            .filter(|_| lead.is_some_and(|l| l == chat_id))
        else {
            return;
        };
        let chat = chat.clone();
        let (outcome, secs) = {
            let c = chat.read(cx);
            (c.last_outcome, c.last_secs)
        };
        let Some(goal) = self.goal_mut(name) else {
            return;
        };
        goal.secs += secs;
        if outcome != Some(Outcome::Ended) {
            return self.goal_changed(name, cx);
        }
        // Before the plan: a summary turn that also resubmitted the plan still summarised.
        let reply = &chat.read(cx).last_reply;
        if goal.state == GoalState::Ready
            && goal.summary_asked
            && goal.summary.is_none()
            && !reply.trim().is_empty()
        {
            goal.summary = Some(reply.clone());
            self.log_goal(name, "", &self.lead_name(name), "wrote the summary");
        }
        if !self.read_plan(name, &chat, cx) {
            return self.goal_changed(name, cx);
        }
        let goal = self.goal_mut(name).unwrap();
        if goal.problems.is_empty() && team::same_plan(&team::proposed(goal), &goal.rows) {
            // Nothing new to approve: the lead resubmitted the plan that is running.
            goal.draft.clear();
            chat.update(cx, |c, cx| c.plan_approved(cx));
            return self.plan_changed(name, cx);
        }
        let fix = !goal.problems.is_empty() && !std::mem::replace(&mut goal.asked_fix, true);
        if goal.state == GoalState::Planning && !fix {
            goal.state = GoalState::Review;
        }
        let problems = goal.problems.join("\n- ");
        self.log_goal(name, "", &self.lead_name(name), "submitted a plan");
        self.plan_changed(name, cx);
        if fix {
            let text = format!(
                "Shark couldn't read the plan's tasks as written:\n- {problems}\n\nFix the `## Tasks` section and submit the plan again."
            );
            if !self.ask_lead(name, &text, window, cx)
                && let Some(goal) = self.goal_mut(name)
                && goal.state == GoalState::Planning
            {
                goal.state = GoalState::Review;
                self.goal_changed(name, cx);
            }
        }
    }

    fn lead_name(&self, goal: &str) -> String {
        (self.goal(goal).and_then(|g| g.roster.first()))
            .map_or("The lead".into(), |a| a.name.clone())
    }

    /// **Approve and start**: the draft becomes the plan Shark runs. The lead stays in plan
    /// mode and is sent nothing.
    pub(crate) fn approve_goal(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.goal(name).is_none() {
            return;
        }
        // A plan whose turn was cut short hasn't been read yet: what is approved is what shows.
        if let Some(chat) = self.lead_chat(name, window, cx) {
            self.read_plan(name, &chat, cx);
        }
        self.recheck(name);
        let goal = self.goal_mut(name).unwrap();
        if let Some(problem) = goal.problems.first() {
            let problem = format!("The plan can't start yet: {problem}");
            self.plan_changed(name, cx);
            return notify_err(window, cx, problem);
        }
        let first = matches!(goal.state, GoalState::Planning | GoalState::Review);
        if goal.draft.is_empty() && !first {
            return; // nothing waits
        }
        // A stopped goal is resumed, not approved back to work.
        if goal.state == GoalState::Stopped {
            return notify_err(window, cx, "The goal is stopped; resume it first");
        }
        goal.rows = team::proposed(goal);
        goal.draft.clear();
        goal.state = match goal.state {
            GoalState::Paused(why) => GoalState::Paused(why),
            _ => GoalState::Running,
        };
        // New work after the summary gets a summary of its own.
        goal.summary_asked = false;
        goal.summary = None;
        let n = goal.rows.len();
        if let Some(chat) = self.lead_chat(name, window, cx) {
            chat.update(cx, |c, cx| c.plan_approved(cx));
        }
        self.log_goal(
            name,
            "",
            "You",
            format!("approved the plan: {n} task{}", plural(n)),
        );
        self.plan_changed(name, cx);
        self.advance_goals(window, cx);
    }

    /// A turn ended in a task Shark started from a plan row.
    fn part_turn_done(
        &mut self,
        name: &str,
        chat_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(part) = self.task(name).and_then(|t| t.part.clone()) else {
            return;
        };
        let Some(chat) = self.chats.get(chat_id).cloned() else {
            return;
        };
        let (outcome, secs, reply, waiting, meta) = {
            let c = chat.read(cx);
            // Still planning (stricter approval): the user is next, not Shark.
            (
                c.last_outcome,
                c.last_secs,
                c.last_reply.clone(),
                c.meta.plan,
                c.meta.clone(),
            )
        };
        let (goal, key) = (part.goal.as_str(), part.key.as_str());
        let Some(g) = self.goal_mut(goal) else { return };
        g.secs += secs;
        let mine = |r: &&mut Row| {
            r.key == key
                && r.state.live()
                && r.chat.as_deref() == Some(chat_id)
                && r.task.as_ref().is_some_and(|t| t.name == name)
        };
        // Another chat of the task, or a turn after the row settled: the time counts, no more.
        let Some(row) = g.rows.iter_mut().find(mine) else {
            let _ = self.state.save();
            return self.advance_goals(window, cx);
        };
        if outcome != Some(Outcome::Ended) {
            let why = match outcome {
                Some(Outcome::Failed) => "The turn failed",
                Some(Outcome::Interrupted) => "Interrupted",
                _ => "The agent stopped",
            };
            match row.state {
                RowState::Committing => self.fail_commit(goal, key),
                _ => self.fail_row(goal, key, why),
            }
            self.goal_changed(goal, cx);
            // The lead may know another way; an interruption was somebody's decision.
            if outcome == Some(Outcome::Failed) {
                let text = format!(
                    "{key} is stuck: its agent's turn failed. Its last reply:\n\n{reply}\n\nIf the plan should change, submit it again; otherwise tell the user what to do."
                );
                self.ask_lead(goal, &text, window, cx);
            }
            return self.advance_goals(window, cx);
        }
        if waiting {
            return self.goal_changed(goal, cx);
        }
        if row.state == RowState::Started {
            // Now, not after the commit turn: its reply would replace what the next tasks need.
            row.result = Some(reply);
            // An agent that already worked here doesn't get it again, so a cycle ends.
            let next = self.handoff_target(&meta).filter(|next| {
                let on_team = (self.goal(goal).and_then(|g| self.team(&g.team)))
                    .is_some_and(|t| t.members().any(|a| a == next.slug));
                let fresh = (self.task(name))
                    .is_some_and(|t| !t.chat_agents().contains(&next.slug.as_str()));
                on_team && fresh
            });
            if let Some(next) = next {
                self.row_mut(goal, key).unwrap().handoff = Some(next.slug);
                self.goal_changed(goal, cx);
                return self.advance_goals(window, cx);
            }
        }
        let _ = self.state.save();
        let Some(ws) = self.task(name).cloned() else {
            return;
        };
        let (goal, key) = (goal.to_string(), key.to_string());
        cx.spawn_in(window, async move |this, cx| {
            let check = ws.clone();
            let state = (cx.background_executor())
                .spawn(async move { worktree_state(&check) })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.row_checked(&goal, &key, &ws, state, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn commit_prompt(ws: &Task) -> String {
        let pairs = ws.repos.iter().map(|r| (&*r.name, ws.branch_of(r)));
        crate::prompts::render_each("team_commit", pairs, |rs| rs.join(", "))
    }

    /// Git has answered for a row whose turn ended: commit what changed, or it is finished.
    pub(super) fn row_checked(
        &mut self,
        goal: &str,
        key: &str,
        ws: &Task,
        (dirty, ahead): (bool, bool),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row(goal, key).filter(|r| r.state.live()) else {
            return;
        };
        // Stopped while git was asked: nothing more is sent.
        if self
            .goal(goal)
            .is_some_and(|g| g.state == GoalState::Stopped)
        {
            self.fail_row(goal, key, STOPPED);
            return self.goal_changed(goal, cx);
        }
        let chat = row.chat.as_ref().and_then(|id| self.chats.get(id)).cloned();
        match (row.state, dirty) {
            // The commit is itself a turn; its end comes back through `part_turn_done`.
            (RowState::Started, true) => {
                let prompt = Self::commit_prompt(ws);
                let sent = chat.is_some_and(|c| c.update(cx, |c, cx| c.ask(&prompt, window, cx)));
                match sent {
                    true => self.row_mut(goal, key).unwrap().state = RowState::Committing,
                    false => self.fail_commit(goal, key),
                }
            }
            // A rejected pre-commit hook leaves changes behind; no commit leaves nothing ahead.
            (RowState::Committing, _) if dirty || !ahead => self.fail_commit(goal, key),
            _ => self.finish_row(goal, key),
        }
        self.goal_changed(goal, cx);
        self.advance_goals(window, cx);
    }

    fn finish_row(&mut self, goal: &str, key: &str) {
        let Some(row) = self.row_mut(goal, key) else {
            return;
        };
        row.state = RowState::Finished;
        row.error = None;
        self.log_row(goal, key, "finished");
    }

    /// After a restart no turn runs and nothing resumes by itself: a commit that landed is
    /// finished, every other row that was mid-turn is stuck until the user retries it.
    pub(crate) fn settle_goals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live: Vec<(String, Row)> = (self.state.tasks.iter())
            .filter_map(|t| Some((t.name.clone(), t.goal.as_ref()?)))
            .flat_map(|(name, g)| {
                (g.rows.iter())
                    .filter(|r| r.state.live())
                    .map(move |r| (name.clone(), r.clone()))
            })
            .collect();
        let failed = !live.is_empty();
        let mut checks = vec![];
        for (goal, row) in live {
            let Some(ws) = self.row_task(&row).cloned() else {
                self.fail_row(&goal, &row.key, QUIT);
                continue;
            };
            // Entities exist only for opened tasks: these need theirs to report turns and git.
            self.ensure_sync(&ws, window, cx);
            if let Some(sync) = self.syncs.get(&ws.name) {
                sync.update(cx, |g, cx| g.set_active(false, cx));
            }
            let meta = (row.chat.as_ref())
                .and_then(|id| ws.chats.iter().find(|c| &c.id == id))
                .cloned();
            // A plan that waits for approval was no turn: the row is where the user left it.
            let planned = (meta.as_ref()).is_some_and(|m| m.plan && crate::plan::unapproved(&m.id));
            if let Some(meta) = meta {
                self.ensure_chat(&ws, &meta, window, cx);
            }
            match row.state {
                RowState::Committing => checks.push((goal, row.key, ws)),
                _ if planned => {}
                _ => self.fail_row(&goal, &row.key, QUIT),
            }
        }
        // A summary that was being written is asked for again.
        let mut changed = !checks.is_empty();
        for goal in self.state.tasks.iter_mut().filter_map(|t| t.goal.as_mut()) {
            if goal.state == GoalState::Ready && goal.summary.is_none() && goal.summary_asked {
                (goal.summary_asked, changed) = (false, true);
            }
        }
        // Nothing was mid-turn: nothing to write, and nothing to wait for.
        if !changed && !failed {
            return self.advance_goals(window, cx);
        }
        let _ = self.state.save();
        cx.spawn_in(window, async move |this, cx| {
            let states = (cx.background_executor())
                .spawn(async move {
                    (checks.into_iter())
                        .map(|(goal, key, ws)| (goal, key, worktree_state(&ws)))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                for (goal, key, (dirty, ahead)) in states {
                    match !dirty && ahead {
                        true => s.finish_row(&goal, &key),
                        false => s.fail_row(&goal, &key, QUIT),
                    }
                }
                let _ = s.state.save();
                s.advance_goals(window, cx);
                s.refresh_attention(cx);
            })
            .ok();
        })
        .detach();
    }

    /// **Retry** a stuck row: a task that never started is queued again; one that did gets a
    /// message to go on, or the commit request again.
    pub(crate) fn retry_row(
        &mut self,
        goal: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row(goal, key).filter(|r| r.state == RowState::Failed) else {
            return;
        };
        if self
            .goal(goal)
            .is_some_and(|g| g.state == GoalState::Stopped)
        {
            return notify_err(window, cx, "The goal is stopped; resume it first");
        }
        let row = row.clone();
        let Some(ws) = self.row_task(&row).cloned() else {
            let row = self.row_mut(goal, key).unwrap();
            (row.state, row.error, row.task, row.chat) = (RowState::Queued, None, None, None);
            self.log_goal(goal, key, "You", "retried");
            self.goal_changed(goal, cx);
            return self.advance_goals(window, cx);
        };
        let meta = (row.chat.as_ref())
            .and_then(|id| ws.chats.iter().find(|c| &c.id == id))
            .or(ws.chats.first())
            .cloned();
        let Some(meta) = meta else { return };
        self.ensure_sync(&ws, window, cx);
        let chat = self.ensure_chat(&ws, &meta, window, cx);
        let commit = row.commit_failed;
        let text = match (commit, meta.session_id.is_some()) {
            (true, _) => Self::commit_prompt(&ws),
            (false, true) => RETRY.to_string(),
            (false, false) => first_message(&row),
        };
        if !chat.update(cx, |c, cx| !c.busy && c.ask(&text, window, cx)) {
            return notify_err(
                window,
                cx,
                format!("Couldn't message the agent of {key}; open its task to see why"),
            );
        }
        let slot = self.row_mut(goal, key).unwrap();
        slot.state = match commit {
            true => RowState::Committing,
            false => RowState::Started,
        };
        (slot.error, slot.commit_failed, slot.chat) = (None, false, Some(meta.id));
        self.log_goal(goal, key, "You", "retried");
        self.goal_changed(goal, cx);
    }

    /// **Start now**: a queued row starts whatever the limits say, once the rows it comes after
    /// are finished.
    pub(crate) fn start_row_now(
        &mut self,
        goal: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(g) = self.goal(goal) else { return };
        let Some(row) = g.rows.iter().find(|r| r.key == key) else {
            return;
        };
        if row.state != RowState::Queued {
            return;
        }
        if g.state == GoalState::Stopped {
            return notify_err(window, cx, "The goal is stopped; resume it first");
        }
        // The limits are the user's to override; a branch to start from is not.
        let waits = self
            .team(&g.team)
            .and_then(|t| team::parent_hold(g, t, row));
        if let Some(team::Hold::After(parent)) = waits {
            let msg = format!("{key} comes after {parent}, which isn't finished");
            return notify_err(window, cx, msg);
        }
        self.log_goal(goal, key, "You", "started it now");
        self.start_row(goal, key, window, cx);
        cx.notify();
    }

    /// Gives a queued row of the running plan another agent of the team (the Goal tab).
    pub(crate) fn reassign_row(
        &mut self,
        goal: &str,
        key: &str,
        agent: &str,
        cx: &mut Context<Self>,
    ) {
        let queued = |r: &&mut Row| r.key == key && r.state == RowState::Queued;
        let Some(row) = self
            .goal_mut(goal)
            .and_then(|g| g.rows.iter_mut().find(queued))
        else {
            return;
        };
        row.agent = agent.into();
        // A plan that waits would undo it when approved.
        let g = self.goal_mut(goal).unwrap();
        if let Some(row) = g.draft.iter_mut().find(|r| r.key == key) {
            row.agent = agent.into();
        }
        self.recheck(goal);
        self.plan_changed(goal, cx);
    }

    /// Gives a row of the plan that waits for approval another agent (the plan pane).
    pub(crate) fn reassign_draft_row(
        &mut self,
        goal: &str,
        key: &str,
        agent: &str,
        cx: &mut Context<Self>,
    ) {
        let row = (self.goal_mut(goal)).and_then(|g| g.draft.iter_mut().find(|r| r.key == key));
        let Some(row) = row else { return };
        row.agent = agent.into();
        self.recheck(goal);
        self.plan_changed(goal, cx);
    }

    /// Takes a row out of the plan that waits for approval (the plan pane); rows that came
    /// after it no longer wait for it.
    pub(crate) fn remove_draft_row(&mut self, goal: &str, key: &str, cx: &mut Context<Self>) {
        let Some(g) = self.goal_mut(goal) else { return };
        g.draft.retain(|r| r.key != key);
        for r in &mut g.draft {
            r.after.retain(|k| k != key);
        }
        self.recheck(goal);
        self.plan_changed(goal, cx);
    }

    /// **Remove from plan** in the running plan (the Goal tab): a queued row goes; a started
    /// one is skipped and its agent stopped, and its task stays. Rows that came after it no
    /// longer wait for it.
    pub(crate) fn remove_row(
        &mut self,
        goal: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(g) = self.goal_mut(goal) else { return };
        let rows = &mut g.rows;
        let Some(at) = rows.iter().position(|r| r.key == key) else {
            return;
        };
        // Only a row at work has a turn to stop; a second stop would kill a process that is
        // already winding down.
        let started = rows[at].task.clone().filter(|_| rows[at].state.live());
        match rows[at].state {
            RowState::Queued => _ = rows.remove(at),
            _ => {
                rows[at].state = RowState::Skipped;
                (rows[at].handoff, rows[at].result) = (None, None);
            }
        }
        let queued = !rows.iter().any(|r| r.key == key);
        for r in rows.iter_mut() {
            r.after.retain(|k| k != key);
        }
        // A plan that waits would bring a removed queued row back when approved.
        if queued {
            g.draft.retain(|r| r.key != key);
            for r in &mut g.draft {
                r.after.retain(|k| k != key);
            }
        }
        if g.state == GoalState::Running
            && let Some(state) = unplanned(g)
        {
            g.state = state;
        }
        if let Some(task) = started {
            self.stop_task(&task.name, cx);
        }
        self.recheck(goal);
        self.log_goal(goal, key, "You", "removed it from the plan");
        self.plan_changed(goal, cx);
        self.advance_goals(window, cx);
    }

    /// Stricter approval: the user has looked at a finished row, so rows after it may start.
    pub(crate) fn mark_reviewed(
        &mut self,
        goal: &str,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.row_mut(goal, key) else {
            return;
        };
        row.reviewed = true;
        self.log_goal(goal, key, "You", "reviewed");
        self.goal_changed(goal, cx);
        self.advance_goals(window, cx);
    }

    /// Pause, Resume, Add 1 h and Keep paused all set the goal's state and let what may start.
    fn set_goal_state(
        &mut self,
        name: &str,
        what: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Goal),
    ) {
        let Some(goal) = self.goal_mut(name) else {
            return;
        };
        let before = (goal.state, goal.extra_minutes);
        f(goal);
        if before == (goal.state, goal.extra_minutes) {
            return;
        }
        self.log_goal(name, "", "You", what);
        self.goal_changed(name, cx);
        self.advance_goals(window, cx);
    }

    /// Starts nothing new; running turns finish.
    pub(crate) fn pause_goal(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_goal_state(name, "paused the goal", window, cx, |g| {
            if g.state == GoalState::Running {
                g.state = GoalState::Paused(Why::You);
            }
        });
    }

    pub(crate) fn resume_goal(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_goal_state(name, "resumed the goal", window, cx, |g| {
            if matches!(g.state, GoalState::Paused(_) | GoalState::Stopped) {
                g.state = unplanned(g).unwrap_or(GoalState::Running);
            }
        });
    }

    /// **Add 1 h** at the time limit: this goal gets 60 more minutes and goes on.
    pub(crate) fn add_goal_hour(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_goal_state(name, "added 1 h", window, cx, |g| {
            g.extra_minutes += 60;
            if g.state == GoalState::Paused(Why::Time) {
                g.state = GoalState::Running;
            }
        });
    }

    /// **Keep paused** at the time limit: the item goes, the goal stays paused.
    pub(crate) fn keep_goal_paused(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_goal_state(name, "kept the goal paused", window, cx, |g| {
            if g.state == GoalState::Paused(Why::Time) {
                g.state = GoalState::Paused(Why::You);
            }
        });
    }

    /// Interrupts every running chat of the goal, the lead's too, and starts nothing more.
    pub(crate) fn stop_goal(&mut self, name: &str, cx: &mut Context<Self>) {
        // A goal that is ready, done or stopped has nothing to stop, and keeps its state.
        let at_work = |g: &&mut Goal| g.state.open() && g.state != GoalState::Ready;
        let Some(goal) = self.goal_mut(name).filter(at_work) else {
            return;
        };
        goal.state = GoalState::Stopped;
        let live: Vec<(String, Option<String>)> = (goal.rows.iter())
            .filter(|r| r.state.live())
            .map(|r| (r.key.clone(), r.task.as_ref().map(|t| t.name.clone())))
            .collect();
        for (key, task) in live {
            // A turn that runs ends interrupted and fails its row; one that doesn't (its task
            // still being made, a plan or handoff waiting) is stopped here.
            let busy = (task.as_ref())
                .is_some_and(|t| (self.started_chats(t).iter()).any(|c| c.read(cx).busy));
            match (busy, task) {
                (true, Some(task)) => self.stop_task(&task, cx),
                _ => self.fail_row(name, &key, STOPPED),
            }
        }
        self.stop_task(name, cx);
        self.log_goal(name, "", "You", "stopped the goal");
        self.goal_changed(name, cx);
    }

    /// **Merged · finish goal**: the goal moves to the team's finished goals.
    pub(crate) fn finish_goal(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(goal) = self.goal_mut(name) else {
            return;
        };
        goal.state = GoalState::Done;
        self.log_goal(name, "", "You", "finished the goal");
        self.goal_changed(name, cx);
    }

    /// Whether a finished row's work has landed: every repository merged, or nothing to merge.
    fn row_landed(&self, row: &Row, cx: &App) -> bool {
        let Some(task) = self.row_task(row) else {
            return false;
        };
        (self.overview_repos(&task.name, cx)).is_some_and(|repos| {
            !repos.is_empty()
                && (repos.iter()).all(|r| {
                    let ahead = r.local.as_ref().map_or(1, |l| l.base.0);
                    r.landed() || (ahead == 0 && r.pr().is_none())
                })
        })
    }

    /// Every finished task of a ready goal has landed: the goal offers **Merged · finish goal**.
    pub(crate) fn goal_merged(&self, name: &str, cx: &App) -> bool {
        self.goal(name).is_some_and(|g| {
            g.state == GoalState::Ready
                && (g.rows.iter())
                    .filter(|r| r.state == RowState::Finished)
                    .all(|r| self.row_landed(r, cx))
        })
    }

    /// A chat of `name` ended its turn in plan mode with a plan nobody approved yet.
    pub(crate) fn plan_waiting(&self, name: &str, cx: &App) -> bool {
        (self.started_chats(name).iter()).any(|c| {
            let c = c.read(cx);
            let p = c.plan.read(cx);
            !c.busy
                && c.meta.plan
                && !p.versions.is_empty()
                && p.approved != Some(p.versions.len() - 1)
        })
    }

    /// What a goal or one of a goal's tasks waits on, before the checks every task gets.
    pub(crate) fn goal_needs(&self, name: &str, cx: &App) -> Option<String> {
        let task = self.task(name)?;
        if let Some(part) = &task.part {
            // Stricter approval: its agent ended a turn in plan mode, with a plan or without.
            let row = self.row(&part.goal, &part.key).filter(|r| r.state.live())?;
            return self
                .row_waits(row, cx)
                .then(|| match self.plan_waiting(name, cx) {
                    true => crate::transcript::PLAN_TO_APPROVE.to_string(),
                    false => AGENT_IDLE.to_string(),
                });
        }
        let goal = task.goal.as_ref()?;
        let team = self.team(&goal.team)?;
        let open = goal.state.open();
        let review = || {
            (goal.rows.iter()).any(|r| {
                r.state == RowState::Queued
                    && matches!(
                        team::parent_hold(goal, team, r),
                        Some(team::Hold::Review(_))
                    )
            })
        };
        // Still planning, and the lead isn't: it asked something, failed, or Shark quit.
        let lead_idle = || {
            let chat = task.chats.first().and_then(|c| self.chats.get(&c.id));
            !self.setting_up.contains(&task.name) && !chat.is_some_and(|c| c.read(cx).busy)
        };
        if goal.state == GoalState::Paused(Why::Time) {
            Some(LIMIT.into())
        } else if open && goal.rows.iter().any(|r| r.state == RowState::Failed) {
            Some(STUCK.into())
        } else if goal.state == GoalState::Review || (open && !goal.draft.is_empty()) {
            Some(crate::transcript::PLAN_TO_APPROVE.into())
        } else if goal.state == GoalState::Planning && lead_idle() {
            Some(LEAD_IDLE.into())
        } else if open && review() {
            Some(REVIEW.into())
        } else {
            None
        }
    }

    /// What a goal shows: the highest of its own attention and its started tasks'.
    pub(crate) fn goal_attention(&self, name: &str, cx: &App) -> Attn {
        let own = self.attention_of(name, cx);
        let rows = self.goal(name).map_or(&[][..], |g| &g.rows);
        (rows.iter())
            .filter_map(|r| self.row_task(r))
            .map(|t| self.attention_of(&t.name, cx))
            .fold(
                own,
                |a, b| if attn_rank(&b) > attn_rank(&a) { b } else { a },
            )
    }

    /// Items of a goal that wait for the user: its own and one per task that needs them.
    pub(crate) fn goal_needs_count(&self, name: &str, cx: &App) -> usize {
        let needs = |n: &str| matches!(self.attention_of(n, cx), Attn::Needs(_));
        let rows = self.goal(name).map_or(&[][..], |g| &g.rows);
        usize::from(needs(name))
            + (rows.iter())
                .filter_map(|r| self.row_task(r))
                .filter(|t| needs(&t.name))
                .count()
    }

    pub(crate) fn team_status(&self, slug: &str, cx: &App) -> TeamStatus {
        let needs: usize = (self.team_goals(slug))
            .map(|(t, _)| self.goal_needs_count(&t.name, cx))
            .sum();
        let running = (self.team_goals(slug))
            .filter(|(_, g)| matches!(g.state, GoalState::Planning | GoalState::Running))
            .count();
        match self.team(slug) {
            _ if needs > 0 => TeamStatus::Needs(needs),
            Some(t) if t.paused => TeamStatus::Paused,
            _ if running > 0 => TeamStatus::Running(running),
            _ => TeamStatus::Idle,
        }
    }

    pub(crate) fn row_status(&self, row: &Row, cx: &App) -> RowStatus {
        let needs = || {
            let task = self.row_task(row)?;
            match self.attention_of(&task.name, cx) {
                Attn::Needs(label) => Some(RowStatus::Needs(label)),
                _ => None,
            }
        };
        match row.state {
            RowState::Queued => RowStatus::Queued,
            RowState::Skipped => RowStatus::Skipped,
            RowState::Failed => RowStatus::Stuck(row.error.clone().unwrap_or(STUCK.into())),
            RowState::Started | RowState::Committing => needs().unwrap_or(RowStatus::Running),
            RowState::Finished => needs().unwrap_or(match self.row_landed(row, cx) {
                true => RowStatus::Done,
                false => RowStatus::Ready,
            }),
        }
    }

    /// **Create draft PRs** for a goal: each finished task's, parents first, so a task stacked
    /// on another finds its base on GitHub and the PRs stack.
    pub(crate) fn create_goal_prs(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(goal) = self.goal(name) else { return };
        let tasks: Vec<Task> = (team::in_order(&goal.rows).into_iter())
            .filter(|r| r.state == RowState::Finished)
            .filter_map(|r| self.row_task(r))
            // One that has a PR is left to its own Git button.
            .filter(|t| {
                !(self.overview_repos(&t.name, cx))
                    .is_some_and(|repos| repos.iter().any(|r| r.pr().is_some()))
            })
            .cloned()
            .collect();
        if tasks.is_empty() {
            return notify_err(window, cx, "No finished task is waiting for a pull request");
        }
        if let Some(busy) = tasks.iter().find(|t| self.busy(&t.name, cx)) {
            let msg = format!("{} is still running", busy.display_title());
            return notify_err(window, cx, msg);
        }
        window.push_notification("Creating draft PRs…", cx);
        let goal = name.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let names: Vec<String> = tasks.iter().map(|t| t.name.clone()).collect();
            let results = (cx.background_executor())
                .spawn(async move {
                    let new = crate::pr::NewPr::new("", true);
                    (tasks.iter())
                        .map(|t| {
                            let made = crate::pr::create_linked_prs(t, &t.dir(), &[], &new);
                            (t.display_title().into_owned(), made)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                let (mut made, mut errors) = (0, vec![]);
                for (title, result) in results {
                    match result {
                        Ok((prs, warning)) => {
                            made += prs.len();
                            errors.extend(warning);
                        }
                        // A task that changed nothing has nothing to open.
                        Err(e) if e.contains("no repo has commits ahead") => {}
                        Err(e) => errors.push(format!("{title}: {e}")),
                    }
                }
                if made > 0 {
                    s.state.mark(|st| &mut st.opened_pr);
                    s.log_goal(
                        &goal,
                        "",
                        "You",
                        format!("created {made} draft PR{}", plural(made)),
                    );
                    window.push_notification(
                        Notification::success(format!("Created {made} draft PR{}", plural(made))),
                        cx,
                    );
                }
                if !errors.is_empty() {
                    notify_err(window, cx, errors.join("\n"));
                }
                for name in names {
                    if let Some(sync) = s.syncs.get(&name).cloned() {
                        sync.update(cx, |g, cx| g.fetch_now(window, cx));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A deleted task leaves its goal: a row's task is skipped; a goal's tasks become ordinary.
    pub(crate) fn task_left_goal(&mut self, gone: &Task) {
        if let Some(part) = &gone.part
            && let Some(goal) = self.goal_mut(&part.goal)
        {
            let mine = |r: &&mut Row| {
                r.key == part.key
                    && (r.task.as_ref())
                        .is_some_and(|t| t.name == gone.name && t.instance_id == gone.instance_id)
            };
            if let Some(row) = goal.rows.iter_mut().find(mine) {
                row.state = RowState::Skipped;
                (row.handoff, row.result) = (None, None);
            }
        }
        if gone.goal.is_some() {
            for task in &mut self.state.tasks {
                if task.part.as_ref().is_some_and(|p| p.goal == gone.name) {
                    task.part = None;
                }
            }
        }
    }

    /// Deleting a goal asks what happens to the tasks it started.
    pub(super) fn archive_goal(
        &mut self,
        name: String,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parts = self.goal_tasks(&name);
        let n = parts.len();
        let items = vec![
            CommandItem::new()
                .label(format!("Delete the goal and its {n} task{}", plural(n)))
                .icon(IconName::Trash),
            CommandItem::new()
                .label(format!(
                    "Delete the goal; keep its {n} task{} as ordinary tasks",
                    plural(n)
                ))
                .icon(IconName::CircleDashed),
        ];
        open_menu(
            cx.entity().downgrade(),
            "Delete the goal, its plan and the lead's conversation…",
            items,
            1,
            move |s, row, w, cx| {
                // Nothing new starts while the removals run.
                s.stop_goal(&name, cx);
                if row == 0 {
                    for part in &parts {
                        s.remove_task(part, force, w, cx);
                    }
                }
                s.remove_task(&name, force, w, cx);
            },
            window,
            cx,
        );
    }

    /// The tasks a goal started that still exist.
    pub(crate) fn goal_tasks(&self, name: &str) -> Vec<String> {
        let rows = self.goal(name).map_or(&[][..], |g| &g.rows);
        (rows.iter())
            .filter_map(|r| self.row_task(r))
            .map(|t| t.name.clone())
            .collect()
    }
}
