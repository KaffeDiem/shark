//! Home starts tasks and lists every task; agent pages do both for one agent. Neither leaves
//! the open task.
use super::ask::Ask;
use super::task_draft::TaskDraft;
use super::*;
use crate::state::AgentLink;
use crate::state::Appearance;
use crate::sync::PrState;
use crate::task_composer::{Event as ComposerEvent, TaskComposer};
use crate::ui::{card, close_button, mode_pills};
use gpui_kit::component::input::TextareaState;
use std::time::SystemTime;

const BUILD: &str = "Describe the work; enter starts a task";
const ASK: &str = "Ask how to do something in Shark";
const RELEASES: &str = "https://github.com/KaffeDiem/shark/releases/tag/v";

/// Chat IDs keep creation order even after tabs are moved or closed.
fn agent_chain(task: &Task) -> Vec<&str> {
    let mut chats: Vec<_> = task.chats.iter().chain(&task.closed).collect();
    chats.sort_by_key(|c| {
        c.id.rsplit_once('~')
            .and_then(|(_, n)| n.parse::<u64>().ok())
            .unwrap_or(1)
    });
    let mut chain: Vec<_> = chats
        .into_iter()
        .filter_map(|c| c.agent.as_deref())
        .collect();
    chain.dedup();
    chain
}

#[cfg(test)]
mod handoff_chain_tests {
    use super::*;
    #[core::prelude::v1::test]
    fn closed_and_reordered_chats_keep_agent_chronology() {
        let chat = |id: &str, agent: &str| ChatMeta {
            id: id.into(),
            agent: Some(agent.into()),
            ..Default::default()
        };
        let task = Task {
            chats: vec![
                chat("task~10", "coder"),
                chat("task~3", "planner"),
                chat("task~2", "planner"),
            ],
            closed: vec![chat("task", "designer")],
            ..Default::default()
        };
        assert_eq!(agent_chain(&task), vec!["designer", "planner", "coder"]);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Destination {
    Home,
    Agent(String),
}

/// Home's task groups, in the order they're shown.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Bucket {
    Needs,
    Running,
    Unread,
    Idle,
}

impl Bucket {
    fn title(self) -> &'static str {
        match self {
            Bucket::Needs => "Needs you",
            Bucket::Running => "Running",
            Bucket::Unread => "Unread",
            Bucket::Idle => "Idle",
        }
    }
}

/// A task's group, and for Needs you what it waits on. A running agent outranks the rest
/// (as the sidebar's spinner does); a git step that's yours outranks a plan or question the
/// chat ended on, which outranks a turn you haven't read.
pub(super) fn bucket(
    running: usize,
    attn: &Attn,
    waiting: Option<&str>,
) -> (Bucket, Option<String>) {
    match (running, attn, waiting) {
        (1.., _, _) => (Bucket::Running, None),
        (_, Attn::Needs(step), _) => (Bucket::Needs, Some(step.clone())),
        (_, _, Some(label)) => (Bucket::Needs, Some(label.to_string())),
        (_, Attn::Unread, _) => (Bucket::Unread, None),
        _ => (Bucket::Idle, None),
    }
}

/// A repo's line on a Home row. gh logged out or a remote that isn't GitHub isn't news here
/// (the sidebar's git button says so); the rest is the same as the sidebar's.
fn home_summary(r: &crate::sync::Repo) -> String {
    match r.pr {
        Some(Err(_)) => crate::sync::Repo {
            pr: None,
            ..r.clone()
        }
        .summary(),
        _ => r.summary(),
    }
}

type Go = fn(&mut Shark, &mut Window, &mut Context<Shark>);

/// A button that runs `go`, and its shortcut.
fn key_button(b: Button, keys: &str, go: Go, cx: &Context<Shark>) -> Div {
    let b = b.on_click(cx.listener(move |s, _, w, cx| go(s, w, cx)));
    h_flex().gap_2().child(b).child(kbd(keys))
}

/// What each task waits on and when it last changed. Reads transcript tails.
fn scan(wss: Vec<Task>) -> Vec<(String, Option<&'static str>, Option<SystemTime>)> {
    (wss.into_iter())
        .map(|ws| {
            (
                ws.name.clone(),
                crate::transcript::waiting_on_user(&ws),
                crate::transcript::last_activity(&ws),
            )
        })
        .collect()
}

pub(crate) struct Home {
    /// Shown over the open task. With none open, Home shows anyway.
    pub(super) on: bool,
    pub(super) destination: Destination,
    group_tasks: bool,
    /// The task list, which takes j/k/enter.
    pub(super) focus: FocusHandle,
    /// The task under the list cursor.
    pub(super) sel: Option<String>,
    pub(super) prompt: Entity<TextareaState>,
    pub(super) composer: Entity<TaskComposer>,
    pub(super) draft: TaskDraft,
    pub(super) draft_agent: Option<String>,
    pub(super) tray_focus: FocusHandle,
    pub(super) tray_index: usize,
    /// The agent page's task composer, holding `agent_prompt_for`'s draft; other agents'
    /// drafts wait in `agent_drafts`.
    pub(super) agent_prompt: Entity<TextareaState>,
    pub(super) agent_composer: Entity<TaskComposer>,
    pub(super) agent_draft: TaskDraft,
    pub(super) agent_prompt_for: Option<String>,
    pub(super) agent_drafts: HashMap<String, TaskDraft>,
    /// The agent page's header fields.
    pub(super) agent_header: Option<Entity<super::agent_settings::AgentHeader>>,
    /// The highlighted `@agent` completion under Home's prompt.
    pub(super) mention: usize,
    /// The prompt asks Shark instead of starting a task.
    pub(super) asking: bool,
    pub(super) ask: Option<Ask>,
    /// Read off the UI thread when Home opens and when a turn ends, not per frame.
    pub(super) waiting: HashMap<String, &'static str>,
    activity: HashMap<String, SystemTime>,
    /// The latest scan that covers each task; an older scan finishing later is ignored.
    scans: HashMap<String, u64>,
    scan_no: u64,
    /// The page, scrolled to keep the cursor's row in view; `rows` holds the list's rows.
    pub(super) page: ScrollHandle,
    pub(super) rows: ScrollHandle,
    /// The Get started card expanded, while it shows collapsed (there are tasks).
    pub(super) checklist_open: bool,
    _subs: Vec<Subscription>,
}

