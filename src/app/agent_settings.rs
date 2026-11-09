//! Persistent agents. An agent's page is its settings: the header holds its identity,
//! instructions, skills and handoff, edited in place; below sit its composer, tasks and history.
use super::menus::on_shark;
use super::*;
use crate::models::efforts;
use crate::state::Agent;
use gpui_kit::component::input::{Textarea, TextareaState};
use std::cell::Cell;
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
    state.defaults_for(&task)
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

/// An empty list: what's missing and how it fills, in the shape of Home's empty list.
pub(super) fn empty_state(
    theme: &Theme,
    id: &'static str,
    icon: IconName,
    title: &'static str,
    hint: &'static str,
) -> AnyElement {
    v_flex()
        .id(id)
        .test_support()
        .py_10()
        .items_center()
        .gap_2()
        .child(Icon::new(icon).with_size(px(32.)).text_color(faint(theme)))
        .child(div().text_base().child(title))
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint),
        )
        .into_any_element()
}

/// Deleted tasks, newest first, under a heading like the task list's groups.
fn render_history(agent: &Agent, theme: &Theme) -> Option<AnyElement> {
    if agent.history.is_empty() {
        return None;
    }
    let heading = h_flex()
        .pb_1()
        .gap_2()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.muted_foreground)
        .child("History")
        .child(
            div()
                .font_weight(FontWeight::NORMAL)
                .child(agent.history.len().to_string()),
        );
    let rows = agent.history.iter().enumerate().map(|(i, record)| {
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
        let (icon, color) = match record.merged {
            Some(true) => (IconName::GitMerge, theme.muted_foreground),
            _ => (IconName::Archive, theme.muted_foreground),
        };
        h_flex()
            .id(("agent-history", i))
            .aria_label(format!("{}, {status}, {date}", record.title))
            .test_support()
            .h(px(36.))
            .px_3()
            .gap_3()
            .rounded(theme.radius)
            .text_sm()
            .child(Icon::new(icon).with_size(px(14.)).text_color(color))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(record.title.clone()),
            )
            .child(div().flex_1())
            .children(record.prs.iter().enumerate().map(|(j, (repo, url))| {
                let url = url.clone();
                Button::new(SharedString::from(format!("agent-history-pr-{i}-{j}")))
                    .ghost()
                    .xsmall()
                    .role(Role::Link)
                    .label(format!("{repo} PR"))
                    .tooltip(url.clone())
                    .on_click(move |_, _, cx| cx.open_url(&url))
            }))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{status} · {date}")),
            )
    });
    Some(
        v_flex()
            .id("agent-history")
            .test_support()
            .child(heading)
            .children(rows)
            .into_any_element(),
    )
}

