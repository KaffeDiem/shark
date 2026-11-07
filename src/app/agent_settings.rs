//! Persistent agents. An agent's page is its settings: the header holds its identity,
//! instructions and defaults, edited in place; below sit its composer, tasks and history.
use super::menus::on_shark;
use super::*;
use crate::models::{effort_label, efforts, model_label};
use crate::state::Agent;
use gpui_kit::component::input::{Textarea, TextareaState};
use std::cell::Cell;
use std::time::Duration;

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

/// The skills an agent has: folders holding a `SKILL.md`. Read when its page opens and on
/// Refresh skills, never per frame.
fn skill_names(slug: &str) -> Vec<String> {
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
            let draft = self.home.agent_drafts.remove(slug).unwrap_or_default();
            let prompt = self.home.agent_prompt.clone();
            let left = prompt.read(cx).value().to_string();
            if let Some(prev) = self.home.agent_prompt_for.replace(slug.into())
                && !left.trim().is_empty()
            {
                self.home.agent_drafts.insert(prev, left);
            }
            prompt.update(cx, |p, cx| p.set_value(draft, window, cx));
        }
        self.home.agent_prompt.update(cx, |p, cx| {
            p.set_placeholder(format!("Describe a task for {}", agent.name), window, cx)
        });
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
    fn update_agent(&mut self, slug: &str, f: impl FnOnce(&mut Agent), cx: &mut Context<Self>) {
        let Some(i) = self.state.agents.iter().position(|a| a.slug == slug) else {
            return;
        };
        f(&mut self.state.agents[i]);
        let a = &self.state.agents[i];
        let model = a.model.clone().unwrap_or_else(|| repo_model(&self.state, a));
        let a = &mut self.state.agents[i];
        if a.effort.as_deref().is_some_and(|e| !efforts(&model).contains(&e)) {
            a.effort = None;
        }
        let _ = self.state.save();
        cx.notify();
    }

    fn refresh_agent_skills(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(header) = self.home.agent_header.clone()
            && header.read(cx).slug == slug
        {
            header.update(cx, |h, _| h.skills = skill_names(slug));
        }
        match self.sync_agent(slug, true, cx) {
            Ok(()) => window.push_notification(Notification::success("Agent skills refreshed"), cx),
            Err(e) => notify_err(window, cx, format!("Couldn't refresh agent skills: {e}")),
        }
        cx.notify();
    }

    /// What an agent offers, from its sidebar row's right-click and its header's `…`.
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
            menu::item(IconName::SquarePen, "New task", act(Shark::prefill_agent_task)),
            Entry::Sep,
            menu::item(IconName::Pencil, "Rename", act(Shark::rename_agent)),
            menu::item(IconName::Shapes, "Change icon…", act(Shark::pick_agent_icon)),
            menu::item(IconName::FileText, "Edit instructions", act(Shark::edit_agent)),
            Entry::Sep,
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
        entries.extend(open_in);
        entries.extend([
            Entry::Sep,
            menu::item(IconName::Trash, "Delete agent…", act(Shark::delete_agent)),
        ]);
        entries
    }

    fn agent_model_menu(
        &mut self,
        slug: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.agent(slug) else { return };
        let inherited = model_label(&repo_model(&self.state, agent));
        let current = agent.model.clone();
        let pick = |model: Option<&'static str>| {
            let s = slug.to_string();
            on_shark(cx, move |shark, _, cx| {
                shark.update_agent(&s, |a| a.model = model.map(Into::into), cx)
            })
        };
        let mut entries = vec![
            menu::item(None, "Repository default", pick(None))
                .hint(inherited)
                .checked(current.is_none()),
            Entry::Sep,
        ];
        entries.extend(MODELS.iter().map(|m| {
            menu::item(None, model_label(m), pick(Some(m))).checked(current.as_deref() == Some(*m))
        }));
        self.show_menu(pos, entries, window, cx);
    }

    fn agent_effort_menu(
        &mut self,
        slug: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.agent(slug) else { return };
        let model = (agent.model.clone()).unwrap_or_else(|| repo_model(&self.state, agent));
        let current = agent.effort.clone();
        let pick = |effort: Option<&'static str>| {
            let s = slug.to_string();
            on_shark(cx, move |shark, _, cx| {
                shark.update_agent(&s, |a| a.effort = effort.map(Into::into), cx)
            })
        };
        let mut entries = vec![
            menu::item(None, "Repository default", pick(None)).checked(current.is_none()),
            Entry::Sep,
        ];
        entries.extend(efforts(&model).iter().map(|l| {
            menu::item(None, effort_label(l), pick(Some(l))).checked(current.as_deref() == Some(*l))
        }));
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
            on_shark(cx, move |shark, w, cx| shark.refresh_agent_skills(&s, w, cx)),
        ));
        self.show_menu(pos, entries, window, cx);
    }

    /// The header's Attach: default repositories, instructions and skills.
    fn agent_attach_menu(
        &mut self,
        slug: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(agent) = self.agent(slug) else { return };
        let repos: Vec<Entry> = (self.state.repos.iter())
            .map(|r| {
                let on = agent.repos.contains(&r.name);
                let (s, repo) = (slug.to_string(), r.name.clone());
                menu::item(
                    IconName::FolderGit2,
                    r.name.clone(),
                    on_shark(cx, move |shark, _, cx| {
                        shark.update_agent(
                            &s,
                            |a| match on {
                                true => a.repos.retain(|x| x != &repo),
                                false => a.repos.push(repo.clone()),
                            },
                            cx,
                        )
                    }),
                )
                .checked(on)
            })
            .collect();
        let s = slug.to_string();
        let mut entries = vec![match repos.is_empty() {
            true => menu::item(
                IconName::FolderPlus,
                "Add repo…",
                on_shark(cx, |s, w, cx| s.add_repo(w, cx)),
            ),
            false => menu::sub(IconName::FolderGit2, "Repository", repos),
        }];
        entries.push(menu::item(
            IconName::FileText,
            "Instructions",
            on_shark(cx, move |shark, w, cx| shark.edit_agent(&s, w, cx)),
        ));
        let s = slug.to_string();
        entries.push(
            menu::item(IconName::BookOpen, "Skill…", move |w, cx| {
                reveal_skills(&s, w, cx)
            })
            .hint("Opens the skills folder"),
        );
        self.show_menu(pos, entries, window, cx);
    }

    fn agent_repo_menu(
        &mut self,
        slug: &str,
        repo: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (s, r) = (slug.to_string(), repo.to_string());
        let entries = vec![menu::item(
            IconName::X,
            "Remove from defaults",
            on_shark(cx, move |shark, _, cx| {
                shark.update_agent(&s, |a| a.repos.retain(|x| x != &r), cx)
            }),
        )];
        self.show_menu(pos, entries, window, cx);
    }

    /// Icon, name, @handle and `…`; instructions; then the defaults as chips and Attach.
    fn render_agent_header(&self, agent: &Agent, window: &Window, cx: &Context<Self>) -> AnyElement {
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
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    div()
                        .id("agent-name")
                        .role(Role::Button)
                        .aria_label(format!("Rename {}", agent.name))
                        .test_support()
                        .min_w_0()
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
                        .flex_none()
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
                    let entries = s.agent_menu(&slug, cx);
                    s.show_menu(pos, entries, w, cx)
                },
                cx,
            )
        };
        let identity = h_flex()
            .gap_2()
            .child(
                // Outdented by the ghost padding so the glyph lines up with the composer's edge.
                Button::new("agent-page-icon")
                    .accessibility_label("Change agent icon")
                    .ghost()
                    .ml(px(-6.))
                    .icon(Icon::new(super::sidebar::section_icon(&agent.icon)).with_size(px(20.)))
                    .tooltip("Change icon")
                    .on_click(cx.listener(move |s, _, w, cx| s.pick_agent_icon(&icon_slug, w, cx))),
            )
            .child(name)
            .child(more);
        let editing = h.instructions.read(cx).focus_handle(cx).is_focused(window);
        let instructions = v_flex()
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
                    .border_color(if editing { theme.border } else { theme.transparent })
                    .when(!editing, |d| d.hover(|d| d.bg(theme.list_hover)))
                    .text_size(px(14.))
                    .line_height(relative(1.5))
                    .text_color(theme.muted_foreground)
                    .child(Textarea::new(&h.instructions).appearance(false)),
            )
            .when(editing, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Every task of this agent reads these through CLAUDE.md and AGENTS.md."),
                )
            });
        let mut chips: Vec<AnyElement> = (agent.repos.iter())
            .filter(|r| self.state.repos.iter().any(|x| &x.name == *r))
            .map(|repo| {
                let (s, r) = (slug.clone(), repo.clone());
                let id = SharedString::from(format!("agent-repo-{repo}"));
                anchored_button(
                    id.clone(),
                    false,
                    Shark::chip(id, IconName::FolderGit2, repo.clone())
                        .accessibility_label(format!("Default repository {repo}")),
                    move |sh, pos, w, cx| sh.agent_repo_menu(&s, &r, pos, w, cx),
                    cx,
                )
            })
            .collect();
        let model = agent.model.as_deref().map(model_label);
        let effort = agent.effort.as_deref().map(effort_label);
        let skills = match h.skills.len() {
            0 => "No skills".to_string(),
            1 => "1 skill".to_string(),
            n => format!("{n} skills"),
        };
        let chip = |id: &'static str, icon: IconName, label: String, tip: &'static str, open: fn(&mut Shark, &str, Point<Pixels>, &mut Window, &mut Context<Shark>)| {
            let s = slug.clone();
            anchored_button(
                id.into(),
                false,
                Shark::chip(id, icon, label).tooltip(tip),
                move |sh, pos, w, cx| open(sh, &s, pos, w, cx),
                cx,
            )
        };
        chips.push(chip(
            "agent-model",
            IconName::Sparkles,
            model.unwrap_or_else(|| "Default model".into()),
            "Model for new tasks",
            Shark::agent_model_menu,
        ));
        chips.push(chip(
            "agent-effort",
            IconName::Brain,
            effort.unwrap_or_else(|| "Default thinking".into()),
            "Thinking level for new tasks",
            Shark::agent_effort_menu,
        ));
        chips.push(chip(
            "agent-skills",
            IconName::BookOpen,
            skills,
            "This agent's skills",
            Shark::agent_skills_menu,
        ));
        let s = slug.clone();
        chips.push(anchored_button(
            "agent-attach".into(),
            false,
            Button::new("agent-attach")
                .ghost()
                .icon(IconName::Paperclip)
                .label("Attach")
                .accessibility_label("Attach repository, instructions or skill"),
            move |sh, pos, w, cx| sh.agent_attach_menu(&s, pos, w, cx),
            cx,
        ));
        v_flex()
            .id("agent-settings")
            .test_support()
            .gap_2()
            .child(identity)
            .child(instructions)
            .child(h_flex().ml(px(-6.)).flex_wrap().gap_1().children(chips))
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
        let focused = (self.home.agent_prompt.read(cx))
            .focus_handle(cx)
            .is_focused(window);
        let linear = self.linear_in_use().then(|| {
            Button::new("agent-linear-issue")
                .ghost()
                .xsmall()
                .icon(IconName::TagPlus)
                .label("From Linear issue…")
                .on_click(cx.listener(|s, _, w, cx| s.start_from_issue(w, cx)))
        });
        let composer = v_flex()
            .gap_2()
            .child(
                div()
                    .id("agent-composer")
                    .test_support()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(if focused { theme.primary } else { theme.border })
                    .bg(theme.secondary)
                    .text_size(px(14.))
                    .child(Input::new(&self.home.agent_prompt).appearance(false)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(crate::ui::hint("enter", "start task"))
                    .child(div().flex_1())
                    .children(linear)
                    .child(
                        Button::new("agent-from-branch")
                            .ghost()
                            .xsmall()
                            .icon(IconName::GitBranch)
                            .label("From branch or PR…")
                            .on_click(cx.listener(|s, _, w, cx| s.new_task_default(w, cx))),
                    ),
            );
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
    fn new(shark: WeakEntity<Shark>, a: Agent, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            cx.subscribe_in(&this.instructions, window, |this, field, ev, w, cx| match ev {
                InputEvent::Change => {
                    let text = field.read(cx).value().to_string();
                    this.update(cx, |a| a.instructions = text);
                    this.sync_soon(w, cx);
                }
                // Shark's render reads this entity, so notifying here redraws the page.
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }),
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
        std::fs::create_dir_all(path.join("review")).unwrap();
        std::fs::write(path.join("review/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(path.join("draft")).unwrap();
        assert_eq!(skill_names("designer"), ["review"]);
    }
}
