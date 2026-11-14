//! The Goal tab: a goal's plan as tasks, grouped by status, and the chip that leads back to it
//! from one of its tasks.
use super::menus::on_shark;
use super::team::RowStatus;
use super::*;
use crate::state::{Goal, GoalState, Row, RowState, Why};
use crate::team::{self, Hold};
use crate::ui::hm;
use gpui_kit::component::text::TextView;

/// The order the tab lists rows in; `group` maps a status to its place.
const GROUPS: [&str; 6] = [
    "Needs you",
    "Running",
    "Queued",
    "Ready for review",
    "Done",
    "Removed",
];

/// What the user can do to a row, by menu, inline button or key.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Open,
    Retry,
    Start,
    Remove,
    Reviewed,
}

impl Act {
    /// `Remove from plan…` when a confirmation follows, as it does for a started row.
    fn label(self, row: &Row) -> &'static str {
        match self {
            Act::Open => "Open task",
            Act::Retry => "Retry",
            Act::Start => "Start now",
            Act::Remove if row.state == RowState::Queued => "Remove from plan",
            Act::Remove => "Remove from plan…",
            Act::Reviewed => "Mark reviewed",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Act::Open => IconName::ArrowRight,
            Act::Retry => IconName::RotateCw,
            Act::Start => IconName::Play,
            Act::Remove => IconName::Trash,
            Act::Reviewed => IconName::Check,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Act::Open => "enter",
            Act::Retry => "r",
            Act::Start => "s",
            Act::Remove => "x",
            Act::Reviewed => "m",
        }
    }
}

fn group(status: &RowStatus) -> usize {
    match status {
        RowStatus::Needs(_) | RowStatus::Stuck(_) => 0,
        RowStatus::Running => 1,
        RowStatus::Queued => 2,
        RowStatus::Ready => 3,
        RowStatus::Done => 4,
        RowStatus::Skipped => 5,
    }
}

impl Shark {
    /// The open task's name, when it is a goal.
    pub(super) fn goal_open(&self) -> Option<String> {
        let task = self.active_task()?;
        task.goal.as_ref().map(|_| task.name.clone())
    }

    /// Whether the open goal's list is on screen.
    pub(super) fn goal_tab_shown(&self) -> bool {
        !self.home.on && self.tab == Tab::Goal && self.goal_open().is_some()
    }

    /// The goal's rows in the order the tab lists them, with their status.
    pub(super) fn goal_list(&self, name: &str, cx: &App) -> Vec<(Row, RowStatus)> {
        let rows = self.goal(name).map(|g| g.rows.as_slice()).unwrap_or(&[]);
        let mut list: Vec<_> = (rows.iter())
            .map(|r| (r.clone(), self.row_status(r, cx)))
            .collect();
        list.sort_by_key(|(_, s)| group(s));
        list
    }

    /// The cursor's row: `goal_sel` while it is still listed, else the first.
    fn goal_cursor(&self, list: &[(Row, RowStatus)]) -> Option<usize> {
        (list.iter())
            .position(|(r, _)| Some(&r.key) == self.goal_sel.as_ref())
            .or((!list.is_empty()).then_some(0))
    }

    fn goal_move(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let Some(name) = self.goal_open() else { return };
        let list = self.goal_list(&name, cx);
        let Some(at) = self.goal_cursor(&list) else {
            return;
        };
        let (row, _) = &list[nav.to(at, list.len(), 10)];
        self.goal_sel = Some(row.key.clone());
        let task = self.row_task(row).map(|t| t.name.clone());
        self.follow_peek(task, false);
        cx.notify();
    }

    /// Space: the cursor row's task as the card Home and the sidebar show.
    fn goal_peek(&mut self, cx: &mut Context<Self>) {
        if self.peek.task.is_some() || self.peek.browse {
            return self.close_peek(cx);
        }
        let Some(name) = self.goal_open() else { return };
        let list = self.goal_list(&name, cx);
        let task = (self.goal_cursor(&list))
            .and_then(|i| self.row_task(&list[i].0))
            .map(|t| t.name.clone());
        if task.is_some() {
            self.peek.task = task;
            (self.peek.keyboard, self.peek.home, self.peek.browse) = (true, false, false);
            cx.notify();
        }
    }