impl Home {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shark>) -> Self {
        let composer = cx.new(|cx| TaskComposer::new(window, cx));
        composer.update(cx, |c, cx| c.set_sources(vec![], vec![], true, false, cx));
        let agent_composer = cx.new(|cx| TaskComposer::new(window, cx));
        let prompt = composer.read(cx).input.clone();
        let agent_prompt = agent_composer.read(cx).input.clone();
        prompt.update(cx, |p, cx| p.set_placeholder(BUILD, window, cx));
        let _subs = vec![
            cx.subscribe_in(&composer, window, |s, _, ev: &ComposerEvent, w, cx| {
                s.draft_composer_event(false, ev, w, cx)
            }),
            cx.subscribe_in(
                &agent_composer,
                window,
                |s, _, ev: &ComposerEvent, w, cx| s.draft_composer_event(true, ev, w, cx),
            ),
        ];
        Home {
            on: false,
            destination: Destination::Home,
            group_tasks: false,
            focus: cx.focus_handle(),
            sel: None,
            prompt,
            composer,
            draft: TaskDraft::default(),
            draft_agent: None,
            tray_focus: cx.focus_handle(),
            tray_index: 0,
            agent_prompt,
            agent_composer,
            agent_draft: TaskDraft::default(),
            agent_prompt_for: None,
            agent_drafts: HashMap::new(),
            agent_header: None,
            mention: 0,
            asking: false,
            ask: None,
            waiting: HashMap::new(),
            activity: HashMap::new(),
            scans: HashMap::new(),
            scan_no: 0,
            page: ScrollHandle::new(),
            rows: ScrollHandle::new(),
            checklist_open: false,
            _subs,
        }
    }

    pub(super) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.is_focused(window)
            || self.tray_focus.is_focused(window)
            || self.prompt.read(cx).focus_handle(cx).is_focused(window)
            || (self.agent_prompt.read(cx))
                .focus_handle(cx)
                .is_focused(window)
    }
}

/// A Home task row, in display order.
pub(super) struct HomeRow {
    pub(super) ix: usize,
    bucket: Bucket,
    label: Option<String>,
    running: usize,
}

