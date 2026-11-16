//! The Overview: a section's chat over the section's tasks. It starts, messages, reads and stops
//! them through Shark's MCP server (`crate::mcp`), and only when the user messages it. Nothing
//! here opens a task, takes focus or records interaction: the user is looking at something else.
use super::*;
use crate::chat::{Glyph, News, NewsKind, SectionTask, one_line};
use crate::mcp::{self, MESSAGE, READ, SPAWN, STOP};
use crate::state::AgentLink;
use serde_json::{Value, json};

/// A task for Shark to create in the background.
struct NewTask {
    name: String,
    branch: String,
    title: String,
    agent: AgentLink,
    repos: Vec<Repo>,
    /// `(repo, branch)`: start from that local branch instead of the trunk.
    local_parents: Vec<(String, String)>,
    first: String,
}

/// A task an Overview asked for whose worktrees are still being made.
pub(super) struct Starting {
    name: String,
    /// The Overview's task, which names the section when the new task is ready.
    overview: String,
    title: String,
    agent: AgentLink,
}

/// A reply in a list of tasks; one task read alone gets all of `REPLY_CAP`.
const LISTED_REPLY: usize = 400;
/// The most of a last reply `read_task` returns: the transcript holds the rest.
const REPLY_CAP: usize = 8000;

const NOT_AN_OVERVIEW: &str =
    "This chat is not a section's Overview any more, so it has no tasks to act on.";

/// The tracked lines a task changed, summed over its repositories; `None` before every one is
/// known.
pub(super) fn task_diff(repos: Option<&[crate::sync::Repo]>) -> Option<(u32, u32)> {
    repos?.iter().try_fold((0u32, 0u32), |(a, d), r| {
        let (add, del) = r.local.as_ref().ok()?.diff?;
        Some((a.saturating_add(add), d.saturating_add(del)))
    })
}

fn diff_text(diff: Option<(u32, u32)>) -> String {
    diff.map_or(String::new(), |(a, d)| format!("+{a} −{d}"))
}

fn slug_or(source: &str, fallback: &str) -> String {
    match crate::state::slug(source) {
        s if s.is_empty() => fallback.to_string(),
        s => s,
    }
}

fn outcome_text(outcome: Option<crate::chat::Outcome>) -> &'static str {
    use crate::chat::Outcome::*;
    match outcome {
        Some(Ended) | None => "its turn ended",
        Some(Failed) => "its turn failed",
        Some(Interrupted) => "it was stopped",
        Some(Exited) => "its process went away mid-turn",
    }
}

