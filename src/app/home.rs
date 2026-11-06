//! Home (cmd-0): a prompt to start the next workspace or ask Shark, setup, every workspace by
//! what it needs from you, and appearance. It is what shows while no workspace is open, and
//! over the open one while `Home::on`.
use super::ask::Ask;
use super::*;
use crate::state::Appearance;
use crate::sync::PrState;
use crate::ui::{card, close_button, mode_pills};
use std::time::SystemTime;

const BUILD: &str = "Describe the change; enter picks repos and a branch";
const ASK: &str = "Ask how to do something in Shark";
const RELEASES: &str = "https://github.com/KaffeDiem/shark/releases/tag/v";

/// Home's workspace groups, in the order they're shown.
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

/// A workspace's group, and for Needs you what it waits on. A running agent outranks the rest
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

/// What each workspace waits on and when it last changed. Reads transcript tails.
fn scan(wss: Vec<Workspace>) -> Vec<(String, Option<&'static str>, Option<SystemTime>)> {
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
    /// Shown over the open workspace. With none open, Home shows anyway.
    pub(super) on: bool,
    /// The workspace list, which takes j/k/enter.
    pub(super) focus: FocusHandle,
    /// The workspace under the list cursor.
    pub(super) sel: Option<String>,
    pub(super) prompt: Entity<InputState>,
    /// The prompt asks Shark instead of starting a workspace.
    pub(super) asking: bool,
    pub(super) ask: Option<Ask>,
    /// Read off the UI thread when Home opens and when a turn ends, not per frame.
    pub(super) waiting: HashMap<String, &'static str>,
    activity: HashMap<String, SystemTime>,
    /// The latest scan that covers each workspace; an older scan finishing later is ignored.
    scans: HashMap<String, u64>,
    scan_no: u64,
    /// The page, scrolled to keep the cursor's row in view; `rows` holds the list's rows.
    page: ScrollHandle,
    rows: ScrollHandle,
    /// The Get started card expanded, while it shows collapsed (there are workspaces).
    pub(super) checklist_open: bool,
    _subs: Vec<Subscription>,
}

impl Home {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shark>) -> Self {
        let prompt = cx.new(|cx| InputState::new(window, cx).placeholder(BUILD));
        let _subs = vec![
            cx.subscribe_in(&prompt, window, |s, _, ev: &InputEvent, w, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    s.submit_prompt(w, cx)
                }
            }),
        ];
        Home {
            on: false,
            focus: cx.focus_handle(),
            sel: None,
            prompt,
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
        self.focus.is_focused(window) || self.prompt.read(cx).focus_handle(cx).is_focused(window)
    }
}

/// A Home workspace row, in display order.
pub(super) struct HomeRow {
    pub(super) ix: usize,
    bucket: Bucket,
    label: Option<String>,
    running: usize,
}

