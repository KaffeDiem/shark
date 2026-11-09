//! Shared Home and agent-page draft controls. Pickers only change the draft.
use super::home::{Destination, agent_prompt};
use super::task_draft::{BranchSpec, DraftRepo, TaskDraft};
use super::*;
use crate::models::{effort_label, efforts, model_label};
use crate::task_composer::{Event as ComposerEvent, Source};
use gpui_kit::component::Disableable as _;

impl Shark {
    fn anchor_draft_control(&self, key: String, control: impl IntoElement) -> AnyElement {
        let anchors = self.draft_anchors.clone();
        div()
            .relative()
            .flex_none()
            .child(control)
            .child(
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
            .into_any_element()
    }

    fn draft_control_position(&self, key: &str, window: &Window) -> Point<Pixels> {
        self.draft_anchors
            .borrow()
            .get(key)
            .map(|b| point(b.left(), b.bottom() + px(4.)))
            .unwrap_or_else(|| window.mouse_position())
    }

    pub(super) fn draft(&self, agent: bool) -> &TaskDraft {
        if agent {
            &self.home.agent_draft
        } else {
            &self.home.draft
        }
    }

    pub(super) fn draft_mut(&mut self, agent: bool) -> &mut TaskDraft {
        if agent {
            &mut self.home.agent_draft
        } else {
            &mut self.home.draft
        }
    }

    pub(super) fn draft_slug(&self, agent: bool) -> Option<&str> {
        if agent {
            self.home.agent_prompt_for.as_deref()
        } else {
            self.home.draft_agent.as_deref()
        }
    }

    pub(super) fn draft_target(&self) -> bool {
        matches!(self.home.destination, Destination::Agent(_))
    }

    pub(super) fn refresh_draft_defaults(&mut self, slug: &str, cx: &mut Context<Self>) {
        let defaults = TaskDraft::for_agent(&self.state, Some(slug));
        let update = |draft: &mut TaskDraft| {
            if !draft.repos_edited {
                draft.repos = defaults
                    .repos
                    .iter()
                    .map(|repo| {
                        draft
                            .repos
                            .iter()
                            .find(|r| r.repo.name == repo.repo.name)
                            .unwrap_or(repo)
                            .clone()
                    })
                    .collect();
            }
        };
        if self.home.agent_prompt_for.as_deref() == Some(slug) {
            update(&mut self.home.agent_draft);
            self.refresh_draft_sources(true, cx);
        }
        if self.home.draft_agent.as_deref() == Some(slug) {
            update(&mut self.home.draft);
            self.refresh_draft_sources(false, cx);
        }
        if let Some(draft) = self.home.agent_drafts.get_mut(slug) {
            update(draft);
        }
    }

    pub(super) fn refresh_draft_sources(&mut self, agent: bool, cx: &mut Context<Self>) {
        self.sync_reference_completion(agent, cx);
        let unresolved: Vec<_> = self
            .draft(agent)
            .repos
            .iter()
            .filter_map(|r| {
                matches!(&r.branch, BranchSpec::New { base } if base.is_empty())
                    .then(|| r.repo.path.clone())
            })
            .collect();
        if !unresolved.is_empty() {
            cx.spawn(async move |this, cx| {
                let bases = cx
                    .background_executor()
                    .spawn(async move {
                        unresolved
                            .into_iter()
                            .map(|path| {
                                let base = git::resolve_base(&path, "");
                                (path, base)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                this.update(cx, |s, cx| {
                    s.draft_bases.extend(bases);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let draft = self.draft(agent);
        let codex =
            crate::codex::is_codex(Some(&draft.defaults(&self.state, self.draft_slug(agent)).0));
        let sources = draft
            .repos
            .iter()
            .map(|r| Source {
                name: r.repo.name.clone(),
                path: r.repo.path.clone(),
                pr: match &r.branch {
                    BranchSpec::Pr(pr) => Some(pr.clone()),
                    _ => None,
                },
                new_branch: matches!(r.branch, BranchSpec::New { .. }),
                start_ref: match &r.branch {
                    BranchSpec::New { base } => base.clone(),
                    BranchSpec::Existing(branch) => branch.clone(),
                    BranchSpec::Pr(pr) => format!("refs/pull/{}/head", pr.number),
                },
            })
            .collect();
        let skills = self
            .draft_slug(agent)
            .map(super::agent_settings::skill_names)
            .unwrap_or_default();
        let composer = if agent {
            self.home.agent_composer.clone()
        } else {
            self.home.composer.clone()
        };
        let home_agent = if agent {
            None
        } else {
            self.home.draft_agent.clone()
        };
        composer.update(cx, |c, cx| {
            c.set_home_agent(home_agent, cx);
            c.set_sources(sources, skills, !agent, codex, cx);
        });
    }

    pub(super) fn draft_composer_event(
        &mut self,
        agent: bool,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::AttachReference(reference) => {
                self.attach_draft_reference(agent, reference.clone(), window, cx)
            }
            ComposerEvent::SearchIssues(query) => self.search_reference_issues(query.clone(), cx),
            ComposerEvent::PasteReference { text } => {
                self.paste_draft_reference(agent, text.clone(), window, cx)
            }
            ComposerEvent::UndoReference { text } => {
                self.undo_draft_reference(agent, text.clone(), window, cx)
            }
            ComposerEvent::Submit => {
                if agent {
                    self.submit_task_draft(window, cx);
                } else if !self.mentions(cx).is_empty() {
                    self.complete_mention(None, window, cx);
                } else {
                    self.submit_prompt(window, cx);
                }
            }
            ComposerEvent::Change => {
                let input = if agent {
                    &self.home.agent_prompt
                } else {
                    &self.home.prompt
                };
                let content = input.read(cx).content();
                let text = content.text().to_string();
                if !agent {
                    let slug = agent_prompt(text.trim(), &self.state.agents)
                        .0
                        .map(str::to_string);
                    if self.home.draft_agent != slug {
                        if !self.home.draft.repos_edited {
                            self.home.draft.repos =
                                TaskDraft::for_agent(&self.state, slug.as_deref()).repos;
                        }
                        self.home.draft_agent = slug;
                        self.refresh_draft_sources(false, cx);
                    }
                    self.home.mention = 0;
                }
                self.draft_mut(agent).text = text;
                self.draft_mut(agent).content = Some(content);
                cx.notify();
            }
            ComposerEvent::Escape => {
                if !agent {
                    self.cancel_ask(cx);
                }
                self.home.focus.focus(window, cx);
                cx.notify();
            }
            ComposerEvent::ToggleAsk => {
                if !agent {
                    self.toggle_ask_mode(window, cx);
                }
            }
            ComposerEvent::TogglePlan => self.toggle_draft_plan(cx),
            ComposerEvent::ModelMenu => self.draft_model_menu(window, cx),
            ComposerEvent::AgentMove(by) => self.move_mention(*by, cx),
            ComposerEvent::Tray => {
                if agent || !self.home.asking {
                    self.home.tray_index = 0;
                    self.home.tray_focus.focus(window, cx);
                    cx.notify();
                }
            }
            ComposerEvent::Attach { paths, image } => {
                self.attach_draft_files(agent, paths.clone(), image.clone(), window, cx)
            }
        }
    }

    pub(super) fn focus_draft(&mut self, agent: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = if agent {
            self.home.agent_prompt.clone()
        } else {
            self.home.prompt.clone()
        };
        let placeholder = if !agent && self.home.asking {
            "Ask how to do something in Shark".into()
        } else if let Some(issue) = self.draft(agent).issues.first() {
            format!("Add notes, or press Enter to plan {}", issue.identifier)
        } else if let Some(a) = self.draft_slug(agent).and_then(|s| self.agent(s)) {
            format!("Describe a task for {}", a.name)
        } else {
            "Describe the work; enter starts a task".into()
        };
        prompt.update(cx, |p, cx| {
            p.set_placeholder(placeholder, window, cx);
            p.focus(window, cx);
        });
        self.refresh_draft_sources(agent, cx);
        cx.notify();
    }

    pub(super) fn draft_at_mut(&mut self, destination: &Destination) -> Option<&mut TaskDraft> {
        match destination {
            Destination::Home => Some(&mut self.home.draft),
            Destination::Agent(slug) if self.home.agent_prompt_for.as_ref() == Some(slug) => {
                Some(&mut self.home.agent_draft)
            }
            Destination::Agent(slug) => self.home.agent_drafts.get_mut(slug),
        }
    }

    pub(super) fn add_draft_issues_to(
        &mut self,
        destination: &Destination,
        issues: Vec<Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.draft_at_mut(destination) else {
            return;
        };
        if draft.starting {
            return;
        }
        for issue in issues {
            if !draft
                .issues
                .iter()
                .any(|i| i.id == issue.id && i.org == issue.org)
            {
                draft.issues.push(issue);
            }
        }
        if &self.home.destination == destination {
            self.focus_draft(self.draft_target(), window, cx);
        }
        cx.notify();
    }

    pub(super) fn add_draft_issues(
        &mut self,
        issues: Vec<Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_draft_issues_to(&self.home.destination.clone(), issues, window, cx);
    }

    pub(super) fn pick_draft_branch(
        &mut self,
        only: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.draft_target();
        if self.draft(agent).starting {
            return;
        }
        let current = self.draft(agent).repos.clone();
        let position = self.draft_control_position(
            &only
                .as_ref()
                .map(|(name, _)| format!("branch-{name}"))
                .unwrap_or_else(|| "draft-add".into()),
            window,
        );
        let destination = self.home.destination.clone();
        self.pick_draft_repos_at(
            current,
            only,
            Rc::new(move |s, repos, w, cx| {
                let Some(draft) = s.draft_at_mut(&destination) else {
                    return;
                };
                draft.repos = repos;
                draft.repos_edited = true;
                if s.home.destination == destination {
                    s.focus_draft(agent, w, cx);
                }
            }),
            position,
            window,
            cx,
        );
    }

    fn draft_repo_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        let Some(current) = self.draft(agent).repos.get(index) else {
            return;
        };
        let name = current.repo.name.clone();
        let mut entries: Vec<_> = self
            .state
            .repos
            .iter()
            .filter(|repo| {
                repo.name == name
                    || !self
                        .draft(agent)
                        .repos
                        .iter()
                        .any(|r| r.repo.name == repo.name)
            })
            .map(|repo| {
                let repo = repo.clone();
                let checked = repo.name == name;
                menu::item(
                    IconName::FolderGit2,
                    repo.name.clone(),
                    on_shark(cx, move |s, w, cx| {
                        if s.draft(agent).starting {
                            return;
                        }
                        s.draft_mut(agent).repos[index] = DraftRepo {
                            repo: repo.clone(),
                            pr_label: None,
                            branch: BranchSpec::New {
                                base: String::new(),
                            },
                        };
                        s.draft_mut(agent).repos_edited = true;
                        s.focus_draft(agent, w, cx);
                    }),
                )
                .checked(checked)
            })
            .collect();
        entries.push(Entry::Sep);
        entries.push(menu::item(
            IconName::X,
            "Remove repository",
            on_shark(cx, move |s, w, cx| {
                if s.draft(agent).starting {
                    return;
                }
                s.draft_mut(agent).repos.remove(index);
                s.draft_mut(agent).repos_edited = true;
                s.focus_draft(agent, w, cx);
            }),
        ));
        self.show_menu(window.mouse_position(), entries, window, cx);
    }

    pub(super) fn draft_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.home_shown()
            || self.draft(self.draft_target()).starting
            || (!self.draft_target() && self.home.asking)
        {
            return;
        }
        self.draft_add_picker(self.draft_control_position("draft-add", window), window, cx);
    }

    fn draft_repository_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.repos.is_empty() {
            crate::settings::open_repos(&self.state, window, cx);
        } else {
            self.draft_add_menu(window, cx);
        }
    }

    pub(super) fn change_draft_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.home_shown()
            || self.draft(self.draft_target()).starting
            || (!self.draft_target() && self.home.asking)
        {
            return;
        }
        let draft = self.draft(self.draft_target());
        let index = if self.home.tray_focus.is_focused(window) {
            self.home.tray_index.saturating_sub(draft.issues.len()) / 2
        } else {
            0
        };
        if let Some(repo) = draft.repos.get(index).or_else(|| draft.repos.first()) {
            self.pick_draft_branch(Some((repo.repo.name.clone(), false)), window, cx);
        } else {
            self.draft_repository_menu(window, cx);
        }
    }

    pub(super) fn reset_draft_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        if self.draft(agent).starting || self.draft_slug(agent).is_none() {
            return;
        }
        let mut defaults = TaskDraft::for_agent(&self.state, self.draft_slug(agent));
        let draft = self.draft_mut(agent);
        for repo in &mut defaults.repos {
            if let Some(current) = draft.repos.iter().find(|r| r.repo.name == repo.repo.name) {
                repo.branch = current.branch.clone();
                repo.pr_label = current.pr_label.clone();
            }
        }
        draft.repos = defaults.repos;
        draft.repos_edited = false;
        draft.model = None;
        draft.effort = None;
        draft.follow_app = false;
        self.focus_draft(agent, window, cx);
    }

    pub(super) fn save_draft_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        if self.draft(agent).starting {
            return;
        }
        let Some(slug) = self.draft_slug(agent).map(str::to_owned) else {
            return;
        };
        let Some(index) = self.state.agents.iter().position(|a| a.slug == slug) else {
            return;
        };
        let previous = self.state.agents[index].clone();
        let (model, effort) = self.draft(agent).saved_settings(&previous);
        let repos = self
            .draft(agent)
            .repos
            .iter()
            .map(|r| r.repo.name.clone())
            .collect();
        let effective = self.draft(agent).defaults(&self.state, Some(&slug)).0;
        let effort = effort.filter(|e| efforts(&effective).contains(&e.as_str()));
        let a = &mut self.state.agents[index];
        a.repos = repos;
        a.model = model;
        a.effort = effort;
        if let Err(error) = self.state.save() {
            self.state.agents[index] = previous;
            notify_err(window, cx, format!("Couldn't save agent defaults: {error}"));
            return;
        }
        let draft = self.draft_mut(agent);
        draft.model = None;
        draft.effort = None;
        draft.follow_app = false;
        draft.repos_edited = false;
        self.refresh_draft_defaults(&slug, cx);
        self.focus_draft(agent, window, cx);
    }

    pub(super) fn toggle_draft_plan(&mut self, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        if self.home.asking && !agent {
            return;
        }
        let draft = self.draft_mut(agent);
        if !draft.starting && !draft.repos.is_empty() {
            draft.plan = !draft.plan;
            cx.notify();
        }
    }

    fn draft_settings_source(&self, agent: bool) -> String {
        let draft = self.draft(agent);
        let default_agent = self.draft_slug(agent).and_then(|slug| self.agent(slug));
        let source = |model: bool| {
            if if model {
                draft.model.is_some()
            } else {
                draft.effort.is_some()
            } {
                "Task override"
            } else if !draft.follow_app
                && default_agent.is_some_and(|a| {
                    if model {
                        a.model.is_some()
                    } else {
                        a.effort.is_some()
                    }
                })
            {
                "Agent default"
            } else if draft.repos.iter().any(|r| {
                self.state.repo_settings.get(&r.repo.name).is_some_and(|s| {
                    if model {
                        s.model.is_some()
                    } else {
                        s.effort.is_some()
                    }
                })
            }) {
                "Repository default"
            } else {
                "App default"
            }
        };
        format!("Model: {} · Thinking: {}", source(true), source(false))
    }

    pub(super) fn draft_model_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        if self.draft(agent).starting || (!agent && self.home.asking) {
            return;
        }
        let (model, effort) = self
            .draft(agent)
            .defaults(&self.state, self.draft_slug(agent));
        let mut entries: Vec<_> = MODELS
            .iter()
            .map(|&id| {
                menu::item(
                    None,
                    model_label(id),
                    on_shark(cx, move |s, _, cx| {
                        let draft = s.draft_mut(agent);
                        draft.model = Some(id.into());
                        if draft
                            .effort
                            .as_deref()
                            .is_some_and(|e| !efforts(id).contains(&e))
                        {
                            draft.effort = None;
                        }
                        s.refresh_draft_sources(agent, cx);
                        cx.notify();
                    }),
                )
                .checked(model == id)
            })
            .collect();
        let (inherited_model, inherited_effort) =
            self.draft(agent).inherited_defaults(&self.state, None);
        entries.insert(
            0,
            menu::item(
                None,
                format!(
                    "Follow app default · {} · {}",
                    model_label(&inherited_model),
                    effort_label(inherited_effort.as_deref().unwrap_or(crate::models::EFFORT))
                ),
                on_shark(cx, move |s, _, cx| {
                    let draft = s.draft_mut(agent);
                    draft.model = None;
                    draft.effort = None;
                    draft.follow_app = true;
                    s.refresh_draft_sources(agent, cx);
                    cx.notify();
                }),
            )
            .checked(self.draft(agent).follow_app),
        );
        entries.insert(1, Entry::Sep);
        entries.push(Entry::Sep);
        entries.extend(efforts(&model).iter().map(|&level| {
            menu::item(
                None,
                effort_label(level),
                on_shark(cx, move |s, _, cx| {
                    s.draft_mut(agent).effort = Some(level.into());
                    cx.notify();
                }),
            )
            .checked(effort.as_deref().unwrap_or(crate::models::EFFORT) == level)
        }));
        if self.draft(agent).model.is_some()
            || self.draft(agent).effort.is_some()
            || self.draft(agent).follow_app
        {
            let (model, effort) = self
                .draft(agent)
                .inherited_defaults(&self.state, self.draft_slug(agent));
            entries.push(Entry::Sep);
            entries.push(menu::item(
                None,
                format!(
                    "Use agent default ({} · {})",
                    model_label(&model),
                    effort_label(effort.as_deref().unwrap_or(crate::models::EFFORT))
                ),
                on_shark(cx, move |s, _, cx| {
                    let draft = s.draft_mut(agent);
                    draft.model = None;
                    draft.effort = None;
                    draft.follow_app = false;
                    s.refresh_draft_sources(agent, cx);
                    cx.notify();
                }),
            ));
        }
        entries[0] =
            std::mem::replace(&mut entries[0], Entry::Sep).hint(self.draft_settings_source(agent));
        self.show_menu(window.mouse_position(), entries, window, cx);
    }

    pub(super) fn submit_task_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        if self.draft(agent).starting || self.draft(agent).staging > 0 {
            return;
        }
        if !agent {
            self.draft_composer_event(false, &ComposerEvent::Change, window, cx);
        }
        let input = if agent {
            self.home.agent_prompt.clone()
        } else {
            self.home.prompt.clone()
        };
        let text = input.read(cx).value().to_string();
        let mut draft = self.draft(agent).clone();
        draft.text = if agent {
            text
        } else {
            agent_prompt(text.trim(), &self.state.agents).1.to_string()
        };
        let link = self.selected_task_agent(cx);
        let slug = link.as_ref().map(|a| a.slug.clone());
        self.draft_mut(agent).starting = true;
        let composer = if agent {
            self.home.agent_composer.clone()
        } else {
            self.home.composer.clone()
        };
        composer.update(cx, |c, cx| c.set_read_only(true, cx));
        self.start_task_draft(
            draft,
            link,
            Rc::new(move |s, success, w, cx| {
                let same = !agent || s.home.agent_prompt_for == slug;
                let reset = TaskDraft::for_agent(&s.state, slug.as_deref());
                if same {
                    if success {
                        *s.draft_mut(agent) = if agent { reset } else { TaskDraft::default() };
                        if !agent {
                            s.home.draft_agent = None;
                        }
                        input.update(cx, |p, cx| p.set_value("", w, cx));
                    } else {
                        s.draft_mut(agent).starting = false;
                    }
                    composer.update(cx, |c, cx| c.set_read_only(false, cx));
                    s.refresh_draft_sources(agent, cx);
                } else if let Some(slug) = &slug {
                    if success {
                        s.home.agent_drafts.insert(slug.clone(), reset);
                    } else if let Some(draft) = s.home.agent_drafts.get_mut(slug) {
                        draft.starting = false;
                    }
                }
                cx.notify();
            }),
            window,
            cx,
        );
        cx.notify();
    }

    pub(super) fn pick_draft_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let destination = self.home.destination.clone();
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add to task".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |s, w, cx| {
                    s.attach_draft_files_to(&destination, paths, None, w, cx)
                })
                .ok();
            }
        })
        .detach();
    }

    fn attach_draft_files(
        &mut self,
        _agent: bool,
        paths: Vec<PathBuf>,
        image: Option<(Vec<u8>, &'static str)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attach_draft_files_to(&self.home.destination.clone(), paths, image, window, cx);
    }

    fn attach_draft_files_to(
        &mut self,
        destination: &Destination,
        paths: Vec<PathBuf>,
        image: Option<(Vec<u8>, &'static str)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.draft_at_mut(destination) else {
            return;
        };
        if draft.starting {
            return;
        }
        draft.staging += 1;
        let destination = destination.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { super::task_draft::stage_files(paths, image) })
                .await;
            this.update_in(cx, |s, window, cx| {
                let Some(draft) = s.draft_at_mut(&destination) else {
                    if let Ok(files) = result {
                        super::task_draft::discard_staged_files(&files);
                    }
                    return;
                };
                draft.staging = draft.staging.saturating_sub(1);
                match result {
                    Ok(files) => {
                        draft.files.extend(files);
                        if s.home.destination == destination {
                            s.focus_draft(s.draft_target(), window, cx);
                        }
                    }
                    Err(error) => notify_err(window, cx, error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn draft_file_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        let Some(path) = self.draft(agent).files.get(index) else {
            return;
        };
        let Ok(url) = url::Url::from_file_path(path) else {
            return;
        };
        self.show_menu(
            window.mouse_position(),
            vec![menu::item(IconName::File, "Open file", move |_, cx| {
                cx.open_url(url.as_str())
            })],
            window,
            cx,
        );
    }

    fn draft_tray_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.draft_target();
        let draft = self.draft(agent);
        if draft.starting {
            return;
        }
        let (issues, repos, files) = (draft.issues.len(), draft.repos.len(), draft.files.len());
        let empty = usize::from(repos == 0);
        let add = issues + repos * 2 + files + empty;
        let edited = draft.settings_edited(&self.state, self.draft_slug(agent));
        let count = add + 1 + if edited { 2 } else { 0 };
        let index = self.home.tray_index.min(count - 1);
        match event.keystroke.key.as_str() {
            "left" => self.home.tray_index = index.saturating_sub(1),
            "right" => self.home.tray_index = (index + 1).min(count - 1),
            "backspace" | "delete" if index < add - empty => {
                let draft = self.draft_mut(agent);
                if index < issues {
                    draft.issues.remove(index);
                } else if index < issues + repos * 2 {
                    draft.repos.remove((index - issues) / 2);
                    draft.repos_edited = true;
                } else {
                    let removed = draft.files.remove(index - issues - repos * 2);
                    super::task_draft::discard_staged_files(&[removed]);
                }
                self.home.tray_index = index.min(count.saturating_sub(2));
                self.refresh_draft_sources(agent, cx);
                if self.draft(agent).issues.len()
                    + self.draft(agent).repos.len()
                    + self.draft(agent).files.len()
                    == 0
                {
                    self.focus_draft(agent, window, cx);
                }
            }
            "enter" | "space" => {
                if index < issues {
                    let issue = &draft.issues[index];
                    let url = issue.url.clone();
                    let entries = vec![
                        menu::item(
                            None,
                            format!("{} · {}", issue.identifier, issue.title),
                            move |_, cx| cx.open_url(&url),
                        ),
                        menu::item(None, issue.status.clone(), |_, _| {}),
                    ];
                    self.show_menu(window.mouse_position(), entries, window, cx);
                } else if index < issues + repos * 2 {
                    let repo = (index - issues) / 2;
                    if (index - issues) % 2 == 0 {
                        self.draft_repo_menu(repo, window, cx);
                    } else {
                        let name = draft.repos[repo].repo.name.clone();
                        self.pick_draft_branch(Some((name, false)), window, cx);
                    }
                } else if index < issues + repos * 2 + files {
                    self.draft_file_menu(index - issues - repos * 2, window, cx);
                } else if empty == 1 && index == add - 1 {
                    self.draft_repository_menu(window, cx);
                } else if index == add {
                    self.draft_add_menu(window, cx);
                } else if index == add + 1 {
                    self.reset_draft_defaults(window, cx);
                } else if index == add + 2 {
                    self.save_draft_defaults(window, cx);
                }
            }
            "escape" | "down" | "tab" => self.focus_draft(agent, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_task_composer(
        &self,
        agent: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let draft = self.draft(agent);
        let asking = !agent && self.home.asking;
        let input = if agent {
            &self.home.agent_prompt
        } else {
            &self.home.prompt
        };
        let composer = if agent {
            self.home.agent_composer.clone()
        } else {
            self.home.composer.clone()
        };
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let mut pills: Vec<AnyElement> = vec![];
        let default_agent = self.draft_slug(agent).and_then(|slug| self.agent(slug));
        let edited = draft.settings_edited(&self.state, self.draft_slug(agent));
        let tray_focused = self.home.tray_focus.is_focused(window);
        let tray_index = self.home.tray_index;
        let add_index = draft.issues.len()
            + draft.repos.len() * 2
            + draft.files.len()
            + usize::from(draft.repos.is_empty());
        if !asking {
            for (i, issue) in draft.issues.iter().enumerate() {
                let disconnected = self.linear_org_id() != Some(issue.org.as_str());
                let label = format!(
                    "{} {}{}",
                    issue.identifier,
                    issue.title,
                    if disconnected {
                        " · Linear disconnected"
                    } else {
                        ""
                    }
                );
                let mut rows = vec![
                    ("Issue", issue.identifier.clone()),
                    ("Status", issue.status.clone()),
                ];
                if let Some(project) = &issue.project {
                    rows.push(("Project", project.clone()));
                }
                if let Some(assignee) = &issue.assignee {
                    rows.push(("Assignee", assignee.clone()));
                }
                if disconnected {
                    rows.push(("Linear", "Disconnected".into()));
                }
                let card = super::attach::ChipCard {
                    mark: Icon::new(IconName::Tag),
                    title: issue.title.clone(),
                    rows,
                };
                pills.push(
                    h_flex()
                        .gap_1()
                        .max_w(px(400.))
                        .child(
                            card.around(
                                &format!("draft-issue-{i}"),
                                Button::new(("draft-issue", i))
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Tag)
                                    .label(label)
                                    .disabled(draft.starting),
                                self.menu.is_some(),
                            ),
                        )
                        .child(
                            Button::new(("remove-draft-issue", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::X)
                                .accessibility_label("Remove issue from this task")
                                .disabled(draft.starting)
                                .on_click(cx.listener(move |s, _, w, cx| {
                                    s.draft_mut(agent).issues.remove(i);
                                    s.focus_draft(agent, w, cx);
                                })),
                        )
                        .into_any_element(),
                );
            }
            for (i, repo) in draft.repos.iter().enumerate() {
                let branch = match &repo.branch {
                    BranchSpec::New { base } => format!(
                        "New branch from {}",
                        if base.is_empty() {
                            self.draft_bases
                                .get(&repo.repo.path)
                                .map(String::as_str)
                                .unwrap_or("default branch")
                        } else {
                            base
                        }
                    ),
                    BranchSpec::Existing(b) => b.clone(),
                    BranchSpec::Pr(pr) => repo
                        .pr_label
                        .clone()
                        .unwrap_or_else(|| format!("Pull request #{}", pr.number)),
                };
                let changed = default_agent.is_some_and(|a| !a.repos.contains(&repo.repo.name));
                let stop = draft.issues.len() + i * 2;
                let name = repo.repo.name.clone();
                let tooltip = format!(
                    "{}, {}{} · Branch or pull request (⌘⇧B)",
                    name,
                    branch,
                    if changed {
                        ", changed from agent default"
                    } else {
                        ""
                    }
                );
                pills.push(
                    h_flex()
                        .child(
                            Button::new(("draft-repo", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::FolderGit2)
                                .label(format!(
                                    "{}{}",
                                    if changed { "● " } else { "" },
                                    repo.repo.name
                                ))
                                .accessibility_label(format!(
                                    "{}{}",
                                    repo.repo.name,
                                    if changed {
                                        ", changed from agent default"
                                    } else {
                                        ""
                                    }
                                ))
                                .tooltip("Switch or remove repository (⌘I)")
                                .when(tray_focused && tray_index == stop, |b| {
                                    b.bg(theme.list_hover)
                                })
                                .disabled(draft.starting)
                                .on_click(
                                    cx.listener(move |s, _, w, cx| s.draft_repo_menu(i, w, cx)),
                                ),
                        )
                        .child(div().h(px(16.)).border_l_1().border_color(theme.border))
                        .child(
                            Button::new(("draft-branch", i))
                                .xsmall()
                                .ghost()
                                .icon(if matches!(repo.branch, BranchSpec::Pr(_)) {
                                    IconName::GitPullRequest
                                } else {
                                    IconName::GitBranch
                                })
                                .label(branch)
                                .accessibility_label(tooltip.clone())
                                .tooltip(tooltip)
                                .when(tray_focused && tray_index == stop + 1, |b| {
                                    b.bg(theme.list_hover)
                                })
                                .disabled(draft.starting)
                                .on_click(cx.listener(move |s, _, w, cx| {
                                    s.pick_draft_branch(Some((name.clone(), false)), w, cx)
                                }))
                                .map(|b| {
                                    self.anchor_draft_control(
                                        format!("branch-{}", repo.repo.name),
                                        b,
                                    )
                                }),
                        )
                        .child(
                            Button::new(("remove-draft-repo", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::X)
                                .accessibility_label("Remove repository from this task")
                                .disabled(draft.starting)
                                .on_click(cx.listener(move |s, _, w, cx| {
                                    s.draft_mut(agent).repos.remove(i);
                                    s.draft_mut(agent).repos_edited = true;
                                    s.focus_draft(agent, w, cx);
                                })),
                        )
                        .into_any_element(),
                );
            }
            for (i, path) in draft.files.iter().enumerate() {
                pills.push(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(("draft-file", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::File)
                                .label(
                                    path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned(),
                                )
                                .tooltip("Open file (Enter or Space)")
                                .disabled(draft.starting)
                                .on_click(
                                    cx.listener(move |s, _, w, cx| s.draft_file_menu(i, w, cx)),
                                ),
                        )
                        .child(
                            Button::new(("remove-draft-file", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::X)
                                .accessibility_label("Remove file from this task")
                                .disabled(draft.starting)
                                .on_click(cx.listener(move |s, _, w, cx| {
                                    let removed = s.draft_mut(agent).files.remove(i);
                                    super::task_draft::discard_staged_files(&[removed]);
                                    s.focus_draft(agent, w, cx);
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        let mut footer = h_flex().gap_2().flex_wrap();
        if !agent && !asking {
            let selected = self.draft_slug(false).and_then(|slug| self.agent(slug));
            footer = footer.child(
                Button::new("home-agent-picker")
                    .xsmall()
                    .ghost()
                    .icon(IconName::AtSign)
                    .label(selected.map_or_else(|| "No agent".into(), |a| a.name.clone()))
                    .disabled(draft.starting)
                    .tooltip("Pick an agent, or type @")
                    .on_click(cx.listener(|s, _, w, cx| s.pick_home_agent(w, cx))),
            );
        }
        if !asking {
            let (model, effort) = draft.defaults(&self.state, self.draft_slug(agent));
            let overridden = default_agent
                .is_some_and(|a| draft.saved_settings(a) != (a.model.clone(), a.effort.clone()));
            let source = self.draft_settings_source(agent);
            let label = format!(
                "{}{} · {}",
                if overridden { "● " } else { "" },
                model_label(&model),
                effort_label(effort.as_deref().unwrap_or(crate::models::EFFORT))
            );
            footer = footer
                .child(
                    Button::new("draft-model")
                        .xsmall()
                        .label(label.clone())
                        .disabled(draft.starting)
                        .accessibility_label(format!(
                            "{label}, {source}{}",
                            if overridden {
                                ", changed from agent default"
                            } else {
                                ""
                            }
                        ))
                        .tooltip(format!("{source} · Model and thinking for this task (⌘/)"))
                        .on_click(cx.listener(|s, _, w, cx| s.draft_model_menu(w, cx))),
                )
                .child(
                    Button::new("draft-plan")
                        .xsmall()
                        .ghost()
                        .label(if draft.plan || draft.repos.is_empty() {
                            "✓ Plan"
                        } else {
                            "Plan"
                        })
                        .disabled(draft.starting || draft.repos.is_empty())
                        .tooltip(if draft.repos.is_empty() {
                            "Tasks without a repository start in Plan mode."
                        } else {
                            "Plan before implementing (⇧Tab)"
                        })
                        .on_click(cx.listener(|s, _, _, cx| s.toggle_draft_plan(cx))),
                );
        }
        footer = footer.child(div().flex_1()).child(
            Button::new("draft-start")
                .loading(draft.starting || draft.staging > 0)
                .primary()
                .small()
                .label(if asking {
                    "Ask ↵"
                } else if draft.staging > 0 {
                    "Adding files…"
                } else if draft.starting {
                    "Starting…"
                } else {
                    "Start ↵"
                })
                .disabled(draft.starting || draft.staging > 0)
                .on_click(cx.listener(move |s, _, w, cx| {
                    if agent {
                        s.submit_task_draft(w, cx);
                    } else {
                        s.submit_prompt(w, cx);
                    }
                })),
        );
        let hits = if agent { vec![] } else { self.mentions(cx) };
        let at = self.home.mention.min(hits.len().saturating_sub(1));
        let suggestions = (!hits.is_empty()).then(|| {
            v_flex()
                .id("home-mentions")
                .test_support()
                .children(hits.iter().enumerate().map(|(row, &i)| {
                    let a = &self.state.agents[i];
                    Button::new(("home-mention", i))
                        .ghost()
                        .small()
                        .accessibility_label(format!("{} @{}", a.name, a.slug))
                        .child(
                            h_flex()
                                .gap_1()
                                .child(super::agent_icon_view::agent_icon(&a.icon))
                                .child(format!("{} @{}", a.name, a.slug)),
                        )
                        .when(row == at, |b| b.bg(theme.list_hover))
                        .on_click(
                            cx.listener(move |s, _, w, cx| s.complete_mention(Some(i), w, cx)),
                        )
                }))
        });
        v_flex()
            .gap_2()
            .max_w(px(712.))
            .w_full()
            .child(
                v_flex()
                    .id(if agent {
                        "agent-composer"
                    } else {
                        "home-composer"
                    })
                    .test_support()
                    .key_context("HomePrompt")
                    .on_action(cx.listener(|s, _: &PickModel, w, cx| s.draft_model_menu(w, cx)))
                    .on_action(cx.listener(|s, _: &AddToDraft, w, cx| s.draft_add_menu(w, cx)))
                    .on_action(cx.listener(|s, _: &ChangeDraftBranch, w, cx| s.change_draft_branch(w, cx)))
                    .on_action(cx.listener(|s, _: &TogglePlanMode, _, cx| s.toggle_draft_plan(cx)))
                    .px_3()
                    .py_2()
                    .gap_2()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(if focused { theme.primary } else { theme.border })
                    .bg(theme.secondary)
                    .text_size(px(14.))
                    .child(composer)
                    .children(suggestions)
                    .when(!asking, |d| {
                        d.child(
                            h_flex()
                                .id("draft-tray")
                                .track_focus(&self.home.tray_focus)
                                .on_key_down(cx.listener(|s, e, w, cx| s.draft_tray_key(e, w, cx)))
                                .gap_2()
                                .flex_wrap()
                                .children(pills.into_iter().enumerate().map(|(i, pill)| {
                                    let stop = if i < draft.issues.len() { i }
                                        else if i < draft.issues.len() + draft.repos.len() { draft.issues.len() + (i - draft.issues.len()) * 2 }
                                        else { i + draft.repos.len() };
                                    div()
                                        .rounded(theme.radius)
                                        .border_1()
                                        .border_color(
                                            if self.home.tray_focus.is_focused(window)
                                                && (self.home.tray_index == stop || (i >= draft.issues.len() && i < draft.issues.len() + draft.repos.len() && self.home.tray_index == stop + 1))
                                            {
                                                theme.primary
                                            } else {
                                                theme.border
                                            },
                                        )
                                        .child(pill)
                                }))
                                .when(draft.repos.is_empty(), |row| row.child(
                                    Button::new("draft-empty-repo").xsmall().ghost().icon(IconName::FolderGit2)
                                        .label("Add repository").text_color(theme.muted_foreground)
                                        .border_1().border_dashed().border_color(theme.border)
                                        .when(tray_focused && tray_index == add_index - 1, |b| b.bg(theme.list_hover))
                                        .tooltip("Add repository (⌘I)").disabled(draft.starting)
                                        .on_click(cx.listener(|s, _, w, cx| s.draft_repository_menu(w, cx)))
                                ))
                                .child(Button::new("draft-add").xsmall().ghost().icon(IconName::Plus)
                                    .when(draft.issues.is_empty() && draft.repos.is_empty() && draft.files.is_empty(), |b| b.label("Issue, PR or repository"))
                                    .accessibility_label("Add issue, PR or repository")
                                    .tooltip("Add issue, PR or repository (⌘I)").disabled(draft.starting)
                                    .when(tray_focused && tray_index == add_index, |b| b.bg(theme.list_hover))
                                    .on_click(cx.listener(|s, _, w, cx| s.draft_add_menu(w, cx)))
                                    .map(|b| self.anchor_draft_control("draft-add".into(), b)))
                                .when(draft.repos.is_empty(), |row| row.child(div().text_xs().text_color(theme.muted_foreground).child("Plans only without a repository")))
                                .when(edited, |row| row.child(
                                    h_flex().gap_1().child(div().text_xs().text_color(theme.muted_foreground).child("Edited"))
                                        .child(Button::new("draft-reset").xsmall().ghost().label("Reset").disabled(draft.starting)
                                            .when(tray_focused && tray_index == add_index + 1, |b| b.bg(theme.list_hover))
                                            .on_click(cx.listener(|s, _, w, cx| s.reset_draft_defaults(w, cx))))
                                        .child(Button::new("draft-save-default").xsmall().ghost().label("Save as default").disabled(draft.starting)
                                            .tooltip(format!("Saves repositories, model and thinking for {}", default_agent.map_or("agent", |a| a.name.as_str())))
                                            .when(tray_focused && tray_index == add_index + 2, |b| b.bg(theme.list_hover))
                                            .on_click(cx.listener(|s, _, w, cx| s.save_draft_defaults(w, cx))))
                                )),
                        )
                    })
                    .child(footer),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(crate::ui::hint("shift-enter", "new line"))
                    .when(!asking, |d| {
                        d.child(crate::ui::hint(
                            "@",
                            if agent { "file" } else { "agent / file" },
                        ))
                        .child(crate::ui::hint("#", "issue or PR"))
                        .child(crate::ui::hint("/", "skill"))
                        .child(crate::ui::hint("cmd-i", "add"))
                    })
                    .when(!agent, |d| {
                        d.child(crate::ui::hint(
                            "tab",
                            if asking { "new task" } else { "ask Shark" },
                        ))
                    }),
            )
            .into_any_element()
    }
}
