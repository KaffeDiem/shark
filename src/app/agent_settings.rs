//! Persistent agents, chat presets. One dialog creates and edits them: identity, instructions,
//! skills and handoff.
use super::menus::on_shark;
use super::*;
use crate::models::efforts;
use crate::state::Agent;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::{Textarea, TextareaState};
use std::cell::{Cell, RefCell};
use std::time::Duration;

fn repo_defaults(state: &State, agent: &Agent) -> (String, Option<String>) {
    let task = Task {
        repos: agent
            .repos
            .iter()
            .filter_map(|name| state.repos.iter().find(|r| &r.name == name))
            .map(|r| crate::state::TaskRepo {
                name: r.name.clone(),
                src: r.path.clone(),
                base: String::new(),
                branch: None,
            })
            .collect(),
        ..Default::default()
    };
    state.defaults_for(&task, None)
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

/// The skills an agent has: folders holding a `SKILL.md`. Read when its page opens and on
/// Refresh skills, never per frame.
pub(super) fn skill_names(slug: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(crate::state::agent_skills_dir(slug)) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn reveal_skills(slug: &str, window: &mut Window, cx: &mut App) {
    match create_skills_folder(slug) {
        Ok(path) => cx.reveal_path(&path),
        Err(e) => notify_err(window, cx, e.to_string()),
    }
}

/// A control whose menu hangs from it, as dropdowns do.
fn menu_button(
    id: &'static str,
    button: Button,
    open: impl Fn(Point<Pixels>, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let at = Rc::new(Cell::<Bounds<Pixels>>::default());
    let click = at.clone();
    let button = button.on_click(move |_, w, cx| {
        let b = click.get();
        open(point(b.left(), b.bottom() + px(4.)), w, cx)
    });
    div()
        .relative()
        .flex_none()
        .child(crate::ui::quiet_toolbar(id, button))
        .child(
            canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .into_any_element()
}

/// What runs on a created agent's slug: the chat or Home composer that asked for it picks it.
pub(super) type AfterCreate = Rc<dyn Fn(&mut Shark, &str, &mut Window, &mut Context<Shark>)>;

impl Shark {
    pub(super) fn agent(&self, slug: &str) -> Option<&Agent> {
        self.state.agents.iter().find(|a| a.slug == slug)
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
            || self
                .state
                .tasks
                .iter()
                .flat_map(|task| task.chats.iter().chain(&task.closed))
                .any(|chat| chat.agent.as_deref() == Some(slug.as_str()))
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
        self.agent_dialog(None, None, window, cx);
    }

    pub(super) fn open_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent(slug).is_some() {
            self.agent_dialog(Some(slug), None, window, cx);
        }
    }

    /// The agent dialog: `slug`'s settings, or a new agent that `then` receives once created.
    pub(super) fn agent_dialog(
        &mut self,
        slug: Option<&str>,
        then: Option<AfterCreate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = slug.and_then(|s| self.agent(s)).cloned();
        let (shark, anchors) = (cx.entity().downgrade(), self.icon_anchors.clone());
        let editor = cx.new(|cx| AgentEditor::new(shark, agent, anchors, then, window, cx));
        self.agent_editor = Some(editor.downgrade());
        let shown = editor.clone();
        window.open_dialog(cx, move |d, _, cx| {
            let new = shown.read(cx).slug.is_none();
            let footer = shown.update(cx, |e, cx| e.footer(cx));
            // Enter belongs to the fields and footer: a refused Create must not close it.
            crate::ui::dialog(d)
                .title(if new { "New agent" } else { "Edit agent" })
                .w(px(560.))
                .on_ok(|_, _, _| false)
                .child(shown.clone())
                .footer(footer)
        });
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    /// Changes an agent and saves. A thinking level its model can't do falls back to the default.
    pub(super) fn update_agent(
        &mut self,
        slug: &str,
        f: impl FnOnce(&mut Agent),
        cx: &mut Context<Self>,
    ) {
        let Some(i) = self.state.agents.iter().position(|a| a.slug == slug) else {
            return;
        };
        let old_next = self.state.agents[i].next.clone();
        let old_identity = (
            self.state.agents[i].name.clone(),
            self.state.agents[i].icon.clone(),
        );
        let old_defaults = (
            self.state.agents[i].model.clone(),
            self.state.agents[i].effort.clone(),
        );
        let draft_defaults = (self.home.draft_agent.as_deref() == Some(slug))
            .then(|| self.home.draft.defaults(&self.state, Some(slug)));
        f(&mut self.state.agents[i]);
        let a = &self.state.agents[i];
        let model = a
            .model
            .clone()
            .unwrap_or_else(|| repo_defaults(&self.state, a).0);
        let a = &mut self.state.agents[i];
        if a.effort
            .as_deref()
            .is_some_and(|e| !efforts(&model).contains(&e))
        {
            a.effort = None;
        }
        let next_changed = old_next != a.next;
        let identity_changed = old_identity != (a.name.clone(), a.icon.clone());
        let changed_defaults = (old_defaults.0 != a.model, old_defaults.1 != a.effort);
        if (changed_defaults.0 || changed_defaults.1)
            && let Some(before) = draft_defaults
        {
            (self.home.draft).preserve_defaults(&self.state, slug, before, changed_defaults);
        }
        self.refresh_draft_defaults(slug, cx);
        let _ = self.state.save();
        if next_changed {
            let _ = self.sync_agent(slug, false, cx);
        }
        if identity_changed {
            self.publish_agent_identity(slug, cx);
        }
        cx.notify();
    }

    fn refresh_agent_skills(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.sync_agent_skills(slug, cx) {
            Ok(()) => window.push_notification(Notification::success("Agent skills refreshed"), cx),
            Err(e) => notify_err(window, cx, format!("Couldn't refresh agent skills: {e}")),
        }
    }

    pub(super) fn sync_agent_skills(
        &mut self,
        slug: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(editor) = self.agent_editor.as_ref().and_then(|e| e.upgrade())
            && editor.read(cx).slug.as_deref() == Some(slug)
        {
            editor.update(cx, |e, cx| {
                e.skills = skill_names(slug);
                cx.notify();
            });
        }
        if self.home.draft_agent.as_deref() == Some(slug) {
            self.refresh_draft_sources(cx);
        }
        cx.notify();
        self.sync_agent(slug, true, cx)
    }

    pub(super) fn agent_next_menu(
        &mut self,
        slug: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.agent(slug) else {
            return;
        };
        let current = agent.next.as_deref().filter(|s| self.agent(s).is_some());
        let pick = |next: Option<String>| {
            let slug = slug.to_string();
            on_shark(cx, move |s, _, cx| {
                s.update_agent(&slug, |a| a.next = next.clone(), cx)
            })
        };
        let mut entries = vec![
            menu::item(None, "None", pick(None)).checked(current.is_none()),
            Entry::Sep,
        ];
        entries.extend(
            self.state
                .agents
                .iter()
                .filter(|a| a.slug != slug)
                .map(|a| {
                    menu::item(None, a.name.clone(), pick(Some(a.slug.clone())))
                        .agent_icon(a.icon.clone())
                        .checked(current == Some(a.slug.as_str()))
                }),
        );
        self.show_menu(pos, entries, window, cx);
    }

    pub(super) fn agent_skills_menu(
        &mut self,
        slug: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let skills = skill_names(slug);
        let dir = crate::state::agent_skills_dir(slug);
        let mut entries: Vec<Entry> = skills
            .into_iter()
            .map(|name| {
                let path = dir.join(&name).join("SKILL.md");
                menu::item(IconName::BookOpen, name, move |_, cx| cx.reveal_path(&path))
            })
            .collect();
        if !entries.is_empty() {
            entries.push(Entry::Sep);
        }
        let s = slug.to_string();
        entries.push(menu::item(
            IconName::Plus,
            "Add skills…",
            on_shark(cx, move |shark, w, cx| {
                shark.add_agent_skills(&s, w, cx);
            }),
        ));
        let s = slug.to_string();
        entries.push(
            menu::item(IconName::FolderOpen, "Open skills folder", move |w, cx| {
                reveal_skills(&s, w, cx)
            })
            .hint("<skill>/SKILL.md"),
        );
        let s = slug.to_string();
        entries.push(menu::item(
            IconName::RefreshCw,
            "Refresh skills",
            on_shark(cx, move |shark, w, cx| {
                shark.refresh_agent_skills(&s, w, cx)
            }),
        ));
        self.show_menu(pos, entries, window, cx);
    }

    pub(super) fn pick_agent(
        &mut self,
        new_task: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.agents.is_empty() {
            let then: Option<AfterCreate> = new_task.then(|| {
                Rc::new(
                    |s: &mut Shark, slug: &str, w: &mut Window, cx: &mut Context<Shark>| {
                        s.prefill_agent_task(slug, w, cx)
                    },
                ) as AfterCreate
            });
            return self.agent_dialog(None, then, window, cx);
        }
        let agents = self.state.agents.clone();
        let items = agents
            .iter()
            .map(|a| super::agent_icon_view::command(a))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            if new_task {
                "New task for agent…"
            } else {
                "Edit agent…"
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
            .values()
            .any(|agent| agent.as_deref() == Some(slug))
        {
            notify_err(window, cx, "Wait for the agent's task to finish creating");
            return false;
        }
        true
    }

    pub(super) fn delete_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_delete_agent(slug, window, cx) {
            return;
        }
        let Some(agent) = self.agent(slug) else {
            return;
        };
        let slug = slug.to_string();
        confirm(
            format!("Delete {}?", agent.name),
            "Its instructions and skills are deleted. Chats that use it keep their transcripts and go on without them.",
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
                // Tasks whose chats use it, or an agent handing off to it.
                let affected: Vec<_> = s
                    .state
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.agents.iter().any(|a| {
                            a.slug == slug
                                || s.agent(&a.slug)
                                    .is_some_and(|a| a.next.as_deref() == Some(&slug))
                        })
                    })
                    .map(|task| task.name.clone())
                    .collect();
                s.state.agents.retain(|a| a.slug != slug);
                for agent in &mut s.state.agents {
                    if agent.next.as_deref() == Some(&slug) {
                        agent.next = None;
                    }
                }
                if let Err(e) = s.state.save() {
                    s.state.agents = agents;
                    return notify_err(window, cx, e.to_string());
                }
                if let Some(deleted) = agents.iter().find(|a| a.slug == slug)
                    && !s.state.agents.iter().any(|a| a.icon == deleted.icon)
                    && let Err(error) = crate::agent_icon::remove(&deleted.icon)
                {
                    notify_err(
                        window,
                        cx,
                        format!("Agent deleted, but couldn't remove its image: {error}"),
                    );
                }
                for task in affected {
                    if let Err(e) = s.relink_agents(&task, cx) {
                        notify_err(window, cx, e);
                    }
                    s.publish_task(&task, cx);
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
                // The agent's dialog under this confirmation has nothing left to show.
                window.defer(cx, |w, cx| w.close_all_dialogs(cx));
                cx.notify();
            },
            window,
            cx,
        );
    }
}

/// The agent dialog, for a new agent and an existing one. An existing agent's edits save at once
/// and Done only closes; a new agent exists once Create makes it.
pub(crate) struct AgentEditor {
    shark: WeakEntity<Shark>,
    /// `None` until Create makes the agent.
    pub(super) slug: Option<String>,
    pub(super) name: Entity<InputState>,
    pub(super) instructions: Entity<TextareaState>,
    pub(super) skills: Vec<String>,
    focus: FocusHandle,
    /// Shark's icon picker opens below the avatar it finds here.
    anchors: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    then: Option<AfterCreate>,
    /// Bumped per name or instructions edit; only the latest edit's timer syncs open tasks.
    edits: Rc<Cell<u64>>,
    _subs: Vec<Subscription>,
}

impl AgentEditor {
    fn new(
        shark: WeakEntity<Shark>,
        agent: Option<Agent>,
        anchors: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
        then: Option<AfterCreate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (name, instructions) = agent
            .as_ref()
            .map(|a| (a.name.clone(), a.instructions.clone()))
            .unwrap_or_default();
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Agent name")
                .default_value(name)
        });
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 14)
                .placeholder("Instructions every chat with this agent follows…")
                .default_value(instructions)
        });
        let slug = agent.map(|a| a.slug);
        let mut this = Self {
            shark,
            skills: slug.as_deref().map(skill_names).unwrap_or_default(),
            slug,
            name,
            instructions,
            focus: cx.focus_handle(),
            anchors,
            then,
            edits: Rc::default(),
            _subs: vec![],
        };
        this._subs.push(
            cx.subscribe_in(&this.name, window, |this, field, ev, w, cx| match ev {
                InputEvent::Change => {
                    let name = field.read(cx).value().trim().to_string();
                    // An empty name keeps the saved one.
                    if !name.is_empty() && this.slug.is_some() {
                        this.update(cx, |a| a.name = name);
                        this.sync_soon(w, cx);
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if this.slug.is_none() => this.create(w, cx),
                InputEvent::PressEnter { .. } => {
                    this.instructions.update(cx, |i, cx| i.focus(w, cx))
                }
                _ => {}
            }),
        );
        this._subs.push(
            cx.subscribe_in(&this.instructions, window, |this, field, ev, w, cx| {
                if let InputEvent::Change = ev
                    && this.slug.is_some()
                {
                    let text = field.read(cx).value().to_string();
                    this.update(cx, |a| a.instructions = text);
                    this.sync_soon(w, cx);
                }
            }),
        );
        this
    }

    /// A new agent starts on its name; an existing one on the dialog, where ⌘D pastes an icon.
    fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.slug {
            None => self.name.update(cx, |i, cx| i.focus(window, cx)),
            Some(_) => self.focus.focus(window, cx),
        }
    }

    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Agent)) {
        let Some(slug) = self.slug.clone() else {
            return;
        };
        self.shark
            .update(cx, |s, cx| s.update_agent(&slug, f, cx))
            .ok();
    }

    /// Chats and their tasks follow the agent once typing pauses. Detached, so closing the
    /// dialog mid-pause still syncs.
    fn sync_soon(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slug) = self.slug.clone() else {
            return;
        };
        let edit = self.edits.get() + 1;
        self.edits.set(edit);
        let (edits, shark) = (self.edits.clone(), self.shark.clone());
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            if edits.get() != edit {
                return;
            }
            shark
                .update_in(cx, |s, window, cx| {
                    // Root docs carry no instructions: the snapshot alone reaches the chats.
                    if let Err(e) = s.sync_agent(&slug, false, cx) {
                        let e = format!("Couldn't refresh agent instructions: {e}");
                        notify_err(window, cx, e);
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Create: the agent, with its instructions, then whatever asked for it.
    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().to_string();
        let instructions = self.instructions.read(cx).value().to_string();
        let Some(shark) = self.shark.upgrade() else {
            return;
        };
        let created = shark.update(cx, |s, cx| {
            let slug = s.create_named_agent(&name, cx)?;
            if !instructions.trim().is_empty() {
                s.update_agent(&slug, |a| a.instructions = instructions, cx);
            }
            Ok::<_, String>(slug)
        });
        match created {
            Ok(slug) => {
                window.close_dialog(cx);
                if let Some(then) = self.then.clone() {
                    shark.update(cx, |s, cx| then(s, &slug, window, cx));
                }
            }
            Err(e) => notify_err(window, cx, e),
        }
    }

    /// Create and Cancel for a new agent; Delete and Done for one that exists.
    fn footer(&mut self, cx: &mut Context<Self>) -> Div {
        let row = h_flex().w_full().gap_2();
        match self.slug.clone() {
            None => {
                let named = !self.name.read(cx).value().trim().is_empty();
                row.child(div().flex_1())
                    .child(
                        Button::new("agent-cancel")
                            .label("Cancel")
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("agent-create")
                            .primary()
                            .label("Create agent")
                            .disabled(!named)
                            .on_click(cx.listener(|this, _, w, cx| this.create(w, cx))),
                    )
            }
            Some(slug) => {
                let shark = self.shark.clone();
                // Red text only: `.danger()` would replace ghost with a filled pill beside Done.
                // Pulled left like the chips, so its text lines up with the fields.
                row.child(
                    Button::new("agent-delete")
                        .ghost()
                        .ml(px(-8.))
                        .text_color(cx.theme().button_danger_foreground)
                        .label("Delete agent…")
                        .on_click(move |_, w, cx| {
                            shark.update(cx, |s, cx| s.delete_agent(&slug, w, cx)).ok();
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("agent-done")
                        .primary()
                        .label("Done")
                        .on_click(|_, w, cx| w.close_dialog(cx)),
                )
            }
        }
    }
}

impl Render for AgentEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shark) = self.shark.upgrade() else {
            return div().into_any_element();
        };
        let s = shark.read(cx);
        let agent = self.slug.as_deref().and_then(|slug| s.agent(slug)).cloned();
        if self.slug.is_some() && agent.is_none() {
            return div().child("This agent was deleted.").into_any_element();
        }
        let next = (agent.as_ref())
            .and_then(|a| a.next.as_deref())
            .and_then(|n| s.agent(n))
            .map(|a| a.name.clone());
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let avatar = match &agent {
            None => super::agent_icon_view::agent_icon(&Default::default())
                .size(px(40.))
                .into_any_element(),
            Some(agent) => {
                let (slug, at, weak) =
                    (agent.slug.clone(), self.anchors.clone(), self.shark.clone());
                let key = slug.clone();
                div()
                    .relative()
                    .flex_none()
                    .child(
                        Button::new("agent-icon")
                            .accessibility_label("Change agent icon")
                            .ghost()
                            .p_0()
                            .size(px(40.))
                            .child(super::agent_icon_view::agent_icon(&agent.icon).size(px(40.)))
                            .tooltip("Change icon · ⌘D pastes an image")
                            .on_click(move |_, w, cx| {
                                weak.update(cx, |s, cx| s.pick_agent_icon(&slug, w, cx))
                                    .ok();
                            }),
                    )
                    .child(
                        canvas(
                            move |b, _, _| {
                                at.borrow_mut().insert(key.clone(), b);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .into_any_element()
            }
        };
        let identity = h_flex().gap_3().child(avatar).child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .id("agent-name")
                        .test_support()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(Input::new(&self.name).appearance(false).text_lg().px_0()),
                )
                .when_some(agent.as_ref(), |d, a| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(format!("@{}", a.slug)),
                    )
                }),
        );
        let instructions = v_flex()
            .gap_1()
            .child(
                div()
                    .id("agent-instructions")
                    .test_support()
                    .text_size(px(14.))
                    .line_height(relative(1.5))
                    .child(Textarea::new(&self.instructions)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Chats with this agent get these in their system prompt."),
            );
        let chips = agent.map(|agent| {
            let chip =
                |id: &'static str, label: String, icon: Option<IconName>, tip: &'static str| {
                    Button::new(id)
                        .ghost()
                        .accessibility_label(label.clone())
                        .tooltip(tip)
                        .child(
                            h_flex()
                                .gap_1()
                                .children(icon.map(|i| Icon::new(i).with_size(px(14.))))
                                .child(div().max_w(px(240.)).truncate().child(label))
                                .child(Icon::new(IconName::ChevronDown).with_size(px(12.))),
                        )
                };
            let count = self.skills.len();
            let skills = format!("{count} {}", if count == 1 { "skill" } else { "skills" });
            let next = format!("Hands off to {}", next.as_deref().unwrap_or("nobody"));
            let (weak, slug) = (self.shark.clone(), agent.slug.clone());
            let open_skills = move |pos, w: &mut Window, cx: &mut App| {
                weak.update(cx, |s, cx| s.agent_skills_menu(&slug, pos, w, cx))
                    .ok();
            };
            let (weak, slug) = (self.shark.clone(), agent.slug.clone());
            let open_next = move |pos, w: &mut Window, cx: &mut App| {
                weak.update(cx, |s, cx| s.agent_next_menu(&slug, pos, w, cx))
                    .ok();
            };
            h_flex()
                .flex_wrap()
                .gap_1()
                .ml(px(-8.))
                .child(menu_button(
                    "agent-skills",
                    chip(
                        "agent-skills",
                        skills,
                        Some(IconName::BookOpen),
                        "This agent's skills",
                    ),
                    open_skills,
                ))
                .child(menu_button(
                    "agent-next",
                    chip("agent-next", next, None, "Suggest handing finished work to"),
                    open_next,
                ))
        });
        let (weak, slug) = (self.shark.clone(), self.slug.clone());
        v_flex()
            .id("agent-editor")
            .key_context("AgentEditor")
            .track_focus(&self.focus)
            .test_support()
            .on_action(move |_: &PasteAgentIcon, w, cx| {
                if let Some(slug) = &slug {
                    weak.update(cx, |s, cx| s.paste_agent_icon(slug, w, cx))
                        .ok();
                }
            })
            .gap_4()
            .child(identity)
            .child(instructions)
            .children(chips)
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
                effort: Some("low".into()),
                ..Default::default()
            },
        );
        let mut agent = Agent {
            repos: vec!["api".into()],
            ..Default::default()
        };
        let inherited = repo_defaults(&state, &agent).0;
        assert_eq!(inherited, "codex:gpt-6.1-sol");
        assert_eq!(repo_defaults(&state, &agent).1.as_deref(), Some("low"));
        assert!(efforts(&inherited).contains(&"ultra"));
        agent.model = Some("opus".into());
        assert!(!efforts(agent.model.as_deref().unwrap_or(&inherited)).contains(&"ultra"));
        agent.repos.clear();
        assert_eq!(repo_defaults(&state, &agent).0, "opus");
        state.repos.push(Repo {
            name: "web".into(),
            path: "/web".into(),
        });
        state.repo_settings.insert(
            "web".into(),
            crate::state::RepoSettings {
                model: Some("sonnet".into()),
                ..Default::default()
            },
        );
        agent.repos = vec!["web".into(), "api".into()];
        assert_eq!(
            repo_defaults(&state, &agent).0,
            "sonnet",
            "draft repository order determines inheritance"
        );
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
        std::fs::create_dir_all(path.join("review")).unwrap();
        std::fs::write(path.join("review/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(path.join("draft")).unwrap();
        assert_eq!(skill_names("designer"), ["review"]);
    }
}