impl Shark {
    /// A new workspace on the repos of the last one.
    pub(super) fn new_workspace_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_workspace(self.state.last_repos.clone(), None, window, cx)
    }

    /// Home is on screen: asked for, or nothing else to show.
    pub(crate) fn home_shown(&self) -> bool {
        self.home.on || self.active.is_none()
    }

    /// cmd-0: Home, or back to the open workspace.
    pub(super) fn toggle_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.home_shown() {
            true => self.leave_home(window, cx),
            false => self.enter_home(window, cx),
        }
    }

    fn leave_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_some() {
            self.record_active_interaction(cx);
            let tab = self.tab;
            self.show(tab, window, cx); // clears `home.on`
        }
    }

    pub(super) fn enter_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.home.on = true;
        self.zen = false;
        self.remember_open();
        self.refresh_home(None, cx);
        self.scan_approved(cx);
        let rows = self.home_rows(cx);
        // The cursor starts on the workspace just left.
        self.home.sel = self.active.clone().or(self.home.sel.take());
        if self.home_at(&rows).is_none() {
            self.home.sel = (rows.first()).map(|r| self.state.workspaces[r.ix].name.clone());
        }
        self.focus_fallback(window, cx);
        cx.notify();
    }

    /// Where keys go when nothing else has them: Home's list (its prompt while it has no
    /// rows), or the sidebar.
    pub(super) fn focus_fallback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (self.home_shown(), self.home_rows(cx).is_empty()) {
            (true, true) => self.home.prompt.update(cx, |p, cx| p.focus(window, cx)),
            (true, false) => self.home.focus.focus(window, cx),
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

    /// Re-reads, in the background, what each workspace (or just `only`) waits on and when it
    /// last changed.
    pub(super) fn refresh_home(&mut self, only: Option<&str>, cx: &mut Context<Self>) {
        let wss: Vec<Workspace> = (self.state.workspaces.iter())
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
        let ids: Vec<String> = (self.state.workspaces.iter())
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
        let mut rows: Vec<HomeRow> = (self.ws_order(true).into_iter())
            .map(|ix| {
                let name = &self.state.workspaces[ix].name;
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
        rows.sort_by_key(|r| r.bucket); // stable: sidebar order within a group
        rows
    }

    /// The cursor's position in `rows`.
    pub(super) fn home_at(&self, rows: &[HomeRow]) -> Option<usize> {
        let sel = self.home.sel.as_ref()?;
        rows.iter()
            .position(|r| &self.state.workspaces[r.ix].name == sel)
    }

    /// The cursor's position in `rows`, else the first row.
    pub(super) fn home_cursor(&self, rows: &[HomeRow]) -> Option<usize> {
        self.home_at(rows).or((!rows.is_empty()).then_some(0))
    }

    fn home_move(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let rows = self.home_rows(cx);
        if rows.is_empty() {
            return;
        }
        let to = nav.to(self.home_at(&rows).unwrap_or(0), rows.len(), 10);
        self.home.sel = Some(self.state.workspaces[rows[to].ix].name.clone());
        // The list's children are the rows with a heading before each group's first.
        let headings = rows[..=to].chunk_by(|a, b| a.bucket == b.bucket).count();
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

    fn home_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.home_rows(cx);
        if let Some(r) = self.home_cursor(&rows) {
            self.open_workspace(rows[r].ix, window, cx)
        }
    }

    /// esc: stop the answer being written, else back to the workspace.
    pub(super) fn home_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.cancel_ask(cx) {
            self.leave_home(window, cx)
        }
    }

    pub(super) fn toggle_ask_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.home.asking = !self.home.asking;
        let placeholder = if self.home.asking { ASK } else { BUILD };
        self.home
            .prompt
            .update(cx, |p, cx| p.set_placeholder(placeholder, window, cx));
        cx.notify();
    }

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if self.state.repos.is_empty() {
            window.push_notification(
                "Add a repo first: each workspace gets its own worktree of it",
                cx,
            );
            return self.add_repo(window, cx);
        }
        let first = (!text.is_empty()).then_some(text);
        self.new_workspace(self.state.last_repos.clone(), first, window, cx);
    }

    /// A workspace was created with `first` as its first message: clear the prompt if it
    /// came from there. A cancelled dialog leaves the prompt as typed.
    pub(super) fn home_prompt_used(
        &mut self,
        first: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = self.home.prompt.clone();
        if first.is_some_and(|f| !self.home.asking && prompt.read(cx).value().trim() == f) {
            prompt.update(cx, |p, cx| p.set_value("", window, cx));
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
            || (st.workspaces.iter()).any(|w| {
                (self.overview_repos(&w.name, cx))
                    .is_some_and(|r| r.iter().any(|r| r.pr().is_some()))
            });
        [
            (
                "Tools ready",
                !self.deps.is_empty() && self.deps.iter().all(|d| d.ok),
            ),
            ("Add a repo", !st.repos.is_empty()),
            ("Create a workspace", !st.workspaces.is_empty()),
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

    /// "Approve a plan": plan mode on in the cursor's (or the first) workspace.
    pub(super) fn try_plan_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.home_rows(cx);
        let Some(r) = self.home_cursor(&rows) else {
            return notify_err(window, cx, "Create a workspace first (cmd-n)");
        };
        let ix = rows[r].ix;
        // Leaving the open workspace asks about unsaved edits first; plan mode comes after.
        let leaving =
            (self.state.workspaces.get(ix)).is_some_and(|w| self.active.as_ref() != Some(&w.name));
        self.confirm_discard(leaving, leaving, window, cx, move |s, window, cx| {
            s.open_workspace(ix, window, cx);
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
        side_row(theme)
            .text_color(theme.sidebar_foreground)
            .id("home-row")
            .when(rail, super::sidebar::rail_button)
            .map(|d| cursor_row(d, self.home_shown(), false, false, theme))
            .when(self.home_shown(), |d| d.font_weight(FontWeight::MEDIUM))
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
                                .label("Back to workspace")
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

    fn render_prompt(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let focused = self
            .home
            .prompt
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let (title, enter, other) = match self.home.asking {
            true => ("Ask Shark", "ask", "new workspace"),
            false if self.state.repos.is_empty() => (
                "What do you want to build?",
                "add a repo first",
                "ask Shark",
            ),
            false => ("What do you want to build?", "pick repos", "ask Shark"),
        };
        v_flex()
            .gap_2()
            .child(div().text_base().text_color(theme.foreground).child(title))
            .child(
                div()
                    .key_context("HomePrompt")
                    .on_action(
                        cx.listener(|this, _: &SwitchField, w, cx| this.toggle_ask_mode(w, cx)),
                    )
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(if focused { theme.primary } else { theme.border })
                    .bg(theme.secondary)
                    .text_size(px(14.))
                    .child(Input::new(&self.home.prompt).appearance(false)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(crate::ui::hint("enter", enter))
                    .child(crate::ui::hint("tab", other)),
            )
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
        // Someone with workspaces knows their way around: one line, opened on click.
        if !self.state.workspaces.is_empty() && !self.home.checklist_open {
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
                    2 => ("New workspace", "cmd-n", Shark::new_workspace_default),
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

    fn render_ws_list(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.home_rows(cx);
        let theme = cx.theme();
        let focused = self.home.focus.is_focused(window);
        let list = v_flex()
            .id("home-list")
            .key_context("Home")
            .track_focus(&self.home.focus)
            .track_scroll(&self.home.rows)
            .gap_1();
        let list = nav_actions(list, true, cx, Shark::home_move)
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.home_confirm(w, cx)))
            .on_action(cx.listener(|this, _: &FocusComposer, w, cx| {
                this.home.prompt.update(cx, |p, cx| p.focus(w, cx))
            }));
        if rows.is_empty() {
            return self.render_empty(list, cx);
        }
        let now = SystemTime::now();
        let mut items: Vec<AnyElement> = vec![];
        for group in rows.chunk_by(|a, b| a.bucket == b.bucket) {
            items.push(
                h_flex()
                    .pt(if items.is_empty() { px(0.) } else { px(12.) })
                    .pb_1()
                    .gap_2()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(group[0].bucket.title())
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .child(group.len().to_string()),
                    )
                    .into_any_element(),
            );
            items.extend(group.iter().map(|r| self.home_ws_row(r, focused, now, cx)));
        }
        list.children(items)
            .child(
                h_flex()
                    .pt_2()
                    .gap_3()
                    .text_xs()
                    .text_color(faint(theme))
                    .child(crate::ui::hint("enter", "open"))
                    .child(crate::ui::hint("i", "prompt"))
                    .child(crate::ui::hint("cmd-backspace", "archive"))
                    .child(div().child("right-click for more")),
            )
            .into_any_element()
    }

    /// The list with no workspaces: add a repo, then start one.
    fn render_empty(&self, list: Stateful<Div>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let no_repos = self.state.repos.is_empty();
        let add = Button::new("home-add-repo").label("Add repo");
        let add = add
            .when(no_repos, |b| b.primary())
            .on_click(cx.listener(|s, _, w, cx| s.add_repo(w, cx)));
        let new = Button::new("home-new-ws").label("New workspace");
        let new = key_button(
            new.when(!no_repos, |b| b.primary()),
            "cmd-n",
            Shark::new_workspace_default,
            cx,
        );
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
                    .child("No workspaces yet"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(match no_repos {
                        true => "Add a git repo, then start a workspace on a new branch",
                        false => "A workspace is a branch with a worktree of each repo",
                    }),
            )
            .child(match no_repos {
                true => h_flex().gap_4().child(add).child(new),
                false => h_flex().gap_4().child(new).child(add),
            })
            .into_any_element()
    }

    /// One workspace's row on the list.
    fn home_ws_row(
        &self,
        r: &HomeRow,
        focused: bool,
        now: SystemTime,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let w = &self.state.workspaces[r.ix];
        let ix = r.ix;
        let selected = self.home.sel.as_deref() == Some(w.name.as_str());
        let repos = self.overview_repos(&w.name, cx);
        let merged = (repos.iter().copied().flatten())
            .any(|r| r.pr().is_some_and(|p| p.state == PrState::Merged));
        let when = (self.home.activity.get(&w.name))
            .and_then(|t| now.duration_since(*t).ok())
            .map(crate::sync::ago);
        let names = (w.repos.iter().map(|r| r.name.as_str()))
            .collect::<Vec<_>>()
            .join(" · ");
        let chats = match w.chats.len() {
            1 => "1 chat".to_string(),
            n => format!("{n} chats"),
        };
        let name = w.name.clone();
        let ws = w.clone();
        h_flex()
            .id(("home-ws", ix))
            .items_start()
            .px_3()
            .py_2()
            .gap_3()
            .rounded(theme.radius)
            .map(|d| list_cursor_row(d, selected, focused, theme))
            .on_mouse_down(MouseButton::Left, {
                let name = name.clone();
                cx.listener(move |this, _, _, cx| {
                    this.home.sel = Some(name.clone());
                    cx.notify()
                })
            })
            .on_click(cx.listener(move |this, _, w, cx| this.open_workspace(ix, w, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| {
                    this.home.sel = Some(ws.name.clone());
                    this.home.focus.focus(w, cx);
                    let entries = ws_menu(this, &ws, cx);
                    this.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(div().mt(px(3.)).child(super::sidebar::indicator(
                r.bucket,
                theme.muted_foreground,
                px(14.),
                theme,
            )))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .when(merged, |d| {
                                d.child(
                                    h_flex()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(theme.success_foreground)
                                        .child(Icon::new(IconName::GitMerge).with_size(px(12.)))
                                        .child("Merged"),
                                )
                            })
                            .child(div().flex_1())
                            .children(when.map(|t| {
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(t)
                            })),
                    )
                    .children(r.label.clone().map(|l| {
                        div()
                            .text_xs()
                            .text_color(theme.warning_foreground)
                            .child(l)
                    }))
                    .when(r.running > 1, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} chats running", r.running)),
                        )
                    })
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::GitBranch).with_size(px(12.)))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("{} · {names} · {chats}", w.branch)),
                            ),
                    )
                    .children(match repos {
                        None => vec![div().text_xs().text_color(faint(theme)).child("checking…")],
                        Some(repos) => (repos.iter())
                            .map(|r| {
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(home_summary(r))
                            })
                            .collect(),
                    }),
            )
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