    /// The palette's goal rows: whether `label` applies to the open goal.
    pub(super) fn goal_row_applies(&self, label: &str) -> bool {
        let Some(state) = (self.goal_open().and_then(|n| self.goal(&n))).map(|g| g.state) else {
            return false;
        };
        match label {
            "Pause goal" => state == GoalState::Running,
            "Resume goal" => matches!(state, GoalState::Paused(_) | GoalState::Stopped),
            "Stop goal…" => matches!(state, GoalState::Running | GoalState::Paused(_)),
            _ => state == GoalState::Paused(Why::Time),
        }
    }

    /// Runs `f` on the open goal.
    pub(super) fn with_open_goal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: fn(&mut Shark, &str, &mut Window, &mut Context<Shark>),
    ) {
        if let Some(name) = self.goal_open() {
            f(self, &name, window, cx);
        }
    }

    /// **Stop goal…**: asks first.
    pub(super) fn confirm_stop_goal(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.to_string();
        crate::ui::confirm(
            "Stop this goal?".into(),
            "Running turns are interrupted and nothing new starts.",
            "Stop",
            move |s: &mut Shark, _, cx| s.stop_goal(&name, cx),
            window,
            cx,
        );
    }

    /// What `row` offers now, the first being the one obvious action.
    fn row_acts(&self, goal: &str, row: &Row, status: &RowStatus) -> Vec<Act> {
        let approve =
            (self.goal(goal).and_then(|g| self.team(&g.team))).is_some_and(|t| t.approve_tasks);
        let waits = matches!(
            self.row_hold(goal, &row.key),
            Some(Hold::After(_) | Hold::Review(_))
        );
        let mut acts = vec![];
        match status {
            RowStatus::Stuck(_) => acts.push(Act::Retry),
            RowStatus::Queued if !waits => acts.push(Act::Start),
            _ => {}
        }
        if approve && row.state == RowState::Finished && !row.reviewed {
            acts.insert(0, Act::Reviewed);
        }
        if row.task.is_some() {
            acts.push(Act::Open);
        }
        if *status != RowStatus::Skipped {
            acts.push(Act::Remove);
        }
        acts
    }

    fn do_row(
        &mut self,
        goal: &str,
        key: &str,
        act: Act,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = (self.goal(goal)).and_then(|g| g.rows.iter().find(|r| r.key == key)) else {
            return;
        };
        let row = row.clone();
        match act {
            Act::Open => {
                if let Some(task) = self.row_task(&row).map(|t| t.name.clone()) {
                    self.close_peek(cx);
                    self.open_task_named(&task, window, cx);
                }
            }
            Act::Retry => self.retry_row(goal, key, window, cx),
            Act::Start => self.start_row_now(goal, key, window, cx),
            Act::Reviewed => self.mark_reviewed(goal, key, window, cx),
            Act::Remove if row.state == RowState::Queued => self.remove_row(goal, key, window, cx),
            Act::Remove => {
                let (goal, key) = (goal.to_string(), key.to_string());
                crate::ui::confirm(
                    format!("Remove {key} from the plan?"),
                    "Its agent stops. Its task and changes stay.",
                    "Remove",
                    move |s, w, cx| s.remove_row(&goal, &key, w, cx),
                    window,
                    cx,
                );
            }
        }
    }

    /// The goal's team agents, for the reassign menus.
    fn goal_roster(&self, goal: &str) -> Vec<crate::state::AgentLink> {
        self.goal(goal)
            .map(|g| g.roster.clone())
            .unwrap_or_default()
    }

    /// `a`: the roster in a list to pick from, for a queued row.
    fn goal_reassign_list(
        &mut self,
        goal: &str,
        row: &Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let roster = self.goal_roster(goal);
        let (goal, key) = (goal.to_string(), row.key.clone());
        let at = roster.iter().position(|a| a.slug == row.agent).unwrap_or(0);
        let items = (roster.iter())
            .map(|a| CommandItem::new().label(a.name.clone()))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Reassign to…",
            items,
            at,
            move |s, i, _, cx| {
                if let Some(a) = s.goal_roster(&goal).get(i) {
                    s.reassign_row(&goal, &key, &a.slug.clone(), cx);
                }
            },
            window,
            cx,
        );
    }

    fn goal_row_menu(
        &self,
        goal: &str,
        row: &Row,
        status: &RowStatus,
        cx: &Context<Self>,
    ) -> Vec<Entry> {
        let mut entries: Vec<Entry> = vec![];
        for act in self.row_acts(goal, row, status) {
            let (g, k) = (goal.to_string(), row.key.clone());
            entries.push(menu::item(
                act.icon(),
                act.label(row),
                on_shark(cx, move |s, w, cx| s.do_row(&g, &k, act, w, cx)),
            ));
        }
        if *status == RowStatus::Queued {
            let reassign = (self.goal_roster(goal).into_iter())
                .map(|a| {
                    let (g, k, slug) = (goal.to_string(), row.key.clone(), a.slug.clone());
                    let pick = on_shark(cx, move |s, _, cx| s.reassign_row(&g, &k, &slug, cx));
                    menu::item(None, a.name, pick)
                        .agent_icon(a.icon)
                        .checked(a.slug == row.agent)
                })
                .collect();
            entries.push(menu::sub(IconName::Users, "Reassign", reassign));
        }
        entries
    }

    /// Enter, r, s, a, x and m on the cursor row.
    fn goal_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if e.keystroke.modifiers.modified() || !self.goal_focus.is_focused(window) {
            return;
        }
        let Some(name) = self.goal_open() else { return };
        let list = self.goal_list(&name, cx);
        let Some((row, status)) = self.goal_cursor(&list).map(|i| &list[i]) else {
            return;
        };
        let key = e.keystroke.key.as_str();
        if key == "a" && *status == RowStatus::Queued {
            self.goal_reassign_list(&name, row, window, cx);
        } else if let Some(act) =
            (self.row_acts(&name, row, status).into_iter()).find(|a| a.key() == key)
        {
            self.do_row(&name, &row.key, act, window, cx);
        } else {
            return;
        }
        cx.stop_propagation();
    }

    /// The lead's chat with its plan pane open, for a plan that waits.
    fn show_lead_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_chat(0, window, cx);
        if let Some(chat) = self.active_chat().cloned() {
            chat.update(cx, |c, cx| match c.plan_open {
                true => c.plan.read(cx).focus.clone().focus(window, cx),
                false => c.toggle_plan(window, cx),
            });
        }
    }

    /// "Codex" or "Claude": whose allowance `agent` runs on.
    fn provider_of(&self, agent: &str) -> &'static str {
        let model = (self.agent(agent).and_then(|a| a.model.clone()))
            .unwrap_or_else(|| self.state.new_model());
        match crate::codex::is_codex(Some(&model)) {
            true => "Codex",
            false => "Claude",
        }
    }

    /// One muted line when a queued row waits for the allowance; it needs no decision.
    fn allowance_line(&self, name: &str, goal: &Goal) -> Option<String> {
        let (row, hold) = (goal.rows.iter())
            .filter(|r| r.state == RowState::Queued)
            .find_map(|r| match self.row_hold(name, &r.key)? {
                h @ (Hold::Allowance(_) | Hold::Unknown) => Some((r, h)),
                _ => None,
            })?;
        let who = self.provider_of(&row.agent);
        Some(match (hold, self.allowance_reset(&row.agent)) {
            (Hold::Allowance(_), Some(at)) => format!(
                "Waiting for {who} allowance · resets {}",
                super::usage::local_time(at)
            ),
            (Hold::Allowance(_), None) => {
                format!("Waiting for {who} allowance · reset time unknown")
            }
            _ => format!("Waiting for {who} allowance · allowance unknown"),
        })
    }

    /// The Goal tab of the open goal task.
    pub(super) fn render_goal_tab(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(name) = self.goal_open() else {
            return div().into_any_element();
        };
        let task = self.task(&name).unwrap();
        let goal = task.goal.as_ref().unwrap();
        let team = self.team(&goal.team);
        let list = self.goal_list(&name, cx);
        let cursor = self.goal_cursor(&list);
        let look = self.goal_look(&name, cx);
        let (done, total) = team::progress(goal);
        let running = team.map_or(0, |t| self.team_running(&t.slug, cx));
        let brief_open = self.goal_brief.as_deref() == Some(name.as_str());
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let ready = goal.state == GoalState::Ready;
        let merged = ready && self.goal_merged(&name, cx);
        let plan_waits = !goal.draft.is_empty() || !goal.problems.is_empty();

        let mut facts = vec![format!("tasks {done} of {total}")];
        if let Some(t) = team {
            facts.push(format!("running {running} of {}", t.limits.running));
            facts.push(format!(
                "{} of {}",
                hm(goal.secs),
                hm(team::time_limit(goal, t))
            ));
        }
        let button =
            |id: &'static str, label: &'static str| Button::new(id).small().outline().label(label);
        let (n, n2) = (name.clone(), name.clone());
        let head = h_flex()
            .gap_2()
            .child(look.icon(16.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(task.display_title().into_owned()),
            )
            .child(div().text_sm().text_color(muted).child(look.label.clone()))
            .child(div().flex_1())
            .when(goal.state == GoalState::Running, |d| {
                d.child(crate::ui::toolbar(
                    "goal-pause",
                    button("goal-pause", "Pause")
                        .on_click(cx.listener(move |s, _, w, cx| s.pause_goal(&n, w, cx))),
                ))
            })
            .when(
                matches!(goal.state, GoalState::Paused(_) | GoalState::Stopped),
                |d| {
                    d.child(crate::ui::toolbar(
                        "goal-resume",
                        button("goal-resume", "Resume")
                            .on_click(cx.listener(move |s, _, w, cx| s.resume_goal(&n2, w, cx))),
                    ))
                },
            )
            .when(
                matches!(goal.state, GoalState::Running | GoalState::Paused(_)),
                |d| {
                    let name = name.clone();
                    d.child(crate::ui::toolbar(
                        "goal-stop",
                        button("goal-stop", "Stop").on_click(
                            cx.listener(move |s, _, w, cx| s.confirm_stop_goal(&name, w, cx)),
                        ),
                    ))
                },
            );

        let banner = match goal.state {
            GoalState::Paused(why) => {
                let (n, n2) = (name.clone(), name.clone());
                let (text, actions) = match why {
                    Why::Time => (
                        format!(
                            "Paused: agent time used, {} of {}. Running turns finish; nothing new starts.",
                            hm(goal.secs),
                            team.map_or(String::new(), |t| hm(team::time_limit(goal, t)))
                        ),
                        h_flex()
                            .gap_2()
                            .child(crate::ui::toolbar(
                                "goal-add-hour",
                                Button::new("goal-add-hour")
                                    .small()
                                    .primary()
                                    .label("Add 1 h")
                                    .on_click(
                                        cx.listener(move |s, _, w, cx| s.add_goal_hour(&n, w, cx)),
                                    ),
                            ))
                            .child(crate::ui::toolbar(
                                "goal-keep-paused",
                                button("goal-keep-paused", "Keep paused").on_click(
                                    cx.listener(move |s, _, w, cx| s.keep_goal_paused(&n2, w, cx)),
                                ),
                            )),
                    ),
                    Why::You => (
                        "Paused. Running turns finish; nothing new starts.".to_string(),
                        h_flex().child(crate::ui::toolbar(
                            "goal-banner-resume",
                            Button::new("goal-banner-resume")
                                .small()
                                .primary()
                                .label("Resume")
                                .on_click(cx.listener(move |s, _, w, cx| s.resume_goal(&n, w, cx))),
                        )),
                    ),
                };
                Some(
                    card(theme)
                        .id("goal-banner")
                        .test_support()
                        .p_3()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(look.icon(16.))
                                .child(div().flex_1().text_sm().child(text)),
                        )
                        .child(actions),
                )
            }
            _ => None,
        };

        let notice = plan_waits.then(|| {
            let problems = goal.problems.len();
            h_flex()
                .id("goal-plan-waits")
                .test_support()
                .gap_2()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(
                    Icon::new(IconName::CircleAlert)
                        .with_size(px(16.))
                        .text_color(theme.warning_foreground),
                )
                .child(div().flex_1().text_sm().child(match problems {
                    0 => "The lead's plan waits for your approval.".to_string(),
                    n => format!(
                        "The lead's plan waits for your approval; {n} line{} can't be read.",
                        crate::ui::plural(n)
                    ),
                }))
                .child(crate::ui::toolbar(
                    "goal-show-plan",
                    Button::new("goal-show-plan")
                        .small()
                        .outline()
                        .label("Show plan")
                        .on_click(cx.listener(|s, _, w, cx| s.show_lead_plan(w, cx))),
                ))
        });

        let mut rows: Vec<AnyElement> = vec![];
        let mut last = None;
        for (i, (row, status)) in list.iter().enumerate() {
            let g = group(status);
            if last != Some(g) {
                last = Some(g);
                let count = list.iter().filter(|(_, s)| group(s) == g).count();
                rows.push(
                    div()
                        .px_3()
                        .pt_2()
                        .child(crate::ui::section(theme, GROUPS[g]).into_any_element())
                        .id(("goal-group", g))
                        .aria_label(format!("{}, {count}", GROUPS[g]))
                        .into_any_element(),
                );
            }
            rows.push(self.goal_task_row(&name, row, status, cursor == Some(i), window, cx));
        }

        let lines = goal.brief.lines().count();
        let long = lines > 3 || goal.brief.len() > 240;
        let primary = match (ready, merged) {
            (true, true) => Some(("goal-finish", "Merged · finish goal")),
            (true, false) => Some(("goal-prs", "Create draft PRs")),
            _ => None,
        };
        let n = name.clone();
        let page = v_flex()
            .id("goal-page")
            .key_context("GoalList")
            .track_focus(&self.goal_focus)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .on_action(cx.listener(|s, _: &TogglePeek, _, cx| s.goal_peek(cx)))
            .on_action(
                cx.listener(|s, _: &CancelEdit, _, cx| match s.peek.task.is_some() {
                    true => s.close_peek(cx),
                    false => cx.propagate(),
                }),
            )
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, w, cx| s.goal_key(e, w, cx)))
            .on_click(cx.listener(|s, _, w, cx| s.goal_focus.focus(w, cx)));
        nav_actions(page, true, cx, Shark::goal_move)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(880.))
                    .mx_auto()
                    .p_4()
                    .gap_3()
                    .children(notice)
                    .child(head)
                    .child(div().text_xs().text_color(muted).child(facts.join(" · ")))
                    .children(banner)
                    .children(self.allowance_line(&name, goal).map(|line| {
                        div()
                            .id("goal-allowance")
                            .test_support()
                            .text_xs()
                            .text_color(muted)
                            .child(line)
                    }))
                    .when(ready, |d| {
                        let (id, label) = primary.unwrap();
                        d.child(h_flex().child(
                            crate::ui::toolbar(
                                id,
                                Button::new(id).small().primary().label(label).on_click(
                                    cx.listener(move |s, _, w, cx| match merged {
                                        true => s.finish_goal(&n, cx),
                                        false => s.create_goal_prs(&n, w, cx),
                                    }),
                                ),
                            ),
                        ))
                    })
                    .children(goal.summary.as_ref().map(|text| {
                        div()
                            .id("goal-summary")
                            .text_sm()
                            .child(TextView::markdown("goal-summary-text", text.clone()))
                    }))
                    .child(v_flex().gap_0p5().children(rows))
                    .children(list.get(cursor.unwrap_or(0)).map(|(row, status)| {
                        h_flex()
                            .gap_3()
                            .px_3()
                            .text_xs()
                            .text_color(muted)
                            .child(crate::ui::hint("↑ ↓", "move"))
                            .when(row.task.is_some(), |d| {
                                d.child(crate::ui::hint("enter", "open"))
                            })
                            .when(row.task.is_some(), |d| {
                                d.child(crate::ui::hint("space", "peek"))
                            })
                            .children(
                                self.row_acts(&name, row, status)
                                    .into_iter()
                                    .filter(|a| !matches!(a, Act::Open))
                                    .map(|a| crate::ui::hint(a.key(), a.label(row))),
                            )
                            .when(*status == RowStatus::Queued, |d| {
                                d.child(crate::ui::hint("a", "Reassign"))
                            })
                    }))
                    .when(!goal.brief.trim().is_empty(), |d| {
                        let name = name.clone();
                        d.child(
                            v_flex()
                                .gap_1()
                                .child(crate::ui::section(theme, "Brief"))
                                .child(
                                    div()
                                        .id("goal-brief")
                                        .text_sm()
                                        .when(!brief_open, |d| d.line_clamp(3))
                                        // The clamp counts wrapped lines, not the brief's own.
                                        .child(match brief_open {
                                            true => goal.brief.clone(),
                                            false => {
                                                let lines: Vec<&str> =
                                                    goal.brief.lines().take(3).collect();
                                                lines.join("\n")
                                            }
                                        }),
                                )
                                .when(long, |d| {
                                    d.child(
                                        h_flex().child(
                                            Button::new("goal-brief-toggle")
                                                .ghost()
                                                .xsmall()
                                                .label(if brief_open {
                                                    "Show less"
                                                } else {
                                                    "Show more"
                                                })
                                                .on_click(cx.listener(move |s, _, _, cx| {
                                                    let open = s.goal_brief.as_ref() == Some(&name);
                                                    s.goal_brief = (!open).then(|| name.clone());
                                                    cx.notify();
                                                })),
                                        ),
                                    )
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    /// One 36px row of the list.
    fn goal_task_row(
        &self,
        goal: &str,
        row: &Row,
        status: &RowStatus,
        selected: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.goal_focus.is_focused(window);
        let look = self.row_look(row, cx);
        let task = self.row_task(row);
        let repos = task.and_then(|t| self.overview_repos(&t.name, cx));
        let pr = repos.into_iter().flatten().find_map(|r| r.pr());
        let diff = repos.and_then(|rs| {
            rs.iter().try_fold((0u32, 0u32), |(a, d), r| {
                let (add, del) = r.local.as_ref().ok()?.diff?;
                Some((a.saturating_add(add), d.saturating_add(del)))
            })
        });
        let hold = match (status, self.row_hold(goal, &row.key)) {
            (RowStatus::Queued, Some(Hold::Review(k))) => Some(format!("review of {k}")),
            (RowStatus::Queued, Some(Hold::AgentPaused)) => Some("agent paused".to_string()),
            (RowStatus::Queued, Some(Hold::Allowance(_) | Hold::Unknown)) => {
                Some("allowance".to_string())
            }
            _ => None,
        };
        let after = (!row.after.is_empty()).then(|| format!("after {}", row.after.join(", ")));
        let agent = (self
            .goal(goal)
            .and_then(|g| g.roster.iter().find(|a| a.slug == row.agent)))
        .cloned();
        let agent_name = agent.as_ref().map_or(row.agent.clone(), |a| a.name.clone());
        let acts = self.row_acts(goal, row, status);
        let inline = acts
            .iter()
            .copied()
            .find(|a| !matches!(a, Act::Open | Act::Remove));
        let skipped = *status == RowStatus::Skipped;
        let theme = cx.theme();
        let (muted, fg) = (
            theme.muted_foreground,
            if skipped {
                theme.muted_foreground
            } else {
                theme.foreground
            },
        );
        let needs = (group(status) == 0).then(|| look.label.to_string());
        let changes = diff.map(|(a, d)| format!("+{a} −{d}"));
        let label = [
            Some(row.key.clone()),
            Some(row.title.clone()),
            Some(look.label.to_string()),
            after.clone(),
            hold.clone(),
            changes.clone(),
            pr.map(|p| format!("PR #{} {}", p.number, p.status().label())),
            Some(agent_name.clone()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let (g, k) = (goal.to_string(), row.key.clone());
        let (g2, k2, s2, r2) = (g.clone(), k.clone(), status.clone(), row.clone());
        let peeked = task.filter(|t| self.peek.task.as_deref() == Some(&t.name) && !self.peek.home);
        let el = h_flex()
            .relative()
            .id(SharedString::from(format!("goal-row-{}", row.key)))
            .test_support()
            .aria_label(label)
            .h(px(36.))
            .flex_none()
            .px_3()
            .gap_2()
            .rounded(theme.radius)
            .text_sm()
            .text_color(fg);
        list_cursor_row(el, selected, focused, theme)
            .when_some(peeked, |d, t| d.child(self.render_peek(t, window, cx)))
            .on_click(cx.listener(move |s, _, w, cx| {
                s.goal_sel = Some(k.clone());
                s.do_row(&g, &k, Act::Open, w, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, e: &MouseDownEvent, w, cx| {
                    s.goal_sel = Some(k2.clone());
                    s.goal_focus.focus(w, cx);
                    let entries = s.goal_row_menu(&g2, &r2, &s2, cx);
                    s.show_menu(e.position, entries, w, cx);
                }),
            )
            .child(div().flex_none().child(look.icon(14.)))
            .child(
                div()
                    .flex_none()
                    .w(px(28.))
                    .text_xs()
                    .text_color(muted)
                    .child(row.key.clone()),
            )
            .child(div().flex_1().min_w_0().truncate().child(row.title.clone()))
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .text_xs()
                    .text_color(muted)
                    .children(needs)
                    .children(after)
                    .children(hold)
                    .children(changes)
                    .children(pr.map(|p| {
                        h_flex()
                            .gap_1()
                            .child(p.status().icon(theme).with_size(px(12.)))
                            .child(format!("#{} {}", p.number, p.status().label()))
                    }))
                    .child(
                        h_flex()
                            .gap_1()
                            .children(agent.map(|a| {
                                crate::app::agent_icon_view::agent_icon(&a.icon).size(px(14.))
                            }))
                            .child(agent_name),
                    )
                    .children(inline.map(|act| {
                        let (g, k) = (goal.to_string(), row.key.clone());
                        Button::new(SharedString::from(format!("goal-{}-{k}", act.key())))
                            .ghost()
                            .xsmall()
                            .label(act.label(row))
                            .on_click(cx.listener(move |s, _, w, cx| {
                                cx.stop_propagation();
                                s.do_row(&g, &k, act, w, cx);
                            }))
                    })),
            )
            .into_any_element()
    }

    /// `Team › Goal › T2` at the start of the header of a goal's task; it opens the goal.
    pub(super) fn render_goal_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let part = self.active_task()?.part.as_ref()?;
        let team = self.goal(&part.goal).and_then(|g| self.team(&g.team))?;
        let label = format!("{} › {} › {}", team.name, part.goal_title, part.key);
        let goal = part.goal.clone();
        Some(
            div()
                .min_w_0()
                .flex_shrink_1()
                .child(crate::ui::toolbar(
                    "goal-chip",
                    Button::new("goal-chip")
                        .ghost()
                        .small()
                        .label(label.clone())
                        .accessibility_label(format!("Open goal: {label}"))
                        .on_click(cx.listener(move |s, _, w, cx| s.open_task_named(&goal, w, cx))),
                ))
                .into_any_element(),
        )
    }
}