/// A header control whose menu hangs from it, as dropdowns do.
fn anchored_button(
    id: SharedString,
    quiet: bool,
    button: Button,
    open: impl Fn(&mut Shark, Point<Pixels>, &mut Window, &mut Context<Shark>) + 'static,
    cx: &Context<Shark>,
) -> AnyElement {
    let at = Rc::new(Cell::<Bounds<Pixels>>::default());
    let click = at.clone();
    let button = button.on_click(cx.listener(move |s, _, w, cx| {
        let b = click.get();
        open(s, point(b.left(), b.bottom() + px(4.)), w, cx)
    }));
    div()
        .relative()
        .flex_none()
        .map(|d| match quiet {
            true => d.child(crate::ui::quiet_toolbar(id, button)),
            false => d.child(crate::ui::toolbar(id, button)),
        })
        .child(
            canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
        .into_any_element()
}

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
                                shark.update(cx, |s, cx| s.edit_agent(&slug, w, cx)).ok();
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
        let Some(agent) = self.agent(slug).cloned() else {
            return;
        };
        window.close_all_dialogs(cx);
        self.enter_home(window, cx);
        self.home.destination = super::home::Destination::Agent(slug.to_string());
        if self.home.agent_prompt_for.as_deref() != Some(slug) {
            let draft = self.home.agent_drafts.remove(slug).unwrap_or_else(|| {
                super::task_draft::TaskDraft::for_agent(&self.state, Some(slug))
            });
            let prompt = self.home.agent_prompt.clone();
            self.home.agent_draft.text = prompt.read(cx).value().to_string();
            self.home.agent_draft.content = Some(prompt.read(cx).content());
            let left = std::mem::replace(&mut self.home.agent_draft, draft);
            if let Some(prev) = self.home.agent_prompt_for.replace(slug.into()) {
                self.home.agent_drafts.insert(prev, left);
            }
            let content = self
                .home
                .agent_draft
                .content
                .clone()
                .unwrap_or_else(|| self.home.agent_draft.text.clone().into());
            prompt.update(cx, |p, cx| p.set_value(content, window, cx));
        }
        let starting = self.home.agent_draft.starting;
        self.home
            .agent_composer
            .update(cx, |c, cx| c.set_read_only(starting, cx));
        self.refresh_draft_sources(true, cx);
        let placeholder = self.home.agent_draft.issues.first().map_or_else(
            || format!("Describe a task for {}", agent.name),
            |issue| format!("Add notes, or press Enter to plan {}", issue.identifier),
        );
        self.home
            .agent_prompt
            .update(cx, |p, cx| p.set_placeholder(placeholder, window, cx));
        let current = |e: &Entity<AgentHeader>| e.read(cx).slug == slug;
        if !self.home.agent_header.as_ref().is_some_and(current) {
            let shark = cx.entity().downgrade();
            self.home.agent_header = Some(cx.new(|cx| AgentHeader::new(shark, agent, window, cx)));
        }
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

    /// The agent's page, typing into its instructions.
    pub(super) fn edit_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent(slug, window, cx);
        if let Some(header) = self.home.agent_header.clone() {
            header.update(cx, |h, cx| {
                h.instructions.update(cx, |i, cx| i.focus(window, cx))
            });
        }
    }

    /// The agent's page, its name selected for typing.
    pub(super) fn rename_agent(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent(slug, window, cx);
        let name = self.agent(slug).map(|a| a.name.clone()).unwrap_or_default();
        if let Some(header) = self.home.agent_header.clone() {
            header.update(cx, |h, cx| h.start_rename(name, window, cx));
        }
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
        let draft_defaults = [
            (self.home.agent_prompt_for.as_deref() == Some(slug))
                .then(|| self.home.agent_draft.defaults(&self.state, Some(slug))),
            (self.home.draft_agent.as_deref() == Some(slug))
                .then(|| self.home.draft.defaults(&self.state, Some(slug))),
            self.home
                .agent_drafts
                .get(slug)
                .map(|d| d.defaults(&self.state, Some(slug))),
        ];
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
        if changed_defaults.0 || changed_defaults.1 {
            let drafts = [
                Some(&mut self.home.agent_draft),
                Some(&mut self.home.draft),
                self.home.agent_drafts.get_mut(slug),
            ];
            for (draft, before) in drafts.into_iter().zip(draft_defaults) {
                if let (Some(draft), Some(before)) = (draft, before) {
                    draft.preserve_defaults(&self.state, slug, before, changed_defaults);
                }
            }
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
        if let Some(header) = self.home.agent_header.clone()
            && header.read(cx).slug == slug
        {
            header.update(cx, |h, _| h.skills = skill_names(slug));
        }
        if self.home.agent_prompt_for.as_deref() == Some(slug) {
            self.refresh_draft_sources(true, cx);
        }
        if self.home.draft_agent.as_deref() == Some(slug) {
            self.refresh_draft_sources(false, cx);
        }
        cx.notify();
        self.sync_agent(slug, true, cx)
    }

    /// Identity controls used by the header and the sidebar's richer agent menu.
    pub(super) fn agent_identity_menu(&self, slug: &str, cx: &Context<Self>) -> Vec<Entry> {
        let s = slug.to_string();
        let act = |f: fn(&mut Shark, &str, &mut Window, &mut Context<Shark>)| {
            let s = s.clone();
            on_shark(cx, move |shark, w, cx| f(shark, &s, w, cx))
        };
        vec![
            menu::item(IconName::Pencil, "Rename", act(Shark::rename_agent)),
            menu::item(
                IconName::Shapes,
                "Change icon…",
                act(Shark::pick_agent_icon),
            ),
            menu::item(
                IconName::Image,
                "Paste Image as Icon",
                act(Shark::paste_agent_icon),
            )
            .hint("⌘D"),
            menu::item(
                IconName::FileText,
                "Edit instructions",
                act(Shark::edit_agent),
            ),
            Entry::Sep,
            menu::item(IconName::Trash, "Delete agent…", act(Shark::delete_agent)),
        ]
    }

    /// Task, skills and identity actions offered by the sidebar row's right-click.
    pub(super) fn agent_menu(&self, slug: &str, cx: &Context<Self>) -> Vec<Entry> {
        let s = slug.to_string();
        let act = |f: fn(&mut Shark, &str, &mut Window, &mut Context<Shark>)| {
            let s = s.clone();
            on_shark(cx, move |shark, w, cx| f(shark, &s, w, cx))
        };
        // The folder appears with Open skills folder; until then there's nothing to open.
        let folder = (crate::state::agent_skills_dir(slug).parent())
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf);
        let open_in = folder.and_then(|folder| {
            super::menus::open_in(Some(&("Agent folder".to_string(), folder)), &[], cx)
        });
        let mut entries = vec![
            menu::item(
                IconName::SquarePen,
                "New task",
                act(Shark::prefill_agent_task),
            ),
            Entry::Sep,
            Entry::Sep,
            menu::item(IconName::BookOpen, "Add skills…", {
                let s = s.clone();
                on_shark(cx, move |shark, w, cx| {
                    shark.add_agent_skills(&s, w, cx);
                })
            }),
            menu::item(IconName::FolderOpen, "Open skills folder", {
                let s = s.clone();
                move |w, cx| reveal_skills(&s, w, cx)
            }),
            menu::item(
                IconName::RefreshCw,
                "Refresh skills",
                act(Shark::refresh_agent_skills),
            ),
        ];
        let mut identity = self.agent_identity_menu(slug, cx);
        identity.truncate(identity.len() - 2);
        entries.splice(2..2, identity);
        entries.extend(open_in);
        entries.extend([
            Entry::Sep,
            menu::item(IconName::Trash, "Delete agent…", act(Shark::delete_agent)),
        ]);
        entries
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

    fn agent_skills_menu(
        &mut self,
        slug: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let skills = (self.home.agent_header.as_ref())
            .map(|h| h.read(cx).skills.clone())
            .unwrap_or_default();
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

    /// Identity and instructions, followed by skills and handoff.
    fn render_agent_header(
        &self,
        agent: &Agent,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(header) = self.home.agent_header.as_ref() else {
            return div().into_any_element();
        };
        let h = header.read(cx);
        let theme = cx.theme();
        let slug = agent.slug.clone();
        let icon_slug = slug.clone();
        let name = if h.renaming {
            div()
                .id("agent-name-input")
                .test_support()
                .flex_1()
                .min_w_0()
                .ml(px(-4.))
                .rounded(theme.radius)
                .bg(theme.list_hover)
                .font_weight(FontWeight::SEMIBOLD)
                .child(
                    Input::new(&h.name)
                        .appearance(false)
                        .text_xl()
                        .px_1()
                        .py_0(),
                )
                .into_any_element()
        } else {
            let (header, current) = (header.clone(), agent.name.clone());
            v_flex()
                .flex_1()
                .min_w_0()
                .items_start()
                .child(
                    div()
                        .id("agent-name")
                        .role(Role::Button)
                        .aria_label(format!("Rename {}", agent.name))
                        .test_support()
                        .min_w_0()
                        .max_w_full()
                        .px_1()
                        .ml(px(-4.))
                        .rounded(theme.radius)
                        .cursor_text()
                        .hover(|d| d.bg(theme.list_hover))
                        .text_xl()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(agent.name.clone())
                        .on_click(move |_, w, cx| {
                            header.update(cx, |h, cx| h.start_rename(current.clone(), w, cx))
                        }),
                )
                .child(
                    div()
                        .max_w_full()
                        .truncate()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("@{}", agent.slug)),
                )
                .into_any_element()
        };
        let more = {
            let slug = slug.clone();
            anchored_button(
                "agent-more".into(),
                true,
                Button::new("agent-more")
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .accessibility_label(format!("{} actions", agent.name))
                    .tooltip("Agent actions"),
                move |s, pos, w, cx| {
                    let entries = s.agent_identity_menu(&slug, cx);
                    s.show_menu(pos, entries, w, cx)
                },
                cx,
            )
        };
        let identity = h_flex()
            .gap_2()
            .child(
                Button::new("agent-page-icon")
                    .accessibility_label("Change agent icon")
                    .ghost()
                    .p_0()
                    .size(px(40.))
                    .child(super::agent_icon_view::agent_icon(&agent.icon).size(px(40.)))
                    .tooltip("Change icon")
                    .on_click(cx.listener(move |s, _, w, cx| s.pick_agent_icon(&icon_slug, w, cx))),
            )
            .child(name)
            .child(more);
        let editing = h.instructions.read(cx).focus_handle(cx).is_focused(window);
        let instructions =
            v_flex()
                .gap_1()
                .child(
                    div()
                        .id("agent-instructions")
                        .test_support()
                        .px_2()
                        .py_1()
                        .mx(px(-8.))
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(if editing {
                            theme.border
                        } else {
                            theme.transparent
                        })
                        .when(!editing, |d| d.hover(|d| d.bg(theme.list_hover)))
                        .text_size(px(14.))
                        .line_height(relative(1.5))
                        .text_color(theme.muted_foreground)
                        .child(Textarea::new(&h.instructions).appearance(false)),
                )
                .when(editing, |d| {
                    d.child(div().text_xs().text_color(theme.muted_foreground).child(
                        "Every task of this agent reads these through CLAUDE.md and AGENTS.md.",
                    ))
                });
        let chip = |id: &'static str, value: String, icon: Option<IconName>, tip: &'static str, open: fn(&mut Shark, &str, Point<Pixels>, &mut Window, &mut Context<Shark>)| {
            let s = slug.clone();
            anchored_button(
                id.into(),
                true,
                Button::new(id)
                    .ghost()
                    .accessibility_label(value.clone())
                    .tooltip(tip)
                    .child(h_flex().gap_1()
                        .children(icon.map(|icon| Icon::new(icon).with_size(px(14.))))
                        .child(div().max_w(px(240.)).truncate().child(value))
                        .child(Icon::new(IconName::ChevronDown).with_size(px(12.)))),
                move |sh, pos, w, cx| open(sh, &s, pos, w, cx),
                cx,
            )
        };
        let chips = vec![
            chip(
                "agent-skills",
                format!(
                    "{} {}",
                    h.skills.len(),
                    if h.skills.len() == 1 {
                        "skill"
                    } else {
                        "skills"
                    }
                ),
                Some(IconName::BookOpen),
                "This agent's skills",
                Shark::agent_skills_menu,
            ),
            chip(
                "agent-next",
                format!(
                    "Hands off to {}",
                    agent
                        .next
                        .as_deref()
                        .and_then(|s| self.agent(s))
                        .map(|a| a.name.as_str())
                        .unwrap_or("nobody")
                ),
                None,
                "Hand off finished work to",
                Shark::agent_next_menu,
            ),
        ];
        v_flex()
            .id("agent-settings")
            .test_support()
            .gap_2()
            .child(identity)
            .child(instructions)
            .child(h_flex().flex_wrap().gap_1().children(chips))
            .into_any_element()
    }

    pub(super) fn render_agent_page(
        &self,
        slug: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(agent) = self.agent(slug).cloned() else {
            return div().child("Agent no longer exists").into_any_element();
        };
        let header = self.render_agent_header(&agent, window, cx);
        let list = self.render_ws_list(window, cx);
        let theme = cx.theme();
        let history = render_history(&agent, theme);
        let composer_slug = slug.to_string();
        let composer = self.render_task_composer(true, window, cx);
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
                            .max_w(px(760.))
                            .mx_auto()
                            .px_6()
                            .py_8()
                            .gap_6()
                            .child(header)
                            .child(composer)
                            .child(list)
                            .children(history),
                    ),
            )
            .into_any_element()
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
            .map(|a| super::agent_icon_view::command(a))
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
            .values()
            .any(|agent| agent.as_deref() == Some(slug))
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
        let name = self.agent(slug).map_or(slug, |a| &a.name);
        notify_err(
            window,
            cx,
            format!("Finish or delete {name}'s {count} tasks first"),
        );
        false
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
                let affected: Vec<_> = s
                    .state
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.agent
                            .as_ref()
                            .and_then(|a| s.agent(&a.slug))
                            .is_some_and(|a| a.next.as_deref() == Some(&slug))
                    })
                    .map(|task| task.name.clone())
                    .collect();
                s.state.agents.retain(|a| a.slug != slug);
                for agent in &mut s.state.agents {
                    if agent.next.as_deref() == Some(&slug) {
                        agent.next = None;
                    }
                }
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
                    s.publish_task(&task, cx);
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
                s.collapsed_agents.remove(&slug);
                if s.home.destination == super::home::Destination::Agent(slug.clone()) {
                    s.enter_home(window, cx);
                    s.home.focus.focus(window, cx);
                }
                cx.notify();
            },
            window,
            cx,
        );
    }
}

