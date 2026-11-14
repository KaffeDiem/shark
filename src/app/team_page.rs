//! A team's page, a destination like Home: Goals, Agents and Activity.
use super::*;
use crate::state::Limits;
use crate::team::{self, Activity};
use gpui_kit::component::Selectable as _;
use gpui_kit::component::input::{Escape, Textarea, TextareaState};
use std::rc::Rc;

/// Which of the page's three views shows.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(super) enum TeamView {
    #[default]
    Goals,
    Agents,
    Activity,
}

type Anchors = Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>;

/// The shown team's page. Its list takes Home's focus handle and cursor (`home.focus`,
/// `home.sel`), so peek and the focus fallbacks work as they do on Home.
pub(super) struct TeamPage {
    pub(super) slug: String,
    pub(super) view: TeamView,
    /// "Describe the goal"; enter gives it to the team.
    pub(super) composer: Entity<TextareaState>,
    /// Finished goals are listed.
    finished_open: bool,
    /// The limits popover is open.
    pub(super) limits_open: bool,
    /// The team's log, newest first, read when the Activity view opens or refreshes.
    pub(super) activity: Vec<Activity>,
    /// The day label of each event of `activity`, worked out once when the log is read.
    activity_days: Vec<String>,
    activity_sel: usize,
    anchors: Anchors,
    _sub: Subscription,
}

/// Where a list line sits on the Goals view.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    Needs,
    Open,
    Done,
}

/// One cursor stop of the Goals view: a goal, or a goal's task that needs the user.
pub(super) struct Line {
    /// The task it opens.
    pub(super) task: String,
    /// The goal's task.
    goal: String,
    /// The plan row, for a goal's task.
    key: Option<String>,
    group: Group,
}

/// The child of a list that has the cursor's row, with one heading before each run of `keys`.
fn child_ix<K: PartialEq>(keys: &[K], i: usize) -> usize {
    i + keys[..=i].chunk_by(|a, b| a == b).count()
}

fn day_name(at: u64) -> String {
    use chrono::{Local, TimeZone};
    let day = |ms: u64| {
        Local
            .timestamp_millis_opt(ms as i64)
            .single()
            .map(|t| t.date_naive())
    };
    let (then, today) = (day(at), day(interaction::now_ms()));
    match (then, today) {
        (Some(a), Some(b)) if a == b => "Today".into(),
        (Some(a), Some(b)) if b.pred_opt() == Some(a) => "Yesterday".into(),
        (Some(a), _) => a.format("%A %-d %B").to_string(),
        _ => String::new(),
    }
}

