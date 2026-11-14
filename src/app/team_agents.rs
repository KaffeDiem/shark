//! The Agents view of a team's page: you, the lead and the agents, and what each is doing.
use super::*;
use crate::state::{GoalState, RowState, Team};
use std::rc::Rc;

/// What an agent is doing in a team, from the team's open goals.
struct AgentLine {
    text: String,
    needs: bool,
    /// The task it is working in, for **Open its task**.
    task: Option<String>,
}

impl Shark {
    /// Idle, `T2 running`, Needs you, or Paused in this team.
    fn agent_line(&self, team: &Team, slug: &str, cx: &App) -> AgentLine {
        let line = |text: &str, needs, task| AgentLine {
            text: text.into(),
            needs,
            task,
        };
        let mut running: Vec<(String, String)> = vec![];
        let (mut needs, mut planning) = (None, false);
        for (goal, g) in self.team_goals(&team.slug).filter(|(_, g)| g.state.open()) {
            if team.lead == slug {
                planning |= g.state == GoalState::Planning;
                if matches!(self.attention_of(&goal.name, cx), Attn::Needs(_)) {
                    needs.get_or_insert(goal.name.clone());
                }
            }
            for row in g.rows.iter().filter(|r| r.agent == slug) {
                let live = matches!(row.state, RowState::Started | RowState::Committing);
                let Some(task) = self.row_task(row).filter(|_| live) else {
                    continue;
                };
                if matches!(self.attention_of(&task.name, cx), Attn::Needs(_)) {
                    needs.get_or_insert(task.name.clone());
                }
                running.push((row.key.clone(), task.name.clone()));
            }
        }
        let first = running.first().map(|r| r.1.clone());
        if team.paused_agents.iter().any(|a| a == slug) {
            line("Paused in this team", false, first)
        } else if needs.is_some() {
            line("Needs you", true, needs)
        } else if !running.is_empty() {
            let keys: Vec<_> = running.iter().map(|r| r.0.as_str()).collect();
            line(&format!("{} running", keys.join(", ")), false, first)
        } else if planning {
            line("Planning", false, None)
        } else {
            line("Idle", false, None)
        }
    }

    /// An agent card's menu.
    pub(super) fn team_agent_menu(
        &self,
        team: &Team,
        slug: &str,
        cx: &Context<Self>,
    ) -> Vec<Entry> {
        let on_slug = |f: fn(&mut Shark, &str, &str, &mut Window, &mut Context<Shark>)| {
            let (team, agent) = (team.slug.clone(), slug.to_string());
            on_shark(cx, move |s, w, cx| f(s, &team, &agent, w, cx))
        };
        let paused = team.paused_agents.iter().any(|a| a == slug);
        let mut entries = vec![];
        if let Some(task) = self.agent_line(team, slug, cx).task {
            entries.push(menu::item(
                IconName::ExternalLink,
                "Open its task",
                on_shark(cx, move |s, w, cx| s.open_task_named(&task, w, cx)),
            ));
        }
        entries.push(menu::item(
            if paused {
                IconName::Play
            } else {
                IconName::Pause
            },
            if paused {
                "Resume in this team"
            } else {
                "Pause in this team"
            },
            on_slug(|s, team, agent, w, cx| {
                let agent = agent.to_string();
                s.update_team(team, w, cx, |t| match t.paused_agents.contains(&agent) {
                    true => t.paused_agents.retain(|a| *a != agent),
                    false => t.paused_agents.push(agent.clone()),
                })
            }),
        ));
        let lead = team.lead == slug;
        if !lead {
            entries.push(menu::item(
                IconName::Crown,
                "Make lead",
                on_slug(|s, team, agent, w, cx| s.set_team_lead(team, agent, w, cx)),
            ));
        }
        if self.agent(slug).is_some() {
            entries.push(menu::item(
                IconName::Pencil,
                "Edit agent…",
                on_slug(|s, _, agent, w, cx| s.open_agent(agent, w, cx)),
            ));
        }
        // Only where it can work: not the lead, and not the last agent besides the lead.
        if !lead && team.agents.iter().any(|a| a != slug) {
            entries.push(Entry::Sep);
            entries.push(menu::item(
                IconName::UserMinus,
                "Remove from team",
                on_slug(|s, team, agent, w, cx| {
                    if let Err(e) = s.remove_team_agent(team, agent, w, cx) {
                        notify_err(w, cx, e);
                    }
                }),
            ));
        }
        entries
    }

    /// **Add agent**: the agents not on the team, then **New agent…**.
    fn add_agent_menu(&self, team: &Team, cx: &Context<Self>) -> Vec<Entry> {
        let mut entries: Vec<Entry> = (self.state.agents.iter())
            .filter(|a| !team.members().any(|m| m == a.slug))
            .map(|a| {
                let (slug, agent) = (team.slug.clone(), a.slug.clone());
                menu::item(
                    None,
                    a.name.clone(),
                    on_shark(cx, move |s, w, cx| s.add_team_agent(&slug, &agent, w, cx)),
                )
                .agent_icon(a.icon.clone())
            })
            .collect();
        if !entries.is_empty() {
            entries.push(Entry::Sep);
        }
        let slug = team.slug.clone();
        entries.push(menu::item(
            IconName::Plus,
            "New agent…",
            on_shark(cx, move |s, w, cx| {
                let slug = slug.clone();
                let then: super::agent_settings::AfterCreate =
                    Rc::new(move |s, agent, w, cx| s.add_team_agent(&slug, agent, w, cx));
                s.agent_dialog(None, Some(then), w, cx)
            }),
        ));
        entries
    }

