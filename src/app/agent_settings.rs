//! Persistent agents: named instructions and defaults for independently created tasks.
use super::*;
use crate::models::{effort_label, efforts, model_label};
use crate::state::Agent;
use crate::ui::{card, row, section};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::text::TextView;

type Choice = Entity<SelectState<SearchableVec<SharedString>>>;

fn repo_model(state: &State, agent: &Agent) -> String {
    let task = Task {
        repos: state
            .repos
            .iter()
            .filter(|r| agent.repos.contains(&r.name))
            .map(|r| crate::state::TaskRepo {
                name: r.name.clone(),
                src: r.path.clone(),
                base: String::new(),
                branch: None,
            })
            .collect(),
        ..Default::default()
    };
    state.defaults_for(&task).0
}

fn create_skills_folder(slug: &str) -> std::io::Result<PathBuf> {
    let mut parts = Path::new(slug).components();
    if !matches!(parts.next(), Some(std::path::Component::Normal(_))) || parts.next().is_some() {
        return Err(std::io::Error::other("Invalid agent slug"));
    }
    let path = crate::state::agent_skills_dir(slug);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum AgentTab {
    #[default]
    Tasks,
    Instructions,
    History,
}

fn skills_button(slug: &str) -> Button {
    let slug = slug.to_string();
    Button::new("agent-skills")
        .ghost()
        .small()
        .icon(IconName::Folder)
        .label("Open skills folder")
        .on_click(move |_, window, cx| match create_skills_folder(&slug) {
            Ok(path) => cx.reveal_path(&path),
            Err(e) => notify_err(window, cx, e.to_string()),
        })
}

fn render_history(agent: &Agent, theme: &Theme) -> AnyElement {
    v_flex()
        .gap_2()
        .child(section(theme, "History"))
        .when(agent.history.is_empty(), |d| {
            d.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No task history yet"),
            )
        })
        .children(agent.history.iter().enumerate().map(|(i, record)| {
            let status = match record.merged {
                Some(true) => "Merged",
                Some(false) => "Not merged",
                None => "PR state unknown",
            };
            let date = i64::try_from(record.at)
                .ok()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map_or_else(
                    || "Date unknown".into(),
                    |at| {
                        at.with_timezone(&chrono::Local)
                            .format("%b %-d, %Y")
                            .to_string()
                    },
                );
            v_flex()
                .id(("agent-history", i))
                .aria_label(format!("{}, {status}, {date}", record.title))
                .test_support()
                .gap_1()
                .py_2()
                .when(i > 0, |d| d.border_t_1().border_color(theme.border))
                .child(div().text_sm().child(record.title.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{status} · {date}")),
                )
                .child(h_flex().gap_2().children(record.prs.iter().enumerate().map(
                    |(j, (repo, url))| {
                        let url = url.clone();
                        Button::new(SharedString::from(format!("agent-history-pr-{i}-{j}")))
                            .ghost()
                            .xsmall()
                            .role(Role::Link)
                            .label(format!("{repo} PR"))
                            .tooltip(url.clone())
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    },
                )))
        }))
        .into_any_element()
}

impl Shark {
    pub(super) fn create_named_agent(
        &mut self,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let name = name.trim();
        if self.state.read_only {
            return Err("Can't create agents: state.json couldn't be read".into());
        }
        if name.is_empty() {
            return Err("Enter an agent name".into());
        }
        let base = crate::state::slug(name);
        let base = if base.is_empty() { "agent" } else { &base };
        let mut slug = base.to_string();
        let mut suffix = 2;
        while self.state.agents.iter().any(|a| a.slug == slug)
            || crate::state::agent_skills_dir(&slug)
                .parent()
                .is_some_and(|p| p.exists())
        {
            slug = format!("{base}-{suffix}");
            suffix += 1;
        }
        self.state.agents.push(Agent {
            slug: slug.clone(),
            name: name.into(),
            icon: "bot".into(),
            ..Default::default()
        });
        if let Err(e) = self.state.save() {
            self.state.agents.pop();
            return Err(e.to_string());
        }
        cx.notify();
        Ok(slug)
    }

    pub(super) fn new_agent_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Agent name"));
        let (field, shark) = (input.clone(), cx.entity().downgrade());
        window.open_dialog(cx, move |d, _, _| {
            let (input, shark) = (input.clone(), shark.clone());
            crate::ui::dialog(d)
                .title("New agent")
                .w(px(440.))
                .child(Input::new(&input))
                .on_ok(move |_, window, cx| {
                    let name = input.read(cx).value().to_string();
                    let result = shark.update(cx, |s, cx| s.create_named_agent(&name, cx));
                    match result {
                        Ok(Ok(slug)) => {
                            let shark = shark.clone();
                            window.defer(cx, move |w, cx| {
                                shark
                                    .update(cx, |s, cx| {
                                        s.open_agent(&slug, w, cx);
                                        s.edit_agent(&slug, w, cx);
                                    })
                                    .ok();
                            });
                            true
                        }
                        Ok(Err(e)) => {
                            notify_err(window, cx, e);
                            false
                        }
                        Err(_) => true,
                    }
                })
        });
        field.update(cx, |i, cx| i.focus(window, cx));
    }

    pub(super) fn open_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.agents.iter().any(|a| a.slug == slug) {
            return;
        }
        window.close_all_dialogs(cx);
        self.enter_home(window, cx);
        self.home.destination = super::home::Destination::Agent(slug.to_string());
        self.home.agent_tab = AgentTab::Tasks;
        let tasks = self.home_rows(cx);
        if self.home_at(&tasks).is_none() {
            self.home.sel = tasks
                .first()
                .map(|row| self.state.tasks[row.ix].name.clone());
        }
        self.home.page.set_offset(point(px(0.), px(0.)));
        self.home.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_agent_page(
        &self,
        slug: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(agent) = self.state.agents.iter().find(|a| a.slug == slug) else {
            return div().child("Agent no longer exists").into_any_element();
        };
        let theme = cx.theme();
        let refresh_slug = slug.to_string();
        let repos = agent
            .repos
            .iter()
            .filter(|name| self.state.repos.iter().any(|r| &r.name == *name))
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        let content = match self.home.agent_tab {
            AgentTab::Tasks => {
                let tasks = self
                    .home_rows(cx)
                    .into_iter()
                    .filter(|r| {
                        self.state.tasks[r.ix]
                            .agent
                            .as_ref()
                            .is_some_and(|a| a.slug == slug)
                    })
                    .collect::<Vec<_>>();
                let list = v_flex()
                    .id("agent-tasks")
                    .key_context("Home")
                    .track_focus(&self.home.focus)
                    .track_scroll(&self.home.rows)
                    .gap_2();
                nav_actions(list, true, cx, Shark::home_move)
                    .on_action(cx.listener(|s, _: &Confirm, w, cx| s.home_confirm(w, cx)))
                    .test_support()
                    .when(tasks.is_empty(), |d| {
                        d.child(
                            v_flex()
                                .id("agent-empty-tasks")
                                .test_support()
                                .py_8()
                                .gap_2()
                                .child(div().text_base().child("No tasks yet"))
                                .child(div().text_sm().text_color(theme.muted_foreground).child(
                                    format!(
                                        "Start a task with {} whenever you're ready.",
                                        agent.name
                                    ),
                                )),
                        )
                    })
                    .children(tasks.iter().map(|task| {
                        self.home_ws_row(
                            task,
                            self.home.focus.is_focused(window),
                            std::time::SystemTime::now(),
                            cx,
                        )
                    }))
                    .into_any_element()
            }
            AgentTab::Instructions => {
                let model = agent
                    .model
                    .as_deref()
                    .filter(|m| MODELS.contains(m))
                    .map(model_label)
                    .unwrap_or_else(|| {
                        format!(
                            "Repository default ({})",
                            model_label(&repo_model(&self.state, agent))
                        )
                    });
                let effort = agent
                    .effort
                    .as_deref()
                    .map(effort_label)
                    .unwrap_or_else(|| "Repository default".into());
                v_flex()
                    .id("agent-instructions-page")
                    .test_support()
                    .gap_5()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(section(theme, "Instructions"))
                            .child(
                                TextView::markdown(
                                    "agent-instructions-text",
                                    if agent.instructions.is_empty() {
                                        "No instructions yet. Edit this agent to add them."
                                            .to_string()
                                    } else {
                                        agent.instructions.clone()
                                    },
                                )
                                .selectable(true),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(section(theme, "Defaults"))
                            .child(div().text_sm().child(format!(
                                "Repositories: {}",
                                if repos.is_empty() { "None" } else { &repos }
                            )))
                            .child(div().text_sm().child(format!("Model: {model}")))
                            .child(div().text_sm().child(format!("Thinking: {effort}"))),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(section(theme, "Skills"))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(
                                "Add each skill as <skill>/SKILL.md in this folder, then Refresh skills to update every open task.",
                            ))
                            .child(h_flex().gap_2().child(skills_button(slug))
                            .child(Button::new("agent-refresh-skills").ghost().small().icon(IconName::RefreshCw)
                                .label("Refresh skills").on_click(cx.listener(move |s, _, window, cx| {
                                    match s.sync_agent(&refresh_slug, true, cx) {
                                        Ok(()) => window.push_notification(Notification::success("Agent skills refreshed"), cx),
                                        Err(e) => notify_err(window, cx, format!("Couldn't refresh agent skills: {e}")),
                                    }
                                })))),
                    )
                    .into_any_element()
            }
            AgentTab::History => render_history(agent, theme),
        };
        let (new_slug, edit_slug, composer_slug) =
            (slug.to_string(), slug.to_string(), slug.to_string());
        v_flex()
            .id("agent-page")
            .aria_label(format!("{} agent", agent.name))
            .key_context("HomeView")
            .track_focus(&self.home.focus)
            .on_action(cx.listener(|s, _: &CancelEdit, w, cx| s.home_escape(w, cx)))
            .on_action(cx.listener(move |s, _: &FocusComposer, w, cx| {
                s.prefill_agent_task(&composer_slug, w, cx)
            }))
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .id("agent-page-scroll")
                    .track_scroll(&self.home.page)
                    .overflow_y_scroll()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(900.))
                            .mx_auto()
                            .px_6()
                            .py_8()
                            .gap_6()
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_3()
                                    .child(
                                        Icon::new(super::sidebar::section_icon(&agent.icon))
                                            .with_size(px(28.)),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_xl()
                                                    .truncate()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(agent.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .truncate()
                                                    .text_color(theme.muted_foreground)
                                                    .child(format!("@{}", agent.slug)),
                                            ),
                                    )
                                    .child(
                                        Button::new("agent-page-new-task")
                                            .flex_none()
                                            .primary()
                                            .icon(IconName::Plus)
                                            .label("New task")
                                            .on_click(cx.listener(move |s, _, w, cx| {
                                                s.prefill_agent_task(&new_slug, w, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("agent-page-edit")
                                            .flex_none()
                                            .ghost()
                                            .icon(IconName::Pencil)
                                            .label("Edit agent")
                                            .on_click(cx.listener(move |s, _, w, cx| {
                                                s.edit_agent(&edit_slug, w, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .id("agent-default-repos")
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(if repos.is_empty() {
                                        "No default repositories".to_string()
                                    } else {
                                        format!("Default repositories: {repos}")
                                    })
                                    .test_support(),
                            )
                            .child(
                                h_flex().gap_2().flex_wrap().children(
                                    [
                                        (AgentTab::Tasks, "Tasks"),
                                        (AgentTab::Instructions, "Instructions & skills"),
                                        (AgentTab::History, "History"),
                                    ]
                                    .into_iter()
                                    .map(|(tab, label)| {
                                        crate::ui::option_pill(
                                            label,
                                            None,
                                            self.home.agent_tab == tab,
                                        )
                                        .on_click(
                                            cx.listener(move |s, _, window, cx| {
                                                s.home.agent_tab = tab;
                                                s.home.page.set_offset(point(px(0.), px(0.)));
                                                s.home.focus.focus(window, cx);
                                                cx.notify();
                                            }),
                                        )
                                    }),
                                ),
                            )
                            .child(content),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn edit_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.state.agents.iter().find(|a| a.slug == slug).cloned() else {
            return;
        };
        let shark = cx.entity().downgrade();
        let gone = shark.clone();
        let last = repo_model(&self.state, &agent);
        let view = cx.new(|cx| AgentSettingsView::new(shark, agent, last, window, cx));
        let slug = slug.to_string();
        window.open_dialog(cx, move |d, _, _| {
            let (gone, slug) = (gone.clone(), slug.clone());
            crate::ui::dialog(d)
                .w(px(640.))
                .child(view.clone())
                .on_close(move |_, window, cx| {
                    let (gone, slug) = (gone.clone(), slug.clone());
                    window.defer(cx, move |window, cx| {
                        gone.update(cx, |s, cx| {
                            if let Err(e) = s.sync_agent(&slug, true, cx) {
                                notify_err(window, cx, format!("Couldn't refresh agent instructions: {e}. Reopen agent settings and close them to retry."));
                            }
                        }).ok();
                    });
                })
        });
    }

    pub(super) fn pick_agent(
        &mut self,
        new_task: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.agents.is_empty() {
            return self.new_agent_dialog(window, cx);
        }
        let agents = self.state.agents.clone();
        let items = agents
            .iter()
            .map(|a| {
                CommandItem::new()
                    .label(a.name.clone())
                    .icon(super::sidebar::section_icon(&a.icon))
            })
            .collect();
        open_menu(
            cx.entity().downgrade(),
            if new_task {
                "New task for agent…"
            } else {
                "Open agent…"
            },
            items,
            0,
            move |s, i, w, cx| {
                if new_task {
                    s.prefill_agent_task(&agents[i].slug, w, cx)
                } else {
                    s.open_agent(&agents[i].slug, w, cx)
                }
            },
            window,
            cx,
        );
    }

    pub(super) fn can_delete_agent(
        &self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.state.read_only {
            notify_err(
                window,
                cx,
                "Can't delete agents: state.json couldn't be read",
            );
            return false;
        }
        let mut parts = Path::new(slug).components();
        if !matches!(parts.next(), Some(std::path::Component::Normal(_))) || parts.next().is_some()
        {
            notify_err(window, cx, "Can't delete an agent with an invalid slug");
            return false;
        }
        let parent = crate::state::support_dir().join("agents");
        match std::fs::symlink_metadata(&parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                notify_err(
                    window,
                    cx,
                    "Can't delete agent skills through a linked agents folder",
                );
                return false;
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                notify_err(window, cx, e.to_string());
                return false;
            }
            _ => {}
        }
        if self
            .creating
            .iter()
            .any(|name| name.starts_with(&format!("{slug}-")))
        {
            notify_err(window, cx, "Wait for the agent's task to finish creating");
            return false;
        }
        let count = self
            .state
            .tasks
            .iter()
            .filter(|t| t.agent.as_ref().is_some_and(|a| a.slug == slug))
            .count();
        if count == 0 {
            return true;
        }
        let name = self
            .state
            .agents
            .iter()
            .find(|a| a.slug == slug)
            .map_or(slug, |a| &a.name);
        notify_err(
            window,
            cx,
            format!("Finish or delete {name}'s {count} tasks first"),
        );
        false
    }

    fn delete_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_delete_agent(slug, window, cx) {
            return;
        }
        let Some(agent) = self.state.agents.iter().find(|a| a.slug == slug) else {
            return;
        };
        let slug = slug.to_string();
        confirm(
            format!("Delete {}?", agent.name),
            "Its instructions, history and skills are deleted.",
            "Delete",
            move |s: &mut Self, window, cx| {
                if !s.can_delete_agent(&slug, window, cx) {
                    return;
                }
                let dir = crate::state::agent_skills_dir(&slug)
                    .parent()
                    .unwrap()
                    .to_path_buf();
                let cursor_slug = match s.sel {
                    SideRow::Agent(i) => s.state.agents.get(i).map(|a| a.slug.clone()),
                    SideRow::Task(_) => None,
                };
                let cursor_at = s.side_rows(false).iter().position(|row| *row == s.sel);
                let agents = s.state.agents.clone();
                let sections = s.state.sections.clone();
                s.state.agents.retain(|a| a.slug != slug);
                for section in &mut s.state.sections {
                    if section.agent.as_deref() == Some(&slug) {
                        section.agent = None;
                    }
                }
                if let Err(e) = s.state.save() {
                    s.state.agents = agents;
                    s.state.sections = sections;
                    return notify_err(window, cx, e.to_string());
                }
                if let Some(cursor_slug) = cursor_slug {
                    s.sel = s
                        .state
                        .agents
                        .iter()
                        .position(|a| a.slug == cursor_slug)
                        .map(SideRow::Agent)
                        .unwrap_or_else(|| {
                            let rows = s.side_rows(false);
                            rows.get(cursor_at.unwrap_or(0).min(rows.len().saturating_sub(1)))
                                .copied()
                                .unwrap_or(SideRow::Task(0))
                        });
                }
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        notify_err(
                            window,
                            cx,
                            format!("Agent deleted, but couldn't remove {}: {e}", dir.display()),
                        );
                    }
                }
                window.close_dialog(cx);
                s.collapsed_agents.remove(&slug);
                if s.home.destination == super::home::Destination::Agent(slug.clone()) {
                    s.enter_home(window, cx);
                    s.home.destination = super::home::Destination::AllTasks;
                    s.home.focus.focus(window, cx);
                }
                cx.notify();
            },
            window,
            cx,
        );
    }
}

struct AgentSettingsView {
    shark: WeakEntity<Shark>,
    slug: String,
    last: String,
    name: Entity<InputState>,
    instructions: Entity<TextareaState>,
    model: Choice,
    effort: Choice,
    _subs: Vec<Subscription>,
}

impl AgentSettingsView {
    fn new(
        shark: WeakEntity<Shark>,
        a: Agent,
        last: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).default_value(a.name.clone()));
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 14)
                .default_value(a.instructions.clone())
        });
        let mut models = vec![SharedString::from("Repository default")];
        models.extend(MODELS.iter().map(|m| model_label(m).into()));
        let at = a
            .model
            .as_deref()
            .and_then(|m| MODELS.iter().position(|x| *x == m))
            .map_or(0, |i| i + 1);
        let model = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(models),
                Some(IndexPath::new(at)),
                window,
                cx,
            )
        });
        let effort = Self::effort_choice(&a, &last, window, cx);
        let mut this = Self {
            shark,
            slug: a.slug,
            last,
            name,
            instructions,
            model,
            effort,
            _subs: vec![],
        };
        this._subs
            .push(cx.subscribe(&this.name, |this, field, ev, cx| {
                if let InputEvent::Change = ev {
                    let name = field.read(cx).value().trim().to_string();
                    if !name.is_empty() {
                        this.update(cx, |a| a.name = name);
                    }
                }
            }));
        this._subs
            .push(cx.subscribe(&this.instructions, |this, field, ev, cx| {
                if let InputEvent::Change = ev {
                    let text = field.read(cx).value().to_string();
                    this.update(cx, |a| a.instructions = text);
                }
            }));
        this._subs
            .push(cx.subscribe_in(&this.model, window, |this, _, ev, w, cx| {
                let SelectEvent::Confirm(Some(v)) = ev else {
                    return;
                };
                let model = MODELS
                    .iter()
                    .find(|m| model_label(m) == v.as_ref())
                    .map(|m| m.to_string());
                let last = this.last.clone();
                if let Some(a) = this.update(cx, |a| {
                    a.model = model;
                    a.effort = a.effort.take().filter(|e| {
                        efforts(a.model.as_deref().unwrap_or(&last)).contains(&e.as_str())
                    });
                }) {
                    this.effort = Self::effort_choice(&a, &this.last, w, cx);
                    this._subs.push(this.watch_effort(w, cx));
                }
            }));
        this._subs.push(this.watch_effort(window, cx));
        this
    }

    fn effort_choice(a: &Agent, last: &str, window: &mut Window, cx: &mut Context<Self>) -> Choice {
        let levels = efforts(a.model.as_deref().unwrap_or(last));
        let mut items = vec![SharedString::from("Repository default")];
        items.extend(levels.iter().map(|l| effort_label(l).into()));
        let at = a
            .effort
            .as_deref()
            .and_then(|e| levels.iter().position(|l| *l == e))
            .map_or(0, |i| i + 1);
        cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(items),
                Some(IndexPath::new(at)),
                window,
                cx,
            )
        })
    }

    fn watch_effort(&self, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(&self.effort, window, |this, _, ev, _, cx| {
            let SelectEvent::Confirm(Some(v)) = ev else {
                return;
            };
            let last = this.last.clone();
            this.update(cx, |a| {
                a.effort = efforts(a.model.as_deref().unwrap_or(&last))
                    .iter()
                    .find(|l| effort_label(l) == v.as_ref())
                    .map(|l| l.to_string());
            });
        })
    }

    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Agent)) -> Option<Agent> {
        let result = self
            .shark
            .update(cx, |s, cx| {
                let a = s.state.agents.iter_mut().find(|a| a.slug == self.slug)?;
                f(a);
                let agent = a.clone();
                let _ = s.state.save();
                cx.notify();
                Some(agent)
            })
            .ok()
            .flatten();
        cx.notify();
        result
    }

    fn set_repo(&mut self, repo: &str, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut agent) = self.update(cx, |a| {
            a.repos.retain(|r| r != repo);
            if on {
                a.repos.push(repo.into());
            }
        }) else {
            return;
        };
        if let Some(shark) = self.shark.upgrade() {
            self.last = repo_model(&shark.read(cx).state, &agent);
        }
        let levels = efforts(agent.model.as_deref().unwrap_or(&self.last));
        if agent
            .effort
            .as_deref()
            .is_some_and(|e| !levels.contains(&e))
        {
            agent.effort = None;
            self.update(cx, |a| a.effort = None);
        }
        self.effort = Self::effort_choice(&agent, &self.last, window, cx);
        self._subs.push(self.watch_effort(window, cx));
    }

    fn pick_icon(&self, window: &mut Window, cx: &mut Context<Self>) {
        let slug = self.slug.clone();
        self.shark
            .update(cx, |s, cx| s.pick_agent_icon(&slug, window, cx))
            .ok();
    }
}

