//! Persistent agents: named instructions and defaults for independently created tasks.
use super::*;
use crate::models::{effort_label, efforts, model_label};
use crate::state::Agent;
use crate::ui::{card, row, section};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};

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

pub(crate) struct AgentsPage {
    shark: WeakEntity<Shark>,
}

impl AgentsPage {
    pub(crate) fn new(shark: WeakEntity<Shark>) -> Self {
        Self { shark }
    }
}

impl Render for AgentsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let agents = self
            .shark
            .upgrade()
            .map(|s| s.read(cx).state.agents.clone())
            .unwrap_or_default();
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(
                card(theme).children(agents.into_iter().enumerate().map(|(i, a)| {
                    h_flex()
                        .id(("agent", i))
                        .test_support()
                        .h(px(48.))
                        .px_3()
                        .gap_3()
                        .when(i > 0, |d| d.border_t_1().border_color(theme.border))
                        .hover(|d| d.bg(theme.secondary_hover))
                        .child(Icon::new(super::sidebar::section_icon(&a.icon)).with_size(px(16.)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(a.name))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("@{}", a.slug)),
                                ),
                        )
                        .child(Icon::new(IconName::ChevronRight).with_size(px(14.)))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.shark
                                .update(cx, |s, cx| s.open_agent(&a.slug, w, cx))
                                .ok();
                        }))
                })),
            )
            .child(
                Button::new("new-agent")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .label("New agent…")
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.shark
                            .update(cx, |s, cx| s.new_agent_dialog(w, cx))
                            .ok();
                    })),
            )
    }
}

impl Shark {
    pub(super) fn ensure_agent_section(
        &mut self,
        slug: &str,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if let Some(i) = self
            .state
            .sections
            .iter()
            .position(|s| s.agent.as_deref() == Some(slug))
        {
            return Some(i);
        }
        let a = self.state.agents.iter().find(|a| a.slug == slug)?;
        if let Some(i) = self
            .state
            .sections
            .iter()
            .position(|s| s.agent.is_none() && s.name == a.name)
        {
            let section = &mut self.state.sections[i];
            section.agent = Some(slug.into());
            section.icon = a.icon.clone();
            return Some(i);
        }
        let mut name = a.name.clone();
        let mut suffix = 2;
        while self.state.sections.iter().any(|s| s.name == name) {
            name = format!("{} {suffix}", a.name);
            suffix += 1;
        }
        self.state.sections.push(Section {
            name,
            icon: a.icon.clone(),
            collapsed: false,
            tasks: vec![],
            agent: Some(slug.into()),
        });
        Some(self.state.sections.len() - 1)
    }

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
        let sections = self.state.sections.clone();
        self.state.agents.push(Agent {
            slug: slug.clone(),
            name: name.into(),
            icon: "folder".into(),
            ..Default::default()
        });
        self.ensure_agent_section(&slug, cx);
        if let Err(e) = self.state.save() {
            self.state.agents.pop();
            self.state.sections = sections;
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
                                shark.update(cx, |s, cx| s.open_agent(&slug, w, cx)).ok();
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
            return crate::settings::open_agents(&self.state, window, cx);
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
                let section = s
                    .state
                    .sections
                    .iter()
                    .position(|section| section.agent.as_deref() == Some(&self.slug));
                if let Some(i) = section {
                    let mut name = agent.name.clone();
                    let mut suffix = 2;
                    while s
                        .state
                        .sections
                        .iter()
                        .enumerate()
                        .any(|(j, section)| j != i && section.name == name)
                    {
                        name = format!("{} {suffix}", agent.name);
                        suffix += 1;
                    }
                    s.state.sections[i].name = name;
                    s.state.sections[i].icon = agent.icon.clone();
                }
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
                s.state
                    .tasks
                    .iter()
                    .filter(|task| task.agent.as_ref().is_some_and(|a| a.slug == self.slug))
                    .map(|task| (task.name.clone(), task.display_title().into_owned()))
                    .collect::<Vec<_>>(),
            )
        });
        let (agent, repos, tasks) = state.unwrap_or_default();
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
        let open_tasks = v_flex()
            .gap_2()
            .child(section(theme, "Open tasks"))
            .when(tasks.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No open tasks"),
                )
            })
            .children(tasks.into_iter().enumerate().map(|(i, (name, title))| {
                Button::new(("agent-open-task", i))
                    .ghost()
                    .small()
                    .label(title)
                    .on_click(cx.listener(move |this, _, w, cx| {
                        let shark = this.shark.clone();
                        w.close_all_dialogs(cx);
                        shark
                            .update(cx, |s, cx| s.open_task_named(&name, w, cx))
                            .ok();
                    }))
            }));
        let history = v_flex()
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
            }));
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
                    .child(
                        Button::new("agent-skills")
                            .ghost()
                            .small()
                            .icon(IconName::Folder)
                            .label("Open skills folder")
                            .on_click(cx.listener(|this, _, w, cx| {
                                match create_skills_folder(&this.slug) {
                                    Ok(path) => cx.reveal_path(&path),
                                    Err(e) => notify_err(w, cx, e.to_string()),
                                }
                            })),
                    )
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
            .child(open_tasks)
            .child(history)
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