impl Shark {
    /// cmd-n opens the selected agent’s prompt, otherwise an empty task.
    pub(super) fn new_empty_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown()
            && let Destination::Agent(slug) = self.home.destination.clone()
        {
            self.prefill_agent_task(&slug, window, cx);
        } else {
            self.new_task(None, None, None, window, cx)
        }
    }

    /// Overview commands add draft context; an open task keeps the standalone branch dialog.
    pub(super) fn new_task_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown() {
            if self.home.destination == Destination::Home && self.home.asking {
                self.toggle_ask_mode(window, cx);
            }
            self.pick_draft_branch(None, window, cx);
        } else {
            self.new_task_from_repos(self.state.last_repos.clone(), None, window, cx);
        }
    }

    /// Capture the visible choice before a task dialog or asynchronous lookup opens.
    pub(super) fn selected_task_agent(&self, cx: &App) -> Option<AgentLink> {
        if !self.home_shown() {
            return None;
        }
        let slug = match &self.home.destination {
            Destination::Agent(slug) => slug.clone(),
            Destination::Home if !self.home.asking => {
                let text = self.home.prompt.read(cx).value();
                agent_prompt(text.trim(), &self.state.agents).0?.to_string()
            }
            _ => return None,
        };
        let agent = self.state.agents.iter().find(|a| a.slug == slug)?;
        Some(AgentLink::from(agent))
    }

    /// Home is on screen: asked for, or nothing else to show.
    pub(crate) fn home_shown(&self) -> bool {
        self.home.on || self.active.is_none()
    }

    /// cmd-0: Home, or back to the open task.
    pub(super) fn toggle_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_shown() && self.home.destination == Destination::Home {
            self.leave_home(window, cx);
        } else {
            self.enter_home(window, cx);
        }
    }

    pub(super) fn leave_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_some() {
            self.record_active_interaction(cx);
            let tab = self.tab;
            self.show(tab, window, cx); // clears `home.on`
        }
    }

    pub(super) fn enter_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.home.on = true;
        self.home.destination = Destination::Home;
        self.zen = false;
        self.remember_open();
        self.refresh_home(None, cx);
        self.scan_approved(cx);
        let rows = self.home_rows(cx);
        // The cursor starts on the task just left.
        self.home.sel = self.active.clone().or(self.home.sel.take());
        if self.home_at(&rows).is_none() {
            self.home.sel = (rows.first()).map(|r| self.state.tasks[r.ix].name.clone());
        }
        self.focus_fallback(window, cx);
        cx.notify();
    }

    /// Where keys go when nothing else has them: Home's list (its prompt while it has no
    /// rows), or the sidebar.
    pub(super) fn focus_fallback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (self.home_shown(), self.home_rows(cx).is_empty()) {
            (true, true) if self.home.destination == Destination::Home => {
                self.home.prompt.update(cx, |p, cx| p.focus(window, cx))
            }
            (true, _) => self.home.focus.focus(window, cx),
            // A closed dialog gives the focus back to a line input that left with the edit.
            _ if self.active.is_some() && matches!(self.tab, Tab::Feature(_)) => {
                if let Tab::Feature(feature) = self.tab {
                    self.focus_feature(feature, FeatureFocus::Main, window, cx);
                }
            }
            _ if self.zen_shown() => {
                if let Some(&pane) = self.panes(cx).first() {
                    self.focus_pane(pane, window, cx)
                }
            }
            (false, _) => self.sidebar_focus.focus(window, cx),
        }
    }

    /// Refocuses `prev`, else the fallback.
    pub(super) fn restore_focus(
        &mut self,
        prev: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match prev {
            Some(focus) => focus.focus(window, cx),
            None => self.focus_fallback(window, cx),
        }
    }

    /// Re-reads, in the background, what each task (or just `only`) waits on and when it
    /// last changed.
    pub(super) fn refresh_home(&mut self, only: Option<&str>, cx: &mut Context<Self>) {
        let wss: Vec<Task> = (self.state.tasks.iter())
            .filter(|w| only.is_none_or(|n| n == w.name))
            .cloned()
            .collect();
        self.home.scan_no += 1;
        let no = self.home.scan_no;
        for w in &wss {
            self.home.scans.insert(w.name.clone(), no);
        }
        let task = cx.background_executor().spawn(async move { scan(wss) });
        cx.spawn(async move |this, cx| {
            let out = task.await;
            this.update(cx, |s, cx| {
                let h = &mut s.home;
                for (name, waiting, activity) in out {
                    if h.scans.get(&name) != Some(&no) {
                        continue;
                    }
                    match waiting {
                        Some(l) => h.waiting.insert(name.clone(), l),
                        None => h.waiting.remove(&name),
                    };
                    match activity {
                        Some(t) => h.activity.insert(name, t),
                        None => h.activity.remove(&name),
                    };
                }
                cx.notify()
            })
            .ok();
        })
        .detach();
    }

    /// "Approve a plan" for approvals from before `State::approved_plan` existed: looks
    /// through the plans on disk, in the background, until one is found.
    fn scan_approved(&mut self, cx: &mut Context<Self>) {
        if self.state.approved_plan {
            return;
        }
        let ids: Vec<String> = (self.state.tasks.iter())
            .flat_map(|w| w.chats.iter().chain(&w.closed))
            .map(|c| c.id.clone())
            .collect();
        let task = (cx.background_executor())
            .spawn(async move { ids.iter().any(|id| crate::plan::approved_any(id)) });
        cx.spawn(async move |this, cx| {
            if task.await {
                this.update(cx, |s, cx| {
                    s.state.mark(|s| &mut s.approved_plan);
                    cx.notify()
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn home_rows(&self, cx: &App) -> Vec<HomeRow> {
        let mut rows: Vec<HomeRow> = (0..self.state.tasks.len())
            .filter(|&ix| match &self.home.destination {
                Destination::Agent(slug) if self.home_shown() => self.state.tasks[ix]
                    .agent
                    .as_ref()
                    .is_some_and(|a| &a.slug == slug),
                _ => true,
            })
            .map(|ix| {
                let name = &self.state.tasks[ix].name;
                let running = self.running_chats(name, cx);
                let waiting = self.home.waiting.get(name).copied();
                let (bucket, label) = bucket(running, &self.attention_of(name, cx), waiting);
                HomeRow {
                    ix,
                    bucket,
                    label,
                    running,
                }
            })
            .collect();
        rows.sort_by_key(|r| r.bucket);
        if self.home.destination == Destination::Home && self.home.group_tasks {
            rows.sort_by_key(|r| {
                self.plain_task_group(r)
                    .unwrap_or(self.state.sections.len())
            });
        }
        rows
    }

    fn plain_task_group(&self, row: &HomeRow) -> Option<usize> {
        self.state.sections.iter().position(|section| {
            section.agent.is_none() && section.tasks.contains(&self.state.tasks[row.ix].name)
        })
    }

    fn home_group_key(&self, row: &HomeRow) -> (Option<usize>, Option<Bucket>) {
        if self.home.destination == Destination::Home && self.home.group_tasks {
            (self.plain_task_group(row), None)
        } else {
            (None, Some(row.bucket))
        }
    }

    fn home_row_group(&self, row: &HomeRow) -> String {
        if self.home.destination == Destination::Home && self.home.group_tasks {
            self.plain_task_group(row)
                .map(|i| self.state.sections[i].name.clone())
                .unwrap_or_else(|| "Ungrouped".into())
        } else {
            row.bucket.title().into()
        }
    }

    /// The cursor's position in `rows`.
    pub(super) fn home_at(&self, rows: &[HomeRow]) -> Option<usize> {
        let sel = self.home.sel.as_ref()?;
        rows.iter()
            .position(|r| &self.state.tasks[r.ix].name == sel)
    }

    /// The cursor's position in `rows`, else the first row.
    pub(super) fn home_cursor(&self, rows: &[HomeRow]) -> Option<usize> {
        self.home_at(rows).or((!rows.is_empty()).then_some(0))
    }

    pub(super) fn home_move(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let rows = self.home_rows(cx);
        if rows.is_empty() {
            return;
        }
        let to = nav.to(self.home_at(&rows).unwrap_or(0), rows.len(), 10);
        self.home.sel = Some(self.state.tasks[rows[to].ix].name.clone());
        // The list's children are the rows with a heading before each group's first.
        let headings = rows[..=to]
            .chunk_by(|a, b| self.home_group_key(a) == self.home_group_key(b))
            .count();
        self.home_scroll_to(to + headings);
        cx.notify();
    }

    /// Scrolls the page the least that shows the list's child `ix`, as of the last layout.
    fn home_scroll_to(&self, ix: usize) {
        let (page, rows) = (&self.home.page, &self.home.rows);
        let Some(b) = rows.bounds_for_item(ix) else {
            return;
        };
        let (view, mut off) = (page.bounds(), page.offset());
        if b.top() < view.top() {
            off.y += view.top() - b.top();
        } else if b.bottom() > view.bottom() {
            off.y -= b.bottom() - view.bottom();
        }
        page.set_offset(off);
    }

    pub(super) fn home_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.home_rows(cx);
        if let Some(r) = self.home_cursor(&rows) {
            self.open_task(rows[r].ix, window, cx)
        }
    }

    /// esc: stop the answer being written, else back to the task.
    pub(super) fn home_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home.destination != Destination::Home || !self.cancel_ask(cx) {
            if self.active.is_some() {
                self.leave_home(window, cx);
            } else if self.home.destination != Destination::Home {
                self.enter_home(window, cx);
            }
        }
    }

    pub(super) fn toggle_ask_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mentions(cx).is_empty() {
            return self.complete_mention(None, window, cx);
        }
        self.home.asking = !self.home.asking;
        self.home
            .composer
            .update(cx, |c, cx| c.set_asking(self.home.asking, cx));
        let placeholder = if self.home.asking { ASK } else { BUILD };
        self.home
            .prompt
            .update(cx, |p, cx| p.set_placeholder(placeholder, window, cx));
        cx.notify();
    }

    pub(super) fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.home.prompt.read(cx).value().trim().to_string();
        if self.home.asking {
            if !text.is_empty() {
                self.home
                    .prompt
                    .update(cx, |p, cx| p.set_value("", window, cx));
                self.ask(text, window, cx);
            }
            return;
        }
        self.submit_task_draft(window, cx);
    }

    /// The agent's page, typing into its task composer.
    pub(super) fn prefill_agent_task(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_agent(slug, window, cx);
        if self.home.destination == Destination::Agent(slug.into()) {
            self.home
                .agent_prompt
                .update(cx, |p, cx| p.focus(window, cx));
        }
    }

    pub(super) fn pick_home_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agents = self.state.agents.clone();
        let text = self.home.prompt.read(cx).value();
        let (slug, _) = agent_prompt(text.trim(), &agents);
        let selected = slug
            .and_then(|slug| agents.iter().position(|a| a.slug == slug))
            .map_or(0, |i| i + 1);
        let mut rows = vec![CommandItem::new().label("No agent").icon(IconName::AtSign)];
        rows.extend(agents.iter().map(|agent| {
            CommandItem::new()
                .label(agent.name.clone())
                .icon(super::sidebar::section_icon(&agent.icon))
        }));
        open_menu(
            cx.entity().downgrade(),
            "Start the task with an agent…",
            rows,
            selected,
            move |s, i, w, cx| {
                let text = s.home.prompt.read(cx).value();
                let (_, body) = agent_prompt(text.trim(), &s.state.agents);
                let value = if i == 0 {
                    body.to_string()
                } else {
                    format!("@{} {body}", agents[i - 1].slug)
                };
                let old_start = text.trim_end().len() - body.len();
                let new_start = value.len() - body.len();
                let mut content = gpui_kit::component::input::InputContent::new(value);
                for span in s.home.prompt.read(cx).tokens() {
                    let range = span.range();
                    if range.start >= old_start && range.end <= old_start + body.len() {
                        content = content
                            .with_token(
                                range.start - old_start + new_start
                                    ..range.end - old_start + new_start,
                                span.token().clone(),
                            )
                            .expect("unchanged token text in the task body");
                    }
                }
                s.home.prompt.update(cx, |p, cx| {
                    p.set_value(content, w, cx);
                    let end = p.value().len();
                    p.set_selected_range(end..end, cx);
                    p.focus(w, cx);
                });
                s.draft_composer_event(false, &ComposerEvent::Change, w, cx);
                cx.notify();
            },
            window,
            cx,
        );
    }

    /// Agents matching the `@` Home's prompt starts with, while that mention is being typed:
    /// prefix matches first, at most six.
    pub(super) fn mentions(&self, cx: &App) -> Vec<usize> {
        if self.home.asking {
            return vec![];
        }
        let text = self.home.prompt.read(cx).value();
        let Some(q) = text.strip_prefix('@') else {
            return vec![];
        };
        if q.contains(char::is_whitespace) {
            return vec![];
        }
        let q = q.to_lowercase();
        let mut hits: Vec<(bool, usize)> = (self.state.agents.iter().enumerate())
            .filter_map(|(i, a)| {
                let name = a.name.to_lowercase();
                let prefix = a.slug.starts_with(&q) || name.starts_with(&q);
                (prefix || a.slug.contains(&q) || name.contains(&q)).then_some((!prefix, i))
            })
            .collect();
        hits.sort();
        hits.into_iter().map(|(_, i)| i).take(6).collect()
    }

    /// Puts `@slug ` for agent `pick`, or the highlighted completion, at the prompt's start.
    pub(super) fn complete_mention(
        &mut self,
        pick: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hits = self.mentions(cx);
        let at = self.home.mention.min(hits.len().saturating_sub(1));
        let Some(i) = pick.or(hits.get(at).copied()) else {
            return;
        };
        let value = format!("@{} ", self.state.agents[i].slug);
        self.home.mention = 0;
        self.home.prompt.update(cx, |p, cx| {
            p.set_value(value, window, cx);
            let end = p.value().len();
            p.set_selected_range(end..end, cx);
            p.focus(window, cx);
        });
        self.draft_composer_event(false, &ComposerEvent::Change, window, cx);
        cx.notify();
    }

    pub(super) fn move_mention(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.mentions(cx).len();
        if n > 0 {
            self.home.mention = (self.home.mention as isize + by).rem_euclid(n as isize) as usize;
            cx.notify();
        }
    }

    /// A task was created with `first` as its first message: clear the prompt if it
    /// came from there. A cancelled dialog leaves the prompt as typed.
    pub(super) fn home_prompt_used(
        &mut self,
        first: Option<&str>,
        agent: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = self.home.prompt.clone();
        let text = prompt.read(cx).value();
        let used = !self.home.asking
            && match agent {
                Some(slug) => {
                    let (mentioned, body) = agent_prompt(text.trim(), &self.state.agents);
                    mentioned == Some(slug) && body == first.unwrap_or_default()
                }
                None => first.is_some_and(|first| text.trim() == first),
            };
        if used {
            prompt.update(cx, |p, cx| p.set_value("", window, cx));
        }
        let Some(slug) = agent else { return };
        let first = first.unwrap_or_default();
        if self.home.agent_prompt_for.as_deref() == Some(slug) {
            let composer = self.home.agent_prompt.clone();
            if composer.read(cx).value().trim() == first {
                composer.update(cx, |p, cx| p.set_value("", window, cx));
            }
        } else if self.home.agent_drafts.get(slug).map(|d| d.text.trim()) == Some(first) {
            self.home.agent_drafts.remove(slug);
        }
    }

    pub(crate) fn set_appearance(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Appearance),
    ) {
        f(&mut self.state.appearance);
        let _ = self.state.save();
        crate::theme::apply(&self.state.appearance, cx);
        cx.notify();
    }

    /// The Get started items, each with whether it's done.
    pub(super) fn checklist(&self, cx: &App) -> [(&'static str, bool); 5] {
        let st = &self.state;
        let pr = st.opened_pr
            || (st.tasks.iter()).any(|w| {
                (self.overview_repos(&w.name, cx))
                    .is_some_and(|r| r.iter().any(|r| r.pr().is_some()))
            });
        [
            (
                "Tools ready",
                !self.deps.is_empty() && self.deps.iter().all(|d| d.ok),
            ),
            ("Add a repo", !st.repos.is_empty()),
            ("Create a task", !st.tasks.is_empty()),
            ("Approve a plan", st.approved_plan),
            ("Open a PR", pr),
        ]
    }

    pub(super) fn checklist_open(&self, items: &[(&'static str, bool)]) -> bool {
        !self.state.home_checklist_dismissed && items.iter().any(|(_, done)| !done)
    }

    /// The Get started card's X, and the palette's "Hide Get started".
    pub(super) fn dismiss_checklist(&mut self, cx: &mut Context<Self>) {
        self.state.home_checklist_dismissed = true;
        let _ = self.state.save();
        cx.notify()
    }

    /// "Approve a plan": plan mode on in the cursor's (or the first) task.
    pub(super) fn try_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.home_rows(cx);
        let Some(r) = self.home_cursor(&rows) else {
            return notify_err(window, cx, "Create a task first (cmd-n)");
        };
        let ix = rows[r].ix;
        // Leaving the open task asks about unsaved edits first; plan mode comes after.
        let leaving =
            (self.state.tasks.get(ix)).is_some_and(|w| self.active.as_ref() != Some(&w.name));
        self.confirm_discard(leaving, leaving, window, cx, move |s, window, cx| {
            s.open_task(ix, window, cx);
            s.with_plan(window, cx, |c, _, cx| c.set_plan_mode(true, cx));
        });
    }

    /// The Home row at the top of the sidebar.
    pub(super) fn render_home_row(
        &self,
        rail: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let modifiers = window.modifiers();
        let cmd_held = modifiers.platform && modifiers.number_of_modifiers() == 1;
        let selected = self.home_shown() && self.home.destination == Destination::Home;
        side_row(theme)
            .text_color(theme.sidebar_foreground)
            .id("home-row")
            .when(!rail, |d| d.h(px(32.)).mb_0())
            .when(rail, super::sidebar::rail_button)
            .map(|d| cursor_row(d, selected, false, false, theme))
            .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
            .child(Icon::new(IconName::House).with_size(px(16.)))
            .when(!rail, |d| d.child("Home"))
            .when(!rail && cmd_held, |d| {
                d.child(
                    kbd("cmd-0")
                        .appearance(false)
                        .text_xs()
                        .text_color(theme.sidebar_foreground),
                )
            })
            .when(rail, |d| {
                d.tooltip(|w, cx| Tooltip::new("Home (cmd-0)").build(w, cx))
            })
            .on_click(cx.listener(|this, _, w, cx| this.enter_home(w, cx)))
            .test_support()
    }

    pub(super) fn render_home(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        match &self.home.destination {
            Destination::Agent(slug) => return self.render_agent_page(slug, window, cx),
            Destination::Home => {}
        }
        let prompt = self.render_prompt(window, cx);
        let ask = self.render_ask(cx);
        // Shrinks with the window; its fix commands truncate.
        let deps = crate::deps::render_setup(&self.deps, cx).map(|d| d.w_full().max_w(px(480.)));
        let list = self.render_ws_list(window, cx);
        let items = self.checklist(cx);
        let setup = self.render_setup(&items, cx);
        let appearance = self.render_appearance(cx);
        let news = self.render_whats_new(cx);
        let theme = cx.theme();
        v_flex()
            .key_context("HomeView")
            .flex_1()
            .min_w_0()
            .h_full()
            .on_action(cx.listener(|this, _: &CancelEdit, w, cx| this.home_escape(w, cx)))
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .id("home-header-empty")
                            .flex_1()
                            .h_full()
                            .gap_2()
                            .child(Icon::new(IconName::House).with_size(px(14.)))
                            .child(div().text_sm().child("Home"))
                            .map(|d| self.draggable_chrome("home-header-empty", d)),
                    )
                    .when(self.active.is_some(), |d| {
                        d.child(
                            Button::new("back-to-workspace")
                                .ghost()
                                .xsmall()
                                .label("Back to task")
                                .on_click(cx.listener(|this, _, w, cx| this.leave_home(w, cx))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("esc"),
                        )
                    }),
            )
            .child(
                div()
                    .id("home-page")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.home.page)
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(760.))
                            .mx_auto()
                            .px_4()
                            .py_8()
                            .gap_8()
                            .child(prompt)
                            .children(news)
                            .children(ask)
                            .children(deps)
                            .child(list)
                            .children(setup)
                            .child(appearance),
                    ),
            )
            .into_any_element()
    }

    fn pick_task_groups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let groups: Vec<_> = self
            .state
            .sections
            .iter()
            .enumerate()
            .filter(|(_, section)| section.agent.is_none())
            .map(|(i, section)| (i, section.name.clone(), section.icon.clone()))
            .collect();
        let mut rows = vec![
            CommandItem::new()
                .label("New task group…")
                .icon(IconName::FolderPlus),
        ];
        rows.extend(groups.iter().map(|(_, name, icon)| {
            CommandItem::new()
                .label(name.clone())
                .icon(super::sidebar::section_icon(icon))
        }));
        open_menu(
            cx.entity().downgrade(),
            "Task groups",
            rows,
            0,
            move |s, i, w, cx| {
                if i == 0 {
                    s.start_naming(None, None, w, cx);
                } else {
                    let entries = super::sidebar::section_menu(s, groups[i - 1].0, cx);
                    s.show_menu(w.mouse_position(), entries, w, cx);
                }
            },
            window,
            cx,
        );
    }

    fn render_prompt(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_2()
            .child(div().text_base().child(if self.home.asking {
                "Ask Shark"
            } else {
                "What do you want to build?"
            }))
            .child(self.render_task_composer(false, window, cx))
            .into_any_element()
    }

    /// The Get started checklist and the cmd-k hint; `None` once there's neither.
    fn render_setup(
        &self,
        items: &[(&'static str, bool); 5],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let list = self
            .checklist_open(items)
            .then(|| self.render_checklist(items, cx));
        let theme = cx.theme();
        let hint = (!self.state.palette_used).then(|| {
            h_flex()
                .gap_1p5()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Press")
                .child(kbd("cmd-k"))
                .child("for every command")
        });
        (list.is_some() || hint.is_some()).then(|| {
            v_flex()
                .gap_4()
                .children(list)
                .children(hint)
                .into_any_element()
        })
    }

    fn render_checklist(
        &self,
        items: &[(&'static str, bool); 5],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let done = items.iter().filter(|(_, d)| *d).count();
        let progress = format!("{done} of {}", items.len());
        // Someone with tasks knows their way around: one line, opened on click.
        if !self.state.tasks.is_empty() && !self.home.checklist_open {
            return h_flex()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(
                    Button::new("open-checklist")
                        .ghost()
                        .small()
                        .label(format!("Get started · {progress}"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.home.checklist_open = true;
                            cx.notify()
                        })),
                )
                .into_any_element();
        }
        card(theme)
            .child(
                h_flex()
                    .h(px(40.))
                    .px_3()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Get started"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(progress),
                    )
                    .child(div().flex_1())
                    .child(
                        close_button("dismiss-checklist", "Dismiss")
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss_checklist(cx))),
                    ),
            )
            .children(items.iter().enumerate().map(|(i, (label, done))| {
                let (btn, keys, go): (_, _, Go) = match i {
                    1 => ("Add repo", "cmd-k", Shark::add_repo),
                    2 => ("New task", "cmd-n", Shark::new_empty_task),
                    3 => ("Use plan mode", "shift-tab", Shark::try_plan_mode),
                    _ => ("Create PRs", "cmd-k", Shark::toggle_palette),
                };
                let right = match (i, done) {
                    (_, true) => None,
                    (0, _) => Some(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match self.deps.is_empty() {
                                true => "Checking…",
                                false => "See the fixes at the top",
                            })
                            .into_any_element(),
                    ),
                    (1, _) => Some(
                        Button::new(("home-do", i))
                            .ghost()
                            .xsmall()
                            .label(btn)
                            .on_click(cx.listener(|s, _, w, cx| s.add_repo(w, cx)))
                            .into_any_element(),
                    ),
                    _ => Some(
                        key_button(
                            Button::new(("home-do", i)).ghost().xsmall().label(btn),
                            keys,
                            go,
                            cx,
                        )
                        .into_any_element(),
                    ),
                };
                h_flex()
                    .h(px(36.))
                    .px_3()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(match done {
                        true => Icon::new(IconName::CircleCheck)
                            .with_size(px(14.))
                            .text_color(theme.success_foreground),
                        false => Icon::new(IconName::Circle)
                            .with_size(px(14.))
                            .text_color(theme.muted_foreground),
                    })
                    .child(
                        div()
                            .flex_1()
                            .when(*done, |d| d.text_color(theme.muted_foreground))
                            .child(*label),
                    )
                    .children(right)
            }))
            .into_any_element()
    }

    /// The task list, grouped by what each task needs; Home can group by task group instead.
    pub(super) fn render_ws_list(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.home_rows(cx);
        let theme = cx.theme();
        let focused = self.home.focus.is_focused(window);
        let on_home = self.home.destination == Destination::Home;
        let list = v_flex()
            .id("home-list")
            .key_context("Home")
            .track_focus(&self.home.focus)
            .track_scroll(&self.home.rows);
        let list = nav_actions(list, true, cx, Shark::home_move)
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.home_confirm(w, cx)))
            .on_action(cx.listener(|this, _: &FocusComposer, w, cx| {
                match this.home.destination.clone() {
                    Destination::Agent(slug) => this.prefill_agent_task(&slug, w, cx),
                    Destination::Home => {
                        this.enter_home(w, cx);
                        this.home.prompt.update(cx, |p, cx| p.focus(w, cx))
                    }
                }
            }));
        if rows.is_empty() {
            return match on_home {
                true => self.render_empty(list, cx),
                false => list
                    .child(super::agent_settings::empty_state(
                        theme,
                        "agent-empty-tasks",
                        IconName::Inbox,
                        "No tasks yet",
                        "Describe the work above and press enter. Each task gets its own branch and chat.",
                    ))
                    .into_any_element(),
            };
        }
        let now = SystemTime::now();
        let mut items: Vec<AnyElement> = vec![];
        let grouping = on_home.then(|| {
            Button::new("task-grouping")
                .ghost()
                .xsmall()
                .icon(IconName::ListFilter)
                .label(if self.home.group_tasks {
                    "Task group"
                } else {
                    "Status"
                })
                .tooltip("Group tasks by")
                .on_click(cx.listener(|s, _, w, cx| {
                    open_menu(
                        cx.entity().downgrade(),
                        "Group tasks by…",
                        vec![
                            CommandItem::new()
                                .label("Status")
                                .checked(!s.home.group_tasks),
                            CommandItem::new()
                                .label("Task group")
                                .checked(s.home.group_tasks),
                            CommandItem::new()
                                .label("Task groups…")
                                .icon(IconName::FolderPlus),
                        ],
                        usize::from(s.home.group_tasks),
                        |s, i, w, cx| match i {
                            2 => s.pick_task_groups(w, cx),
                            _ => {
                                s.home.group_tasks = i == 1;
                                cx.notify();
                            }
                        },
                        w,
                        cx,
                    );
                }))
        });
        let mut grouping = grouping;
        for group in rows.chunk_by(|a, b| self.home_group_key(a) == self.home_group_key(b)) {
            items.push(
                h_flex()
                    .id(("home-group", items.len()))
                    .aria_label(format!(
                        "{}: {} tasks",
                        self.home_row_group(&group[0]),
                        group.len()
                    ))
                    .test_support()
                    .h(px(28.))
                    .mt(if items.is_empty() { px(0.) } else { px(12.) })
                    .when(on_home, |d| d.px_3())
                    .gap_2()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(self.home_row_group(&group[0]))
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .child(group.len().to_string()),
                    )
                    .child(div().flex_1())
                    .when(on_home && self.home.group_tasks, |d| {
                        if let Some(i) = self.plain_task_group(&group[0]) {
                            d.child(
                                Button::new(("task-group-actions", i))
                                    .ghost()
                                    .xsmall()
                                    .label("Group actions…")
                                    .on_click(cx.listener(move |s, _, w, cx| {
                                        let entries = super::sidebar::section_menu(s, i, cx);
                                        s.show_menu(w.mouse_position(), entries, w, cx);
                                    })),
                            )
                        } else {
                            d
                        }
                    })
                    // The first heading carries the list's grouping control.
                    .children(grouping.take())
                    .into_any_element(),
            );
            items.extend(group.iter().map(|r| self.home_ws_row(r, focused, now, cx)));
        }
        list.children(items)
            .when(on_home, |d| {
                d.child(
                    h_flex()
                        .pt_3()
                        .px_3()
                        .gap_3()
                        .text_xs()
                        .text_color(faint(theme))
                        .child(crate::ui::hint("enter", "open"))
                        .child(crate::ui::hint("i", "prompt"))
                        .child(crate::ui::hint("cmd-backspace", "delete"))
                        .child(div().child("right-click for more")),
                )
            })
            .into_any_element()
    }

    /// The list with no agents: start one; a repo can come later.
    fn render_empty(&self, list: Stateful<Div>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let add = Button::new("home-add-repo").label("Add repo");
        let add = add.on_click(cx.listener(|s, _, w, cx| s.add_repo(w, cx)));
        let new = Button::new("home-new-ws").label("New task").primary();
        let new = key_button(new, "cmd-n", Shark::new_empty_task, cx);
        list.py_8()
            .items_center()
            .gap_3()
            .child(
                Icon::new(IconName::Fish)
                    .with_size(px(32.))
                    .text_color(faint(theme)),
            )
            .child(
                div()
                    .text_base()
                    .text_color(theme.foreground)
                    .child("No tasks yet"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("A task plans first; attach a repository when it's time to build"),
            )
            .child(h_flex().gap_4().child(new).child(add))
            .into_any_element()
    }

    /// One task's row on the list: one line, Linear-style. Status icon and title; what it waits
    /// on in warning text; its agent (on Home), repositories and age on the right. The branch and
    /// chat count are in the tooltip and accessible label.
    pub(super) fn home_ws_row(
        &self,
        r: &HomeRow,
        focused: bool,
        now: SystemTime,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let w = &self.state.tasks[r.ix];
        let ix = r.ix;
        let selected = self.home.sel.as_deref() == Some(w.name.as_str());
        let repos = self.overview_repos(&w.name, cx);
        let merged = (repos.iter().copied().flatten())
            .any(|r| r.pr().is_some_and(|p| p.state == PrState::Merged));
        let when = (self.home.activity.get(&w.name))
            .and_then(|t| now.duration_since(*t).ok())
            .map(crate::sync::ago);
        // Each repo's status replaces its bare name once checked.
        let names = match repos {
            Some(repos) => repos.iter().map(home_summary).collect::<Vec<_>>(),
            None => w.repos.iter().map(|r| r.name.clone()).collect(),
        }
        .join(" · ");
        let names = match names.is_empty() {
            true => "No repository".to_string(),
            false => names,
        };
        let chats = match w.chats.len() {
            1 => "1 chat".to_string(),
            n => format!("{n} chats"),
        };
        let status = r.label.clone().or_else(|| {
            if r.running == 0 {
                return None;
            }
            w.chats
                .iter()
                .rev()
                .find_map(|meta| {
                    let chat = self.chats.get(&meta.id)?.read(cx);
                    chat.working().then(|| chat.latest_tool_step()).flatten()
                })
                .or_else(|| Some("Working…".into()))
        });
        let agent = (self.home.destination == Destination::Home)
            .then(|| {
                let link = w.agent.as_ref()?;
                self.state.agents.iter().find(|a| a.slug == link.slug)
            })
            .flatten();
        let name = w.name.clone();
        let title = w.display_title().to_string();
        let detail = format!("{} · {chats} · {names}", w.branch);
        let chain = agent_chain(w);
        let chain = chain
            .into_iter()
            .map(|slug| self.agent(slug).map_or(slug, |a| a.name.as_str()))
            .collect::<Vec<_>>()
            .join(" → ");
        let tip: SharedString = if chain.is_empty() {
            detail.clone()
        } else {
            format!("{detail}\n{chain}")
        }
        .into();
        let ws = w.clone();
        h_flex()
            .id(("home-ws", ix))
            .role(gpui_kit::accesskit::Role::Button)
            .aria_label(format!(
                "Open task {title}{}: {detail}",
                status
                    .as_deref()
                    .map(|s| format!(", {s}"))
                    .unwrap_or_default()
            ))
            .test_support()
            .h(px(36.))
            .when(self.home.destination == Destination::Home, |d| d.px_3())
            .gap_3()
            .rounded(theme.radius)
            .text_sm()
            .map(|d| list_cursor_row(d, selected, focused, theme))
            .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
            .on_mouse_down(MouseButton::Left, {
                let name = name.clone();
                cx.listener(move |this, _, _, cx| {
                    this.home.sel = Some(name.clone());
                    cx.notify()
                })
            })
            .on_click(cx.listener(move |this, _, w, cx| this.open_task(ix, w, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.home.sel = Some(ws.name.clone());
                    this.home.focus.focus(w, cx);
                    let entries = ws_menu(this, &ws, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(div().flex_none().child(super::sidebar::indicator(
                r.bucket,
                theme.muted_foreground,
                px(14.),
                theme,
            )))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .children(status.map(|l| {
                div()
                    .flex_none()
                    .max_w(px(260.))
                    .truncate()
                    .text_color(match r.label.is_some() {
                        true => theme.warning_foreground,
                        false => theme.muted_foreground,
                    })
                    .child(l)
            }))
            .when(merged, |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .text_color(theme.success_foreground)
                        .child(Icon::new(IconName::GitMerge).with_size(px(12.)))
                        .child("Merged"),
                )
            })
            .children(agent.map(|a| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(super::sidebar::section_icon(&a.icon)).with_size(px(12.)))
                    .child(a.name.clone())
            }))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(240.))
                    .flex_shrink(1.)
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(names),
            )
            .children(when.map(|t| {
                div()
                    .flex_none()
                    .w(px(56.))
                    .text_right()
                    .text_color(theme.muted_foreground)
                    .child(t)
            }))
            .into_any_element()
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.state.appearance.mode;
        let theme = cx.theme();
        card(theme)
            .child(
                h_flex()
                    .min_h(px(52.))
                    .px_3()
                    .gap_3()
                    .child(div().text_sm().child("Appearance"))
                    .child(h_flex().gap_1().children(mode_pills(mode).map(|(m, pill)| {
                        pill.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_appearance(cx, |a| a.mode = m)
                        }))
                    })))
                    .child(div().flex_1())
                    .child(
                        Button::new("all-settings")
                            .ghost()
                            .small()
                            .label("All settings")
                            .on_click(cx.listener(|this, _, w, cx| {
                                crate::settings::open(&this.state, w, cx)
                            })),
                    )
                    .child(kbd("cmd-,")),
            )
            .into_any_element()
    }

    /// One line after an update, until dismissed. Never on a fresh install.
    fn render_whats_new(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let v = env!("CARGO_PKG_VERSION");
        // Empty: updated from a version from before this line (`Shark::new`).
        if self.state.last_seen_version == v {
            return None;
        }
        let theme = cx.theme();
        Some(
            h_flex()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Sparkles).with_size(px(14.)))
                .child(format!("Updated to Shark {v}"))
                .child(
                    Link::new("release-notes")
                        .href(format!("{RELEASES}{v}"))
                        .child("Release notes"),
                )
                .child(div().flex_1())
                .child(
                    close_button("dismiss-news", "Dismiss").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.state.last_seen_version = env!("CARGO_PKG_VERSION").into();
                            let _ = this.state.save();
                            cx.notify()
                        },
                    )),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Bucket, bucket};
    use crate::attention::Attn;
    use core::prelude::v1::test;

    #[test]
    fn buckets_by_precedence() {
        let needs = Attn::Needs("Fix checks · api".into());
        let plan = Some(crate::transcript::PLAN_TO_APPROVE);
        assert_eq!(
            bucket(2, &needs, plan),
            (Bucket::Running, None),
            "running wins"
        );
        assert_eq!(
            bucket(0, &needs, plan),
            (Bucket::Needs, Some("Fix checks · api".into())),
            "the git step before the chat's plan"
        );
        assert_eq!(
            bucket(0, &Attn::Unread, plan),
            (Bucket::Needs, Some("Plan to approve".into())),
            "a waiting chat before unread"
        );
        assert_eq!(bucket(0, &Attn::Unread, None), (Bucket::Unread, None));
        assert_eq!(bucket(0, &Attn::None, None), (Bucket::Idle, None));
        assert!(Bucket::Needs < Bucket::Running && Bucket::Unread < Bucket::Idle);
    }
}