/// The agent page header's fields. Every change saves at once; nothing waits for a Save.
pub(super) struct AgentHeader {
    shark: WeakEntity<Shark>,
    pub(super) slug: String,
    pub(super) name: Entity<InputState>,
    pub(super) instructions: Entity<TextareaState>,
    /// The name shows as an input while it's being edited.
    pub(super) renaming: bool,
    pub(super) skills: Vec<String>,
    /// Bumped per name or instructions edit; only the latest edit's timer syncs open tasks.
    edits: Rc<Cell<u64>>,
    _subs: Vec<Subscription>,
}

impl AgentHeader {
    fn new(
        shark: WeakEntity<Shark>,
        a: Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).default_value(a.name.clone()));
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 14)
                .placeholder("Add instructions every task of this agent follows…")
                .default_value(a.instructions.clone())
        });
        let mut this = Self {
            shark,
            skills: skill_names(&a.slug),
            slug: a.slug,
            name,
            instructions,
            renaming: false,
            edits: Rc::default(),
            _subs: vec![],
        };
        this._subs.push(
            cx.subscribe_in(&this.name, window, |this, field, ev, w, cx| match ev {
                InputEvent::Change => {
                    let name = field.read(cx).value().trim().to_string();
                    if !name.is_empty() {
                        this.update(cx, |a| a.name = name);
                        this.sync_soon(w, cx);
                    }
                }
                // Enter hands the keyboard back to the list; a blur leaves it where it went.
                InputEvent::PressEnter { .. } => this.finish_rename(true, w, cx),
                InputEvent::Blur => this.finish_rename(false, w, cx),
                _ => {}
            }),
        );
        this._subs.push(
            cx.subscribe_in(
                &this.instructions,
                window,
                |this, field, ev, w, cx| match ev {
                    InputEvent::Change => {
                        let text = field.read(cx).value().to_string();
                        this.update(cx, |a| a.instructions = text);
                        this.sync_soon(w, cx);
                    }
                    // Shark's render reads this entity, so notifying here redraws the page.
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    _ => {}
                },
            ),
        );
        this
    }

    /// `current` comes from the caller: Shark may be mid-update and unreadable here.
    fn start_rename(&mut self, current: String, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming = true;
        self.name.update(cx, |i, cx| {
            i.set_value(current, window, cx);
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        cx.notify();
    }

    fn finish_rename(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.renaming {
            return;
        }
        self.renaming = false;
        let saved = (self.shark.upgrade())
            .and_then(|s| s.read(cx).agent(&self.slug).map(|a| a.name.clone()))
            .unwrap_or_default();
        self.name.update(cx, |i, cx| i.set_value(saved, window, cx));
        if refocus {
            self.shark
                .update(cx, |s, cx| s.home.focus.focus(window, cx))
                .ok();
        }
        cx.notify();
    }

    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Agent)) {
        let slug = self.slug.clone();
        self.shark
            .update(cx, |s, cx| s.update_agent(&slug, f, cx))
            .ok();
    }

    /// Open tasks' CLAUDE.md follows the agent once typing pauses. Detached, so leaving the
    /// page mid-pause still syncs.
    fn sync_soon(&self, window: &mut Window, cx: &mut Context<Self>) {
        let edit = self.edits.get() + 1;
        self.edits.set(edit);
        let (edits, shark, slug) = (self.edits.clone(), self.shark.clone(), self.slug.clone());
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            if edits.get() != edit {
                return;
            }
            shark
                .update_in(cx, |s, window, cx| {
                    if let Err(e) = s.sync_agent(&slug, true, cx) {
                        let e = format!("Couldn't refresh agent instructions: {e}");
                        notify_err(window, cx, e);
                    }
                })
                .ok();
        })
        .detach();
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