fn clock(at: u64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_millis_opt(at as i64)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

fn team_chip(id: &'static str, icon: IconName, label: String) -> Button {
    Shark::chip(id, icon, label)
        .h(px(26.))
        .rounded(px(13.))
        .px(px(9.))
        .max_w(px(260.))
}

/// A stepper for one limit: the value between minus and plus.
fn stepper(
    id: &'static str,
    value: String,
    name: &'static str,
    cx: &Context<Shark>,
    step: fn(&mut Limits, i32),
) -> impl IntoElement {
    let change = |sign: i32| {
        cx.listener(move |s, _, w, cx| {
            if let Some(slug) = s.team_page.as_ref().map(|p| p.slug.clone()) {
                s.update_team(&slug, w, cx, |t| step(&mut t.limits, sign));
            }
        })
    };
    h_flex()
        .gap_1()
        .child(
            Button::new((id, 0usize))
                .ghost()
                .xsmall()
                .icon(IconName::Minus)
                .accessibility_label(format!("Decrease {name}"))
                .on_click(change(-1)),
        )
        .child(div().min_w(px(64.)).text_center().text_sm().child(value))
        .child(
            Button::new((id, 1usize))
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .accessibility_label(format!("Increase {name}"))
                .on_click(change(1)),
        )
}

fn bump(v: i64, sign: i32, step: i64, min: i64, max: i64) -> i64 {
    (v + i64::from(sign) * step).clamp(min, max)
}

impl Shark {
    /// The team whose page is on screen.
    pub(super) fn team_shown(&self) -> Option<&str> {
        let page = self.team_page.as_ref()?;
        (self.home.on && self.team(&page.slug).is_some()).then_some(page.slug.as_str())
    }

    /// Shows `slug`'s page over the open task, as Home does.
    pub(super) fn open_team(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.team(slug).is_none() {
            return notify_err(window, cx, "The team is no longer available");
        }
        // Opening the team already shown keeps its view.
        let kept = self
            .team_page
            .take()
            .filter(|p| p.slug == slug && self.home.on);
        self.enter_home(window, cx);
        let page = match kept {
            Some(p) => p,
            None => self.new_team_page(slug, window, cx),
        };
        self.team_page = Some(page);
        // A page that stayed on the Activity view shows the log as it is now.
        if self
            .team_page
            .as_ref()
            .is_some_and(|p| p.view == TeamView::Activity)
        {
            self.refresh_activity();
        }
        self.focus_team_page(window, cx);
        cx.notify();
    }

    fn new_team_page(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TeamPage {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .submit_on_enter(true)
                .placeholder("Describe the goal; enter starts planning")
        });
        let _sub = cx.subscribe_in(&composer, window, |s, _, ev: &InputEvent, w, cx| {
            if matches!(ev, InputEvent::PressEnter { shift: false, .. }) {
                s.submit_goal(w, cx);
            }
        });
        TeamPage {
            slug: slug.into(),
            view: TeamView::default(),
            composer,
            finished_open: false,
            limits_open: false,
            activity: vec![],
            activity_days: vec![],
            activity_sel: 0,
            anchors: Default::default(),
            _sub,
        }
    }

    /// **Give a goal…**: the page, with the composer ready to type.
    pub(super) fn open_team_composer(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_team(slug, window, cx);
        self.set_team_view(TeamView::Goals, window, cx);
        if let Some(p) = &self.team_page {
            p.composer.update(cx, |c, cx| c.focus(window, cx));
        }
    }

    /// Gives the composer's text to the team as a goal and clears it.
    fn submit_goal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.team_page else { return };
        let (slug, text) = (
            page.slug.clone(),
            page.composer.read(cx).value().to_string(),
        );
        if text.trim().is_empty() {
            return;
        }
        // Only a goal being created takes the text; a refused one leaves it to fix.
        let creating = self.creating.len();
        self.give_goal(&slug, &text, window, cx);
        if self.creating.len() > creating {
            if let Some(page) = &self.team_page {
                page.composer
                    .update(cx, |c, cx| c.set_value("", window, cx));
            }
        }
        cx.notify();
    }

    /// Puts the keyboard in the page: its list, or the composer when there is nothing to list.
    pub(super) fn focus_team_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.team_page else { return };
        let empty = page.view == TeamView::Goals && self.team_lines(cx).is_empty();
        match empty {
            true => page.composer.update(cx, |c, cx| c.focus(window, cx)),
            false => self.home.focus.focus(window, cx),
        }
    }

    /// Whether the page's list or one of its fields has the focus.
    pub(super) fn team_page_focused(&self, window: &Window, cx: &App) -> bool {
        self.home.focus.contains_focused(window, cx)
            || (self.team_page.as_ref())
                .is_some_and(|p| p.composer.read(cx).focus_handle(cx).is_focused(window))
    }

    pub(super) fn set_team_view(
        &mut self,
        view: TeamView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.team_page.as_mut() else {
            return;
        };
        page.view = view;
        page.limits_open = false;
        if view == TeamView::Activity {
            self.refresh_activity();
        }
        self.close_peek(cx);
        self.focus_team_page(window, cx);
        cx.notify();
    }

    /// Reads the team's log again, newest first.
    fn refresh_activity(&mut self) {
        let Some(page) = self.team_page.as_mut() else {
            return;
        };
        let mut log = team::activity(&page.slug);
        log.reverse();
        // ponytail: only the newest 200 events are kept; older ones stay in the file, unshown.
        log.truncate(200);
        page.activity_days = log.iter().map(|e| day_name(e.at)).collect();
        page.activity_sel = page.activity_sel.min(log.len().saturating_sub(1));
        page.activity = log;
    }

    /// esc: close the limits popover or the card, else back to the task, as Home does.
    fn team_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = self.team_page.as_mut().filter(|p| p.limits_open) {
            page.limits_open = false;
            self.focus_team_page(window, cx);
            return cx.notify();
        }
        if self.peek.task.is_some() {
            return self.close_peek(cx);
        }
        if self.active.is_some() {
            self.leave_home(window, cx);
        }
    }

    /// The Goals view's cursor stops: what needs the user, open goals, then finished goals
    /// while they are listed.
    pub(super) fn team_lines(&self, cx: &App) -> Vec<Line> {
        let Some(page) = &self.team_page else {
            return vec![];
        };
        let goals: Vec<_> = self.team_goals(&page.slug).collect();
        let (mut needs, mut lines) = (vec![], vec![]);
        for (task, goal) in goals.iter().rev().filter(|(_, g)| g.state.open()) {
            let name = &task.name;
            let own = matches!(self.attention_of(name, cx), Attn::Needs(_));
            let line = |task: &str, key: Option<&str>, group| Line {
                task: task.into(),
                goal: name.clone(),
                key: key.map(Into::into),
                group,
            };
            match own {
                true => needs.push(line(name, None, Group::Needs)),
                false => lines.push(line(name, None, Group::Open)),
            }
            for row in &goal.rows {
                let Some(t) = self.row_task(row) else {
                    continue;
                };
                if matches!(self.attention_of(&t.name, cx), Attn::Needs(_)) {
                    needs.push(line(&t.name, Some(&row.key), Group::Needs));
                }
            }
        }
        needs.extend(lines);
        if page.finished_open {
            let done = goals.iter().rev().filter(|(_, g)| !g.state.open());
            needs.extend(done.map(|(t, _)| Line {
                task: t.name.clone(),
                goal: t.name.clone(),
                key: None,
                group: Group::Done,
            }));
        }
        needs
    }

    /// The Goals view's cursor: the line `home.sel` names, else the first.
    pub(super) fn team_cursor(&self, lines: &[Line]) -> Option<usize> {
        (lines.iter())
            .position(|l| self.home.sel.as_deref() == Some(&l.task))
            .or((!lines.is_empty()).then_some(0))
    }

    /// The task under the Goals view's cursor, for space.
    pub(super) fn team_cursor_task(&self, cx: &App) -> Option<String> {
        (self.team_page.as_ref()).filter(|p| p.view == TeamView::Goals)?;
        let lines = self.team_lines(cx);
        self.team_cursor(&lines).map(|i| lines[i].task.clone())
    }

    fn team_move(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let Some(page) = &self.team_page else { return };
        match page.view {
            TeamView::Goals => {
                let lines = self.team_lines(cx);
                if lines.is_empty() {
                    return;
                }
                let at = self.team_cursor(&lines).unwrap_or(0);
                let to = nav.to(at, lines.len(), 10);
                self.home.sel = Some(lines[to].task.clone());
                self.follow_peek(self.home.sel.clone(), true);
                let groups: Vec<_> = lines.iter().map(|l| l.group as u8).collect();
                self.team_scroll_to(child_ix(&groups, to));
            }
            TeamView::Activity => {
                let n = page.activity.len();
                if n == 0 {
                    return;
                }
                let to = nav.to(page.activity_sel, n, 10);
                // The Refresh button is the list's first child.
                self.team_scroll_to(child_ix(&page.activity_days, to) + 1);
                self.team_page.as_mut().unwrap().activity_sel = to;
            }
            TeamView::Agents => {}
        }
        cx.notify();
    }

    /// Scrolls the page the least that shows the list's child `ix`, as of the last layout.
    fn team_scroll_to(&self, ix: usize) {
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

    /// enter: open the line under the cursor.
    fn team_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.team_page else { return };
        let target = match page.view {
            TeamView::Goals => {
                let lines = self.team_lines(cx);
                self.team_cursor(&lines).map(|i| lines[i].task.clone())
            }
            TeamView::Activity => page
                .activity
                .get(page.activity_sel)
                .and_then(|e| self.event_target(e)),
            TeamView::Agents => None,
        };
        if let Some(name) = target {
            self.close_peek(cx);
            self.open_task_named(&name, window, cx);
        }
    }

    /// The task an event is about: its plan row's, else the goal's.
    fn event_target(&self, e: &Activity) -> Option<String> {
        let goal = self.goal(&e.goal)?;
        let row = goal.rows.iter().find(|r| r.key == e.key);
        Some(
            row.and_then(|r| self.row_task(r))
                .map_or(e.goal.clone(), |t| t.name.clone()),
        )
    }

    pub(super) fn team_anchor(&self, key: &'static str, window: &Window) -> Point<Pixels> {
        (self.team_page.as_ref())
            .and_then(|p| p.anchors.borrow().get(key).copied())
            .map_or(window.mouse_position(), |b| {
                point(b.left(), b.bottom() + px(4.))
            })
    }

    /// `control`, remembering where it sits so a menu can open under it.
    pub(super) fn team_anchored(&self, key: &'static str, control: impl IntoElement) -> Div {
        let anchors = self
            .team_page
            .as_ref()
            .map(|p| p.anchors.clone())
            .unwrap_or_default();
        div().relative().flex_none().child(control).child(
            canvas(
                move |bounds, _, _| {
                    anchors.borrow_mut().insert(key, bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    /// The **You approve** menu: what stops for the user.
    pub(super) fn approve_menu(&self, slug: &str, cx: &Context<Self>) -> Vec<Entry> {
        let every = self.team(slug).is_some_and(|t| t.approve_tasks);
        [("Goal plan", false), ("Goal plan and every task", true)]
            .into_iter()
            .map(|(label, all)| {
                let slug = slug.to_string();
                menu::item(
                    None,
                    label,
                    on_shark(cx, move |s, w, cx| {
                        s.update_team(&slug, w, cx, |t| t.approve_tasks = all)
                    }),
                )
                .checked(every == all)
            })
            .collect()
    }

    /// Repositories the team reads, one checked entry each; the last one stays.
    fn team_repos_menu(&self, slug: &str, cx: &Context<Self>) -> Vec<Entry> {
        let Some(team) = self.team(slug) else {
            return vec![];
        };
        (self.state.repos.iter())
            .map(|r| {
                let (slug, name) = (slug.to_string(), r.name.clone());
                let on = team.repos.contains(&name);
                let only = on && team.repos.len() == 1;
                let item = menu::item(
                    IconName::FolderGit2,
                    r.name.clone(),
                    on_shark(cx, move |s, w, cx| {
                        if only {
                            return notify_err(w, cx, "A team needs a repository");
                        }
                        s.update_team(&slug, w, cx, |t| match t.repos.contains(&name) {
                            true => t.repos.retain(|r| *r != name),
                            false => t.repos.push(name.clone()),
                        })
                    }),
                );
                item.checked(on)
            })
            .collect()
    }

    fn render_limits(&self, team: &crate::state::Team, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let l = team.limits.clone();
        let rows = v_flex()
            .w(px(560.))
            .child(crate::ui::row(
                theme,
                false,
                "Running at once",
                "Chats in a turn across the team",
                stepper(
                    "limit-running",
                    l.running.to_string(),
                    "running at once",
                    cx,
                    |l, d| l.running = bump(l.running.into(), d, 1, 1, 10) as u8,
                ),
            ))
            .child(crate::ui::row(
                theme,
                true,
                "Allowance floor",
                "Nothing starts below this share of the plan allowance",
                stepper(
                    "limit-floor",
                    format!("{}%", l.floor),
                    "allowance floor",
                    cx,
                    |l, d| l.floor = bump(l.floor.into(), d, 5, 0, 90) as u8,
                ),
            ))
            .child(crate::ui::row(
                theme,
                true,
                "Agent time per goal",
                "A goal pauses when its agents have worked this long",
                stepper(
                    "limit-minutes",
                    crate::ui::hm(u64::from(l.minutes) * 60),
                    "agent time per goal",
                    cx,
                    |l, d| l.minutes = bump(l.minutes.into(), d, 30, 30, 1440) as u32,
                ),
            ))
            .child(crate::ui::row(
                theme,
                true,
                "Tasks per goal",
                "The most tasks a plan may have",
                stepper(
                    "limit-tasks",
                    l.tasks.to_string(),
                    "tasks per goal",
                    cx,
                    |l, d| l.tasks = bump(l.tasks.into(), d, 1, 1, 30) as u8,
                ),
            ));
        let panel = crate::menu::chrome(theme)
            .id("team-limits")
            .role(gpui_kit::accesskit::Role::Dialog)
            .aria_label("Limits")
            .test_support()
            .occlude()
            .child(rows)
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div().flex_1().child(
                            "Work stops when Shark quits. Tasks that were mid-turn show as stuck, with Retry, at the next launch.",
                        ),
                    )
                    .child(
                        Button::new("limits-done")
                            .ghost()
                            .xsmall()
                            .label("Done")
                            .on_click(cx.listener(|s, _, w, cx| s.team_escape(w, cx))),
                    ),
            );
        deferred(div().absolute().top(px(32.)).left_0().child(panel))
            .with_priority(1)
            .into_any_element()
    }

    pub(super) fn render_team_page(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let (Some(page), Some(slug)) = (&self.team_page, self.team_shown()) else {
            return div().into_any_element();
        };
        let Some(team) = self.team(slug) else {
            return div().into_any_element();
        };
        let slug = slug.to_string();
        let look = super::team_look::team_look_of(self.team_status(&slug, cx), cx.theme());
        let theme = cx.theme();
        let limits = format!(
            "Limits: {} at once · {} per goal",
            team.limits.running,
            crate::ui::hm(u64::from(team.limits.minutes) * 60)
        );
        let repos = match team.repos.as_slice() {
            [one] => one.clone(),
            many => format!("{} repositories", many.len()),
        };
        let approve = if team.approve_tasks {
            "Goal plan and every task"
        } else {
            "Goal plan"
        };
        let paused = team.paused;
        let header = h_flex()
            .h(px(44.))
            .flex_none()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .id("team-header-empty")
                    .flex_1()
                    .h_full()
                    .gap_2()
                    .child(super::agent_icon_view::agent_icon(&team.icon).size(px(16.)))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child(team.name.clone()),
                    )
                    .child(
                        h_flex()
                            .id("team-status")
                            .test_support()
                            .gap_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(look.icon(12.))
                            .child(look.label.clone()),
                    )
                    .map(|d| self.draggable_chrome("team-header-empty", d)),
            )
            .child(
                Button::new("team-pause")
                    .ghost()
                    .xsmall()
                    .icon(if paused {
                        IconName::Play
                    } else {
                        IconName::Pause
                    })
                    .label(if paused { "Resume" } else { "Pause" })
                    .tooltip(if paused {
                        "Resume starting this team's tasks"
                    } else {
                        "Start nothing new; running turns finish"
                    })
                    .on_click({
                        let slug = slug.clone();
                        cx.listener(move |s, _, w, cx| s.toggle_team_paused(&slug, w, cx))
                    }),
            )
            .when(self.active.is_some(), |d| {
                d.child(
                    Button::new("team-back")
                        .ghost()
                        .xsmall()
                        .label("Back to task")
                        .on_click(cx.listener(|s, _, w, cx| s.leave_home(w, cx))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("esc"),
                )
            });
        let chips = h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                self.team_anchored(
                    "approve",
                    team_chip(
                        "team-approve",
                        IconName::CircleCheck,
                        format!("You approve: {approve}"),
                    )
                    .tooltip("What you approve before work continues")
                    .on_click({
                        let slug = slug.clone();
                        cx.listener(move |s, _, w, cx| {
                            let (pos, entries) =
                                (s.team_anchor("approve", w), s.approve_menu(&slug, cx));
                            s.show_menu(pos, entries, w, cx)
                        })
                    }),
                ),
            )
            .child(
                self.team_anchored(
                    "limits",
                    team_chip("team-limits-chip", IconName::Clock, limits)
                        .tooltip("Limits for this team")
                        .selected(page.limits_open)
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(p) = s.team_page.as_mut() {
                                p.limits_open = !p.limits_open;
                            }
                            cx.notify();
                        })),
                )
                .when(page.limits_open, |d| d.child(self.render_limits(team, cx))),
            )
            .child(
                self.team_anchored(
                    "repos",
                    team_chip("team-repos", IconName::FolderGit2, repos)
                        .tooltip("Repositories the team reads")
                        .on_click({
                            let slug = slug.clone();
                            cx.listener(move |s, _, w, cx| {
                                let (pos, entries) =
                                    (s.team_anchor("repos", w), s.team_repos_menu(&slug, cx));
                                s.show_menu(pos, entries, w, cx)
                            })
                        }),
                ),
            );
        let views = h_flex().gap_1().children(
            [
                (TeamView::Goals, "Goals", "1"),
                (TeamView::Agents, "Agents", "2"),
                (TeamView::Activity, "Activity", "3"),
            ]
            .map(|(view, label, key)| {
                crate::ui::option_pill(label, None, page.view == view)
                    .tooltip(format!("{label} ({key})"))
                    .on_click(cx.listener(move |s, _, w, cx| s.set_team_view(view, w, cx)))
            }),
        );
        let body = match page.view {
            TeamView::Goals => self.render_goals(team, window, cx),
            TeamView::Agents => self.render_team_agents(team, cx),
            TeamView::Activity => self.render_activity(window, cx),
        };
        v_flex()
            .id("team-page-root")
            .key_context("TeamPage")
            .flex_1()
            .min_w_0()
            .h_full()
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| s.team_escape(w, cx)))
            .child(header)
            .child(
                div()
                    .id("team-page")
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
                            .py_6()
                            .gap_4()
                            .child(chips)
                            .child(views)
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    /// The list's own element: Home's focus handle, the keys of a team's list.
    pub(super) fn team_list(&self, id: &'static str, cx: &Context<Self>) -> Stateful<Div> {
        let list = v_flex()
            .id(id)
            .key_context("TeamList")
            .track_focus(&self.home.focus)
            .track_scroll(&self.home.rows);
        nav_actions(list, true, cx, Shark::team_move)
            .on_action(cx.listener(|s, _: &TogglePeek, w, cx| s.toggle_peek(w, cx)))
            .on_action(cx.listener(|s, _: &Confirm, w, cx| s.team_confirm(w, cx)))
            .on_action(
                cx.listener(|s, _: &TeamGoals, w, cx| s.set_team_view(TeamView::Goals, w, cx)),
            )
            .on_action(
                cx.listener(|s, _: &TeamAgents, w, cx| s.set_team_view(TeamView::Agents, w, cx)),
            )
            .on_action(
                cx.listener(|s, _: &TeamActivity, w, cx| {
                    s.set_team_view(TeamView::Activity, w, cx)
                }),
            )
    }

    fn render_goals(
        &self,
        team: &crate::state::Team,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let page = self.team_page.as_ref().unwrap();
        let theme = cx.theme();
        let lines = self.team_lines(cx);
        let focused = self.home.focus.is_focused(window);
        let cursor = self.team_cursor(&lines);
        let composer = crate::ui::card(theme)
            .id("team-composer")
            .test_support()
            .p_3()
            .gap_1()
            .capture_action(cx.listener(|s, _: &Escape, w, cx| {
                cx.stop_propagation();
                s.team_escape(w, cx)
            }))
            .child(Textarea::new(&page.composer).appearance(false).p_0())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "enter starts planning with {} · shift-enter adds a line",
                        self.agent_name(&team.lead)
                    )),
            );
        let finished = (self.team_goals(&team.slug))
            .filter(|(_, g)| !g.state.open())
            .count();
        let heading = |label: &'static str, n: usize| {
            h_flex()
                .id(label)
                .aria_label(format!("{label}: {n}"))
                .test_support()
                .h(px(28.))
                .px_3()
                .gap_2()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(label)
                .child(div().font_weight(FontWeight::NORMAL).child(n.to_string()))
        };
        let mut items: Vec<AnyElement> = vec![];
        let mut now_group = None;
        for (ix, line) in lines.iter().enumerate() {
            if now_group != Some(line.group) {
                now_group = Some(line.group);
                let n = lines.iter().filter(|l| l.group == line.group).count();
                let h = match line.group {
                    Group::Needs => heading("Needs you", n).into_any_element(),
                    Group::Open => heading("Goals", n)
                        .mt(px(if items.is_empty() { 0. } else { 12. }))
                        .into_any_element(),
                    Group::Done => self.finished_heading(finished, true, cx).into_any_element(),
                };
                items.push(h);
            }
            items.push(self.render_team_line(ix, line, cursor == Some(ix), focused, window, cx));
        }
        if !page.finished_open && finished > 0 {
            items.push(
                self.finished_heading(finished, false, cx)
                    .into_any_element(),
            );
        }
        let hints = h_flex()
            .pt_3()
            .px_3()
            .gap_3()
            .text_xs()
            .text_color(faint(theme))
            .child(crate::ui::hint("enter", "open"))
            .child(crate::ui::hint("space", "peek"))
            .child(crate::ui::hint("1 2 3", "views"));
        let list = self.team_list("team-list", cx);
        let list = match lines.is_empty() && finished == 0 {
            true => list.child(crate::ui::empty_state(
                theme,
                "team-empty",
                IconName::Users,
                "No goals yet",
                format!(
                    "Describe a goal above and {} starts planning.",
                    self.agent_name(&team.lead)
                ),
            )),
            false => list.children(items).child(hints),
        };
        v_flex()
            .gap_6()
            .child(composer)
            .child(list)
            .into_any_element()
    }

    fn finished_heading(&self, n: usize, open: bool, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        h_flex()
            .h(px(28.))
            .mt(px(12.))
            .px_3()
            .gap_2()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .child(
                Button::new("team-finished")
                    .ghost()
                    .xsmall()
                    .icon(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .accessibility_label(if open {
                        "Collapse finished goals"
                    } else {
                        "Expand finished goals"
                    })
                    .on_click(cx.listener(|s, _, _, cx| {
                        if let Some(p) = s.team_page.as_mut() {
                            p.finished_open = !p.finished_open;
                        }
                        cx.notify();
                    })),
            )
            .child("Finished")
            .child(div().font_weight(FontWeight::NORMAL).child(n.to_string()))
    }

    fn render_team_line(
        &self,
        ix: usize,
        line: &Line,
        selected: bool,
        focused: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(task) = self.task(&line.task) else {
            return div().into_any_element();
        };
        let goal = self.goal(&line.goal);
        let row = (line.key.as_ref()).and_then(|k| goal?.rows.iter().find(|r| &r.key == k));
        let look = match row {
            Some(r) => self.row_look(r, cx),
            None => self.goal_look(&line.goal, cx),
        };
        let title = match row {
            Some(r) => format!("{} {}", r.key, r.title),
            None => task.display_title().to_string(),
        };
        let goal_title = self
            .task(&line.goal)
            .map(|t| t.display_title().to_string())
            .unwrap_or_default();
        let progress = match (row, goal) {
            (None, Some(g)) => Some(team::progress(g))
                .filter(|p| p.1 > 0)
                .map(|(a, b)| format!("{a}/{b}")),
            _ => None,
        };
        let needs = matches!(line.group, Group::Needs);
        let name = task.name.clone();
        let hover_name = name.clone();
        let (menu_task, label) = (task.clone(), look.label.clone());
        h_flex()
            .relative()
            .id(("team-line", ix))
            .on_hover(
                cx.listener(move |s, over: &bool, _, cx| {
                    s.peek_hover(&hover_name, true, *over, cx)
                }),
            )
            .when(
                self.peek.task.as_deref() == Some(&task.name) && self.peek.home,
                |d| d.child(self.render_peek(task, window, cx)),
            )
            .role(gpui_kit::accesskit::Role::Button)
            .aria_label(match needs && row.is_some() {
                true => format!("Open {title} in {goal_title}, {label}"),
                false => format!("Open {title}, {label}"),
            })
            .test_support()
            .h(px(36.))
            .px_3()
            .gap_3()
            .rounded(theme.radius)
            .text_sm()
            .map(|d| list_cursor_row(d, selected, focused, theme))
            .on_mouse_down(MouseButton::Left, {
                let name = name.clone();
                cx.listener(move |s, _, _, cx| {
                    s.home.sel = Some(name.clone());
                    cx.notify()
                })
            })
            .on_click({
                let name = name.clone();
                cx.listener(move |s, _, w, cx| {
                    s.close_peek(cx);
                    s.open_task_named(&name, w, cx);
                })
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, e: &MouseDownEvent, w, cx| {
                    s.home.sel = Some(menu_task.name.clone());
                    s.home.focus.focus(w, cx);
                    let entries = ws_menu(s, &menu_task, cx);
                    s.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(div().flex_none().child(look.icon(14.)))
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.)
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(260.))
                    .truncate()
                    .text_xs()
                    .text_color(match needs {
                        true => theme.warning_foreground,
                        false => theme.muted_foreground,
                    })
                    .child(look.label.clone()),
            )
            .child(div().flex_1().min_w(px(12.)))
            .when(needs && row.is_some(), |d| {
                d.child(
                    div()
                        .min_w_0()
                        .max_w(px(240.))
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("in {goal_title}")),
                )
            })
            .children(progress.map(|p| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(p)
            }))
            .into_any_element()
    }

    fn render_activity(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let page = self.team_page.as_ref().unwrap();
        let theme = cx.theme();
        let focused = self.home.focus.is_focused(window);
        let mut items: Vec<AnyElement> = vec![];
        let mut day = String::new();
        for (ix, e) in page.activity.iter().enumerate() {
            let d = &page.activity_days[ix];
            if *d != day {
                day = d.clone();
                items.push(
                    h_flex()
                        .h(px(28.))
                        .px_3()
                        .mt(px(if items.is_empty() { 0. } else { 12. }))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(d.clone())
                        .into_any_element(),
                );
            }
            let goal = self.task(&e.goal).map(|t| t.display_title().to_string());
            let at = match (goal, e.key.is_empty()) {
                (Some(g), true) => g,
                (Some(g), false) => format!("{g} · {}", e.key),
                (None, _) => String::new(),
            };
            let target = self.event_target(e);
            let selected = page.activity_sel == ix;
            items.push(
                h_flex()
                    .id(("team-event", ix))
                    .role(gpui_kit::accesskit::Role::Button)
                    .aria_label(format!("{} {} {}, {}", clock(e.at), e.who, e.what, at))
                    .test_support()
                    .h(px(36.))
                    .px_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .text_sm()
                    .map(|d| list_cursor_row(d, selected, focused, theme))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, _, _, cx| {
                            if let Some(p) = s.team_page.as_mut() {
                                p.activity_sel = ix;
                            }
                            cx.notify()
                        }),
                    )
                    .on_click(cx.listener(move |s, _, w, cx| {
                        if let Some(name) = &target {
                            s.open_task_named(name, w, cx);
                        }
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(44.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(clock(e.at)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(140.))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(e.who.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink(1.)
                            .truncate()
                            .child(e.what.clone()),
                    )
                    .child(div().flex_1().min_w(px(12.)))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(260.))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(at),
                    )
                    .into_any_element(),
            );
        }
        let list = self.team_list("team-activity", cx).child(
            h_flex().justify_end().child(
                Button::new("team-activity-refresh")
                    .ghost()
                    .xsmall()
                    .label("Refresh")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.refresh_activity();
                        cx.notify();
                    })),
            ),
        );
        match items.is_empty() {
            true => list.child(crate::ui::empty_state(
                theme,
                "team-activity-empty",
                IconName::Activity,
                "Nothing has happened yet",
                "Goals given, plans approved and finished tasks show here.",
            )),
            false => list.children(items),
        }
        .into_any_element()
    }
}