impl Shark {
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
    /// hears how it went: whether the first message went out, or why there is no task.
    fn spawn_task(
        &mut self,
        new: NewTask,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<bool, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
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
                first,
            } = new;
            let created = (cx.background_executor())
                .spawn(async move {
                    let dir = tasks_dir().join(&name);
                    git::create_stacked_task(
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
                let result = created.and_then(|ws| s.adopt_task(ws, &first, window, cx));
                done(s, result, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves a created task and starts its first chat; returns whether the first message went
    /// out. Unlike a task the user starts, it isn't opened, focused or recorded as interacted
    /// with.
    fn adopt_task(
        &mut self,
        mut ws: Task,
        first: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
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
        let ws = self.task(&ws.name).cloned().unwrap_or(ws);
        self.ensure_sync(&ws, window, cx);
        if self.active.as_deref() != Some(&ws.name)
            && let Some(sync) = self.syncs.get(&ws.name)
        {
            sync.update(cx, |g, cx| g.set_active(false, cx));
        }
        let chat = self.ensure_chat(&ws, &ws.chats[0], window, cx);
        // Its brief is the Overview's message: labelled, and no news of the user's.
        chat.update(cx, |c, _| c.overview_brief = true);
        let then = Some((first.to_string(), chat.clone()));
        let sent = self.run_setup(&ws, None, then, window, cx)
            || chat.update(cx, |c, cx| c.ask(first, window, cx));
        self.refresh_home(Some(&ws.name), cx);
        Ok(sent)
    }

    /// Stops what a task's chats are doing.
    fn stop_task(&mut self, name: &str, cx: &mut Context<Self>) {
        for chat in self.started_chats(name) {
            chat.update(cx, |c, cx| c.stop(cx));
        }
    }

    /// Shows section `i`'s Overview. The first time, its task is only made in memory: nothing
    /// is saved or written until a message goes out (`ChatEvent::Born`).
    pub(super) fn open_overview(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(section) = self.state.sections.get(i) else {
            return;
        };
        let name = match self.state.overview_of(i) {
            Some(name) => name.to_string(),
            None => {
                let (name, _) = self.new_task_name("overview");
                let mut ws = Task {
                    name: name.clone(),
                    branch: name.clone(),
                    title: overview_title(&section.name),
                    ..Default::default()
                };
                ws.migrate();
                // Its tab says what it is, not what its first message was.
                ws.chats[0].title = "Overview".into();
                let (model, effort) = self.state.defaults_for(&ws, None);
                ws.fill_models(&model, effort.as_ref());
                self.state.draft_overview(i, ws);
                name
            }
        };
        self.open_task_named(&name, window, cx);
    }

    /// The section whose Overview the open task is.
    pub(super) fn shown_overview(&self) -> Option<usize> {
        self.state.overview_section(self.active.as_deref()?)
    }

    /// Whether `name` is one of the tasks section `i`'s Overview runs.
    fn in_section(&self, i: usize, name: &str) -> bool {
        self.state.sections[i].tasks.iter().any(|n| n == name) && self.task(name).is_some()
    }

    /// The names of what an Overview runs: its section's tasks that exist, in order.
    fn section_task_names(&self, i: usize) -> Vec<String> {
        (self.state.sections[i].tasks.iter())
            .filter(|n| self.state.tasks.iter().any(|t| &t.name == *n))
            .cloned()
            .collect()
    }

    /// Section `i`'s tasks as an Overview's rows show them, with the ones still being made.
    pub(super) fn section_tasks(&self, i: usize, cx: &App) -> Vec<SectionTask> {
        let overview = self.state.sections[i].overview.as_deref();
        let listed = self.section_task_names(i).into_iter().filter_map(|name| {
            let task = self.task(&name)?;
            let facts = self.task_facts(task, cx);
            let agent = (task.chats.first())
                .and_then(|c| task.agent_link(c.agent.as_deref()?))
                .map(|a| (a.icon.clone(), a.name.clone().into()));
            Some(SectionTask {
                title: task.display_title().into_owned().into(),
                glyph: match facts.bucket {
                    home::Bucket::Running => Glyph::Running,
                    home::Bucket::Needs => Glyph::Needs,
                    home::Bucket::Unread => Glyph::Unread,
                    home::Bucket::Idle => Glyph::Idle,
                    home::Bucket::Backlog => Glyph::Backlog,
                },
                agent,
                diff: diff_text(task_diff(facts.repos)).into(),
                name,
            })
        });
        let starting = (self.starting.iter())
            .filter(|s| Some(s.overview.as_str()) == overview)
            .map(|s| SectionTask {
                name: s.name.clone(),
                title: s.title.clone().into(),
                glyph: Glyph::Running,
                agent: Some((s.agent.icon.clone(), s.agent.name.clone().into())),
                diff: SharedString::default(),
            });
        listed.chain(starting).collect()
    }

    /// Before a frame that shows an Overview: its rows read the section as it is now.
    pub(super) fn sync_overview(&mut self, cx: &mut Context<Self>) {
        let Some(i) = self.shown_overview() else {
            return;
        };
        let tasks = self.section_tasks(i, cx);
        if let Some(chat) = self.active_chat().cloned() {
            chat.update(cx, |c, cx| c.set_section(tasks, cx));
        }
    }

    /// Section `i` at a glance: chats running, tasks that need the user, and the agent time
    /// spent on it since Shark started. Its Overview counts too.
    pub(super) fn section_readout(&self, i: usize, cx: &App) -> (usize, usize, u64) {
        let mut names = self.section_task_names(i);
        let tasks = names.len();
        names.extend(self.state.overview_of(i).map(str::to_string));
        let (mut running, mut needs, mut secs) = (0, 0, 0);
        for (n, name) in names.iter().enumerate() {
            let Some(task) = self.task(name) else {
                continue;
            };
            running += self.running_chats(name, cx);
            needs += usize::from(n < tasks && !task.backlog && self.task_facts(task, cx).warning);
            secs += self.agent_secs.get(name).copied().unwrap_or(0);
            secs += (self.started_chats(name).iter())
                .map(|c| c.read(cx).turn_secs())
                .sum::<u64>();
        }
        (running, needs, secs)
    }

    /// An Overview's header: its section, the section at a glance, and Stop all while anything
    /// runs. No Run, Git or repository controls: it has no repositories of its own.
    pub(super) fn render_overview_head(&self, i: usize, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let section = &self.state.sections[i];
        let (running, needs, secs) = self.section_readout(i, cx);
        let tasks = self.section_task_names(i).len();
        let mut facts: Vec<AnyElement> = vec![];
        if running > 0 {
            facts.push(format!("{running} running").into_any_element());
        }
        if needs > 0 {
            let text = format!("{needs} need{} you", if needs == 1 { "s" } else { "" });
            facts.push(
                div()
                    .text_color(theme.warning_foreground)
                    .child(text)
                    .into_any_element(),
            );
        }
        if facts.is_empty() {
            facts.push(format!("{tasks} task{}", crate::ui::plural(tasks)).into_any_element());
        } else if secs >= 60 {
            facts.push(crate::ui::hm(secs).into_any_element());
        }
        let mut readout = h_flex()
            .id("overview-readout")
            .test_support()
            .flex_none()
            .gap_1()
            .pr_2()
            .text_xs()
            .text_color(theme.sidebar_foreground);
        for (n, fact) in facts.into_iter().enumerate() {
            readout = readout.when(n > 0, |d| d.child("·")).child(fact);
        }
        h_flex()
            .h(px(44.))
            .flex_none()
            .px(px(6.))
            .gap(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .pl_2()
                    .gap(px(6.))
                    .min_w_0()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(sidebar::section_icon(&section.icon)).with_size(px(14.)))
                    .child(div().truncate().child(section.name.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .id("overview-toolbar-empty")
                    .map(|d| self.draggable_chrome("overview-toolbar-empty", d)),
            )
            .child(readout)
            .when(running > 0, |d| {
                d.child(
                    Button::new("stop-all")
                        .secondary()
                        .small()
                        .flex_none()
                        .icon(IconName::Square)
                        .label("Stop all")
                        .tooltip("Stop every running chat in this section")
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_section(i, cx))),
                )
            })
    }

    /// Stop all: every running chat of section `i`, its Overview's included.
    pub(super) fn stop_section(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.state.sections.len() {
            return;
        }
        let mut names = self.section_task_names(i);
        names.extend(self.state.overview_of(i).map(str::to_string));
        for name in names {
            self.stop_task(&name, cx);
        }
        cx.notify();
    }

    /// A turn of `name` ended: its time counts toward its section's readout.
    pub(super) fn count_agent_time(&mut self, name: &str, secs: u64) {
        *self.agent_secs.entry(name.to_string()).or_default() += secs;
    }

    /// Something happened to `task`: the Overview of its section hears of it with its next
    /// message. A section whose Overview was never messaged has no turn to be behind.
    pub(super) fn overview_news(
        &mut self,
        task: &str,
        kind: NewsKind,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let overview = (self.section_of(task))
            .and_then(|i| self.state.sections[i].overview.clone())
            .and_then(|o| self.task(&o).cloned());
        let (Some(overview), Some(ws)) = (overview, self.task(task)) else {
            return;
        };
        let title = ws.display_title().into_owned();
        self.tell_overview(&overview, kind, task, title, text, window, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn tell_overview(
        &mut self,
        overview: &Task,
        kind: NewsKind,
        task: &str,
        title: String,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(meta) = overview.chats.first() else {
            return;
        };
        let news = News {
            kind,
            task: task.into(),
            title,
            text,
        };
        let chat = self.ensure_chat(overview, meta, window, cx);
        chat.update(cx, |c, _| c.add_news(news));
    }

    /// What a finished turn of `task`'s first chat tells its section's Overview.
    pub(super) fn turn_news(&self, task: &str, chat: &Entity<Chat>, cx: &App) -> String {
        let c = chat.read(cx);
        let diff = diff_text(task_diff(self.overview_repos(task, cx)));
        let mut text = outcome_text(c.last_outcome).to_string();
        if !diff.is_empty() {
            text += &format!(", {diff}");
        }
        if !c.last_reply.trim().is_empty() {
            text += &format!(". Last reply: {}", c.last_reply);
        }
        text
    }

    /// A tool call from an Overview's chat, answered at once.
    pub(crate) fn overview_call(
        &mut self,
        call: mcp::Call,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The agent was already told this call failed, and may have made it again.
        if call.expired.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let result = self.run_tool(&call.overview, &call.tool, &call.args, window, cx);
        let _ = call.reply.send(result);
        cx.notify();
    }

    fn run_tool(
        &mut self,
        overview: &str,
        tool: &str,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        // Its token outlives the role: a task moved out of the section is an ordinary one.
        let section = (self.state.overview_section(overview))
            .filter(|_| self.task(overview).is_some_and(|t| t.overview && !t.unsent))
            .ok_or(NOT_AN_OVERVIEW)?;
        let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
        if tool != READ && self.overview_planning(overview, cx) {
            return Err(crate::chat::PLAN_ONLY.into());
        }
        // Only what the Overview oversees: never itself, never another section's task.
        let mine = |s: &Self, name: &str| {
            (s.in_section(section, name))
                .then(|| name.to_string())
                .ok_or(format!(
                    "No task `{name}` in this section. read_task lists its tasks."
                ))
        };
        match tool {
            READ => self.read_tasks(section, text("task"), window, cx),
            SPAWN => self.spawn_for(overview, section, args, window, cx),
            MESSAGE => {
                let task = mine(self, text("task").ok_or("`task` is missing")?)?;
                let message = text("message").ok_or("`message` is empty")?;
                let ws = self.task(&task).cloned().ok_or("the task is gone")?;
                let meta = ws.chats.first().ok_or("the task has no chat")?;
                let chat = self.ensure_chat(&ws, meta, window, cx);
                let sent = chat.update(cx, |c, cx| {
                    c.tell(message, cx).then_some(()).ok_or_else(|| {
                        c.held()
                            .unwrap_or("its chat can't take a message now".into())
                    })
                });
                sent.map_err(|why| format!("The message didn't go out: {why}. Try again later."))?;
                Ok(json!({"task": task, "status": "sent"}).to_string())
            }
            STOP => {
                let task = mine(self, text("task").ok_or("`task` is missing")?)?;
                let running = self.running_chats(&task, cx) > 0;
                self.stop_task(&task, cx);
                let status = match running {
                    true => "stopped",
                    false => "was not running",
                };
                Ok(json!({"task": task, "status": status}).to_string())
            }
            other => Err(format!("Shark has no tool `{other}`")),
        }
    }

    /// Whether every chat of the Overview's task is in Plan mode. The agents refuse the tools
    /// that act there themselves (`chat::planmode::refusal`, codex's sandbox); this is the same
    /// rule held on Shark's side of the wire.
    fn overview_planning(&self, overview: &str, cx: &App) -> bool {
        self.task(overview).is_some_and(|t| {
            !t.chats.is_empty()
                && t.chats
                    .iter()
                    .all(|m| (self.chats.get(&m.id)).map_or(m.plan, |c| c.read(cx).meta.plan))
        })
    }

    fn spawn_for(
        &mut self,
        overview: &str,
        section: usize,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        // Nothing is saved this run, so the worktrees would be orphaned on restart.
        if self.state.read_only {
            return Err("Can't create tasks: state.json couldn't be read".into());
        }
        let brief = args["brief"].as_str().map(str::trim).unwrap_or_default();
        if brief.is_empty() {
            return Err("`brief` is empty: it is all the new task's agent knows".into());
        }
        let wanted = args["agent"].as_str().unwrap_or_default().trim();
        let agents = &self.state.agents;
        let agent = (agents.iter())
            .find(|a| a.slug == wanted || a.name.eq_ignore_ascii_case(wanted))
            .map(AgentLink::from)
            .ok_or_else(|| {
                let names: Vec<_> = agents.iter().map(|a| a.name.as_str()).collect();
                format!("No agent `{wanted}`. Agents: {}", names.join(", "))
            })?;
        let known = &self.state.repos;
        let names: Vec<&str> = (args["repos"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        if names.is_empty() {
            return Err("`repos` is empty: a task needs a repository to work in".into());
        }
        let repos = (names.iter())
            .map(|n| {
                (known.iter().find(|r| r.name == *n).cloned()).ok_or_else(|| {
                    let all: Vec<_> = known.iter().map(|r| r.name.as_str()).collect();
                    format!("No repository `{n}`. Repositories: {}", all.join(", "))
                })
            })
            .collect::<Result<Vec<Repo>, String>>()?;
        // The new task starts from the base task's branch in each repository they share.
        let mut local_parents = vec![];
        if let Some(base) = args["base"]
            .as_str()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            let parent = (self.in_section(section, base))
                .then(|| self.task(base))
                .flatten()
                .ok_or(format!("No task `{base}` in this section to start from"))?;
            local_parents = (parent.repos.iter())
                .filter(|r| names.contains(&r.name.as_str()))
                .map(|r| (r.name.clone(), parent.branch_of(r).to_string()))
                .collect();
            if local_parents.is_empty() {
                return Err(format!("`{base}` works in none of {}", names.join(", ")));
            }
        }
        let title = crate::chat::title_of(brief);
        let source = slug_or(&title, "task");
        let (name, branch) = self.new_task_name(&format!("{}/{source}", agent.slug));
        self.starting.push(Starting {
            name: name.clone(),
            overview: overview.into(),
            title: title.clone(),
            agent: agent.clone(),
        });
        let new = NewTask {
            name: name.clone(),
            branch,
            title: title.clone(),
            agent,
            repos,
            local_parents,
            first: brief.into(),
        };
        let task = name.clone();
        self.spawn_task(new, window, cx, move |s, result, window, cx| {
            let Some(at) = s.starting.iter().position(|x| x.name == task) else {
                return;
            };
            let started = s.starting.remove(at);
            let overview = s.task(&started.overview).cloned();
            let failed = match result {
                Ok(true) => None,
                Ok(false) => Some("its first message didn't go out; the task is there".to_string()),
                Err(e) => Some(e),
            };
            // Into the Overview's section as it is now; without one, the task is ungrouped.
            if s.task(&task).is_some() {
                if let Some(i) = s.state.overview_section(&started.overview) {
                    s.state.move_to_section(&task, Some(i));
                    let _ = s.state.save();
                }
                s.auto_title(&task, &started.title, cx);
            }
            if let (Some(why), Some(overview)) = (failed, overview) {
                s.tell_overview(
                    &overview,
                    NewsKind::Failed,
                    &task,
                    started.title,
                    why,
                    window,
                    cx,
                );
            }
        });
        Ok(json!({"task": name, "title": title, "status": "starting"}).to_string())
    }

    /// `read_task`: one task in full, or every task of the section with what `spawn_task` takes.
    fn read_tasks(
        &mut self,
        section: usize,
        one: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let names = self.section_task_names(section);
        if let Some(name) = one {
            if !names.iter().any(|n| n == name) {
                let starting = self.starting.iter().any(|s| s.name == name);
                return match starting {
                    true => Ok(json!({"task": name, "status": "starting"}).to_string()),
                    false => Err(format!(
                        "No task `{name}` in this section. Its tasks: {}",
                        names.join(", ")
                    )),
                };
            }
            return Ok(self.task_report(name, REPLY_CAP, window, cx).to_string());
        }
        let mut tasks: Vec<Value> = (names.iter())
            .map(|n| self.task_report(n, LISTED_REPLY, window, cx))
            .collect();
        let overview = self.state.sections[section].overview.as_deref();
        tasks.extend(
            (self.starting.iter())
                .filter(|s| Some(s.overview.as_str()) == overview)
                .map(|s| json!({"task": s.name, "title": s.title, "status": "starting"})),
        );
        let st = &self.state;
        Ok(json!({
            "section": st.sections[section].name,
            "tasks": tasks,
            "repositories": st.repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            "agents": st.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        })
        .to_string())
    }

    /// One task as `read_task` returns it. Loads its first chat for the last reply.
    fn task_report(
        &mut self,
        name: &str,
        reply_cap: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Value {
        let Some(ws) = self.task(name).cloned() else {
            return json!({"task": name, "status": "gone"});
        };
        let chat = (ws.chats.first()).map(|m| self.ensure_chat(&ws, m, window, cx));
        let facts = self.task_facts(&ws, cx);
        let status = match facts.bucket {
            home::Bucket::Running => "running".to_string(),
            home::Bucket::Needs => format!("needs the user: {}", facts.line),
            home::Bucket::Unread => "finished; the user hasn't read it yet".into(),
            home::Bucket::Idle => "idle".into(),
            home::Bucket::Backlog => "in the backlog".into(),
        };
        let agent = (ws.chats.first())
            .and_then(|c| ws.agent_link(c.agent.as_deref()?))
            .map(|a| a.name.clone());
        let reply = chat.map(|c| one_line(&c.read(cx).last_reply, reply_cap));
        let dir = ws.dir();
        let worktrees: Vec<Value> = (ws.repos.iter())
            .map(|r| json!({"repository": r.name, "path": dir.join(&r.name), "branch": ws.branch_of(r), "base": r.base}))
            .collect();
        json!({
            "task": name,
            "title": ws.display_title(),
            "status": status,
            "running": facts.running > 0,
            "agent": agent,
            "diff": diff_text(task_diff(facts.repos)),
            "last_reply": reply.filter(|r| !r.is_empty()),
            "worktrees": worktrees,
        })
    }
}

/// What an Overview's task is called where a task's title shows: Home, its tab, a notification.
pub(super) fn overview_title(section: &str) -> String {
    format!("Overview: {section}")
}