/// Only a registered leading mention starts an agent task; unknown mentions remain text.
pub(super) fn agent_prompt<'a>(
    text: &'a str,
    agents: &[crate::state::Agent],
) -> (Option<&'a str>, &'a str) {
    let Some(mention) = text.strip_prefix('@') else {
        return (None, text);
    };
    let end = mention.find(char::is_whitespace).unwrap_or(mention.len());
    let slug = &mention[..end];
    if agents.iter().any(|a| a.slug == slug) {
        (Some(slug), mention[end..].trim_start())
    } else {
        (None, text)
    }
}

#[cfg(test)]
mod agent_prompt_tests {
    use super::agent_prompt;

    #[test]
    fn known_mentions_start_tasks_and_unknown_mentions_stay_text() {
        let agents = vec![crate::state::Agent {
            slug: "designer".into(),
            name: "Designer".into(),
            icon: "bot".into(),
            instructions: String::new(),
            repos: vec![],
            model: None,
            effort: None,
            ..Default::default()
        }];
        assert_eq!(
            agent_prompt("@designer fix x", &agents),
            (Some("designer"), "fix x")
        );
        assert_eq!(agent_prompt("@nobody x", &agents), (None, "@nobody x"));
        assert_eq!(agent_prompt("@designer", &agents), (Some("designer"), ""));
        assert_eq!(
            agent_prompt("hello @designer", &agents),
            (None, "hello @designer")
        );
    }
}