impl Render for AgentSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.shark.upgrade().map(|s| {
            let s = s.read(cx);
            (
                s.state.agents.iter().find(|a| a.slug == self.slug).cloned(),
                s.state.repos.clone(),
            )
        });
        let (agent, repos) = state.unwrap_or_default();
        let Some(agent) = agent else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let toggles = repos
            .into_iter()
            .enumerate()
            .map(|(i, repo)| {
                let checked = agent.repos.contains(&repo.name);
                Checkbox::new(("agent-repo", i))
                    .label(repo.name.clone())
                    .checked(checked)
                    .on_click(cx.listener(move |this, on: &bool, window, cx| {
                        this.set_repo(&repo.name, *on, window, cx);
                    }))
            })
            .collect::<Vec<_>>();
        v_flex()
            .id("agent-settings")
            .test_support()
            .gap_4()
            .max_h(px(600.))
            .overflow_y_scroll()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("agent-icon")
                            .accessibility_label("Change agent icon")
                            .ghost()
                            .icon(super::sidebar::section_icon(&agent.icon))
                            .tooltip("Change icon")
                            .on_click(cx.listener(|this, _, w, cx| this.pick_icon(w, cx))),
                    )
                    .child(
                        div()
                            .id("agent-name")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.name)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("@{}", agent.slug)),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(section(theme, "Default repositories"))
                    .children(toggles),
            )
            .child(
                card(theme)
                    .child(row(
                        theme,
                        false,
                        "Default model",
                        "Used for new tasks and chats",
                        Select::new(&self.model),
                    ))
                    .child(row(
                        theme,
                        true,
                        "Thinking",
                        "Their thinking level",
                        Select::new(&self.effort),
                    )),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(section(theme, "Instructions"))
                    .child(
                        div()
                            .id("agent-instructions")
                            .test_support()
                            .child(Textarea::new(&self.instructions)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(skills_button(&self.slug))
                    .child(
                        Button::new("agent-new-task")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("New task")
                            .on_click(cx.listener(|this, _, w, cx| {
                                let slug = this.slug.clone();
                                this.shark
                                    .update(cx, |s, cx| s.prefill_agent_task(&slug, w, cx))
                                    .ok();
                            })),
                    )
                    .child(
                        Button::new("delete-agent")
                            .ghost()
                            .small()
                            .label("Delete agent…")
                            .on_click(cx.listener(|this, _, w, cx| {
                                let slug = this.slug.clone();
                                this.shark
                                    .update(cx, |s, cx| s.delete_agent(&slug, w, cx))
                                    .ok();
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn repository_model_controls_effort_without_overriding_an_explicit_agent_model() {
        let mut state = State {
            repos: vec![Repo {
                name: "api".into(),
                path: "/api".into(),
            }],
            last_model: Some("opus".into()),
            ..Default::default()
        };
        state.repo_settings.insert(
            "api".into(),
            crate::state::RepoSettings {
                model: Some("codex:gpt-6.1-sol".into()),
                ..Default::default()
            },
        );
        let mut agent = Agent {
            repos: vec!["api".into()],
            ..Default::default()
        };
        let inherited = repo_model(&state, &agent);
        assert_eq!(inherited, "codex:gpt-6.1-sol");
        assert!(efforts(&inherited).contains(&"ultra"));
        agent.model = Some("opus".into());
        assert!(!efforts(agent.model.as_deref().unwrap_or(&inherited)).contains(&"ultra"));
        agent.repos.clear();
        assert_eq!(repo_model(&state, &agent), "opus");
    }

    #[test]
    fn skills_folder_uses_the_agent_source_and_reports_creation_errors() {
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = crate::app::ui_tests::test_home();
        let path = create_skills_folder("designer").unwrap();
        assert_eq!(
            path,
            home.join("Library/Application Support/shark/agents/designer/skills")
        );
        assert!(path.is_dir());
        assert_eq!(create_skills_folder("designer").unwrap(), path);
        assert!(create_skills_folder("../outside").is_err());
        let blocked = crate::state::agent_skills_dir("blocked");
        std::fs::create_dir_all(blocked.parent().unwrap()).unwrap();
        std::fs::write(&blocked, "keep").unwrap();
        assert!(create_skills_folder("blocked").is_err());
        assert_eq!(std::fs::read_to_string(blocked).unwrap(), "keep");
    }
}