    fn add_team_agent(
        &mut self,
        slug: &str,
        agent: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = agent.to_string();
        self.update_team(slug, window, cx, |t| {
            if t.lead != agent && !t.agents.contains(&agent) {
                t.agents.push(agent);
            }
        });
    }

    /// One agent's card: avatar, name, harness and model, what it is doing.
    fn agent_card(&self, team: &Team, slug: &str, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let agent = self.agent(slug);
        let name = agent.map_or("Deleted agent".to_string(), |a| a.name.clone());
        let model = agent.and_then(|a| a.model.clone());
        let harness = if crate::codex::is_codex(model.as_deref()) {
            "Codex"
        } else {
            "Claude"
        };
        let model = model.map_or("default model".into(), |m| crate::models::model_label(&m));
        let line = self.agent_line(team, slug, cx);
        let lead = team.lead == slug;
        let (team_slug, open_slug) = (team.slug.clone(), slug.to_string());
        let menu_at = move |s: &mut Shark, pos, w: &mut Window, cx: &mut Context<Shark>| {
            if let Some(team) = s.team(&team_slug).cloned() {
                let entries = s.team_agent_menu(&team, &open_slug, cx);
                s.show_menu(pos, entries, w, cx);
            }
        };
        let menu_at = Rc::new(menu_at);
        let menu_btn = menu_at.clone();
        v_flex()
            .id(SharedString::from(format!("team-agent-{slug}")))
            .role(gpui_kit::accesskit::Role::Group)
            .aria_label(format!(
                "{name}{}, {}",
                if lead { ", lead" } else { "" },
                line.text
            ))
            .test_support()
            .relative()
            .w(px(176.))
            .p_3()
            .gap_1()
            .items_center()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, e: &MouseDownEvent, w, cx| menu_at(s, e.position, w, cx)),
            )
            .child(
                div().absolute().top_1().right_1().child(
                    Button::new(SharedString::from(format!("team-agent-menu-{slug}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .accessibility_label(format!("Actions for {name}"))
                        .on_click(cx.listener(move |s, _, w, cx| {
                            let pos = w.mouse_position();
                            menu_btn(s, pos, w, cx)
                        })),
                ),
            )
            .child(match agent {
                Some(a) => super::agent_icon_view::agent_icon(&a.icon)
                    .size(px(40.))
                    .into_any_element(),
                None => Icon::new(IconName::Bot)
                    .with_size(px(24.))
                    .into_any_element(),
            })
            .child(
                h_flex()
                    .gap_1()
                    .max_w_full()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .when(lead, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Lead"),
                        )
                    }),
            )
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{harness} · {model}")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if line.needs {
                        theme.warning_foreground
                    } else {
                        theme.muted_foreground
                    })
                    .child(line.text.clone()),
            )
            .into_any_element()
    }

    pub(super) fn render_team_agents(&self, team: &Team, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let rule = |h: f32| div().w(px(1.)).h(px(h)).flex_none().bg(theme.border);
        let you = v_flex()
            .id("team-you")
            .role(gpui_kit::accesskit::Role::Group)
            .aria_label("You")
            .test_support()
            .items_center()
            .gap_1()
            .child(
                div()
                    .size(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(Icon::new(IconName::User).with_size(px(18.))),
            )
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("You"))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                match team.approve_tasks {
                    true => "Approve plans and tasks",
                    false => "Approve plans",
                },
            ));
        let others = team.agents.len();
        let row = h_flex()
            .w_full()
            .flex_wrap()
            .justify_center()
            .items_start()
            .children(team.agents.iter().enumerate().map(|(i, slug)| {
                v_flex()
                    .items_center()
                    .child(
                        h_flex()
                            .w_full()
                            .child(div().flex_1().h(px(1.)).when(i > 0, |d| d.bg(theme.border)))
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(1.))
                                    .when(i + 1 < others, |d| d.bg(theme.border)),
                            ),
                    )
                    .child(rule(12.))
                    .child(div().px_1p5().child(self.agent_card(team, slug, cx)))
            }));
        let slug = team.slug.clone();
        let add = self.team_anchored(
            "add-agent",
            Button::new("team-add-agent")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .label("Add agent")
                .on_click(cx.listener(move |s, _, w, cx| {
                    let Some(team) = s.team(&slug).cloned() else {
                        return;
                    };
                    let (pos, entries) =
                        (s.team_anchor("add-agent", w), s.add_agent_menu(&team, cx));
                    s.show_menu(pos, entries, w, cx)
                })),
        );
        self.team_list("team-agents", cx)
            .test_support()
            .items_center()
            .gap_0()
            .child(you)
            .child(rule(16.))
            .child(self.agent_card(team, &team.lead, cx))
            .child(rule(16.))
            .child(row)
            .child(div().pt_4().child(add))
            .into_any_element()
    }
}
