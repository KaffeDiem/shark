//! Home's task draft controls. Pickers only change the draft.
use super::home::agent_prompt;
use super::task_draft::{BranchSpec, DraftRepo, TaskDraft};
use super::*;
use crate::models::{effort_label, efforts, model_label};
use crate::task_composer::{Event as ComposerEvent, Source};
use gpui_kit::component::{Disableable as _, Selectable as _};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DraftStop {
    Agent,
    Repo(String),
    Issue(String, String),
    File(PathBuf),
    More,
    Attach,
    Edited,
}

impl DraftStop {
    fn anchor(&self) -> String {
        match self {
            Self::Agent => "home-agent-picker".into(),
            Self::Repo(name) => format!("branch-{name}"),
            Self::Issue(org, id) => format!("issue-{org}-{id}"),
            Self::File(path) => format!("file-{}", path.display()),
            Self::More => "draft-more".into(),
            Self::Attach => "draft-add".into(),
            Self::Edited => "draft-edited".into(),
        }
    }
}

fn draft_chip(id: impl Into<ElementId>, icon: impl Into<Icon>, label: String) -> Button {
    Shark::chip(id, icon, label)
        .h(px(26.))
        .min_w(px(26.))
        .rounded(px(13.))
        .px(px(9.))
        .max_w(px(200.))
}

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

    /// Agent `slug`'s saved repos, into the draft if it's for `slug` and its repos are unedited.
    pub(super) fn refresh_draft_defaults(&mut self, slug: &str, cx: &mut Context<Self>) {
        let draft = &mut self.home.draft;
        if self.home.draft_agent.as_deref() != Some(slug) {
            return;
        }
        if !draft.repos_edited {
            let defaults = TaskDraft::for_agent(&self.state, Some(slug));
            draft.repos = defaults
                .repos
                .iter()
                .map(|repo| {
                    (draft.repos.iter())
                        .find(|r| r.repo.name == repo.repo.name)
                        .unwrap_or(repo)
                        .clone()
                })
                .collect();
        }
        self.refresh_draft_sources(cx);
    }

    pub(super) fn refresh_draft_sources(&mut self, cx: &mut Context<Self>) {
        self.sync_reference_completion(cx);
        let unresolved: Vec<_> = self
            .home
            .draft
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
        let draft = &self.home.draft;
        let codex = crate::codex::is_codex(Some(
            &draft
                .defaults(&self.state, self.home.draft_agent.as_deref())
                .0,
        ));
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
            .home
            .draft_agent
            .as_deref()
            .map(super::agent_settings::skill_names)
            .unwrap_or_default();
        let home_agent = self.home.draft_agent.clone();
        self.home.composer.update(cx, |c, cx| {
            c.set_home_agent(home_agent, cx);
            c.set_sources(sources, skills, true, codex, cx);
        });
    }

    pub(super) fn draft_composer_event(
        &mut self,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ComposerEvent::AttachReference(reference) => {
                self.attach_draft_reference(reference.clone(), window, cx)
            }
            ComposerEvent::SearchIssues(query) => self.search_reference_issues(query.clone(), cx),
            ComposerEvent::PasteReference { id, text } => {
                self.paste_draft_reference(*id, text.clone(), window, cx)
            }
            ComposerEvent::UndoReference { id, .. } => self.undo_draft_reference(*id, window, cx),
            ComposerEvent::Submit => {
                if !self.mentions(cx).is_empty() {
                    self.complete_mention(None, window, cx);
                } else {
                    self.submit_prompt(window, cx);
                }
            }
            ComposerEvent::Change => {
                let content = self.home.prompt.read(cx).content();
                let text = content.text().to_string();
                let slug = agent_prompt(text.trim(), &self.state.agents)
                    .0
                    .map(str::to_string);
                if self.home.draft_agent != slug {
                    if !self.home.draft.repos_edited {
                        self.home.draft.repos =
                            TaskDraft::for_agent(&self.state, slug.as_deref()).repos;
                    }
                    self.home.draft_agent = slug;
                    self.refresh_draft_sources(cx);
                }
                self.home.mention = 0;
                self.home.draft.text = text;
                self.home.draft.content = Some(content);
                cx.notify();
            }
            ComposerEvent::Escape => {
                self.cancel_ask(cx);
                self.home.focus.focus(window, cx);
                cx.notify();
            }
            ComposerEvent::ToggleAsk => self.toggle_ask_mode(window, cx),
            ComposerEvent::TogglePlan => self.toggle_draft_plan(cx),
            ComposerEvent::ModelMenu => self.draft_model_menu(window, cx),
            ComposerEvent::AgentMove(by) => self.move_mention(*by, cx),
            ComposerEvent::Tray => {
                if !self.home.asking {
                    self.home.tray_stop = self.draft_stops().first().cloned();
                    self.home.tray_focus.focus(window, cx);
                    cx.notify();
                }
            }
            ComposerEvent::Attach { paths, image } => {
                self.attach_draft_files(paths.clone(), image.clone(), window, cx)
            }
        }
    }

    pub(super) fn focus_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.home.prompt.clone();
        let placeholder = if self.home.asking {
            "Ask how to do something in Shark".into()
        } else if let Some(issue) = self.home.draft.issues.first() {
            format!("Add notes, or press Enter to plan {}", issue.identifier)
        } else if let Some(a) = self.home.draft_agent.as_deref().and_then(|s| self.agent(s)) {
            format!("Describe a task for {}", a.name)
        } else {
            "Describe the work; enter starts a task".into()
        };
        prompt.update(cx, |p, cx| {
            p.set_placeholder(placeholder, window, cx);
            p.focus(window, cx);
        });
        self.refresh_draft_sources(cx);
        cx.notify();
    }

    pub(super) fn add_draft_issues(
        &mut self,
        issues: Vec<Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.home.draft;
        if draft.starting {
            return;
        }
        for issue in issues {
            if !(draft.issues.iter()).any(|i| i.id == issue.id && i.org == issue.org) {
                draft.issues.push(issue);
            }
        }
        self.focus_draft(window, cx);
        cx.notify();
    }

    pub(super) fn pick_draft_branch(
        &mut self,
        only: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.home.draft.starting {
            return;
        }
        let current = self.home.draft.repos.clone();
        let position = self.draft_control_position(
            &only
                .as_ref()
                .map(|(name, _)| format!("branch-{name}"))
                .unwrap_or_else(|| "draft-add".into()),
            window,
        );
        self.pick_draft_repos_at(
            current,
            only,
            Rc::new(move |s, repos, w, cx| {
                let draft = &mut s.home.draft;
                draft.repos = repos;
                draft.repos_edited = true;
                s.focus_draft(w, cx);
            }),
            position,
            window,
            cx,
        );
    }

    fn draft_repo_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.home.draft.repos.get(index) else {
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
                        .home
                        .draft
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
                        if s.home.draft.starting {
                            return;
                        }
                        s.home.draft.repos[index] = DraftRepo {
                            repo: repo.clone(),
                            pr_label: None,
                            branch: BranchSpec::New {
                                base: String::new(),
                            },
                        };
                        s.home.draft.repos_edited = true;
                        s.focus_draft(w, cx);
                    }),
                )
                .checked(checked)
            })
            .collect();
        let branch = self.draft_branch_label(current);
        let selected = name.clone();
        entries.insert(
            0,
            menu::item(
                IconName::GitBranch,
                "Branch or pull request…",
                on_shark(cx, move |s, w, cx| {
                    s.pick_draft_branch(Some((selected.clone(), false)), w, cx);
                }),
            )
            .hint(format!("{branch} · ⌘⇧B")),
        );
        entries.insert(1, Entry::Sep);
        entries.push(Entry::Sep);
        entries.push(menu::item(
            IconName::X,
            "Remove repository",
            on_shark(cx, move |s, w, cx| {
                if s.home.draft.starting {
                    return;
                }
                s.home.draft.repos.remove(index);
                s.home.draft.repos_edited = true;
                s.focus_draft(w, cx);
            }),
        ));
        self.show_menu(
            self.draft_control_position(&DraftStop::Repo(name).anchor(), window),
            entries,
            window,
            cx,
        );
    }

    pub(super) fn draft_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.home_shown() || self.home.draft.starting || self.home.asking {
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
        if !self.home_shown() || self.home.draft.starting || self.home.asking {
            return;
        }
        let draft = &self.home.draft;
        let index = match &self.home.tray_stop {
            Some(DraftStop::Repo(name)) if self.home.tray_focus.is_focused(window) => draft
                .repos
                .iter()
                .position(|r| &r.repo.name == name)
                .unwrap_or(0),
            _ => 0,
        };
        if let Some(repo) = draft.repos.get(index).or_else(|| draft.repos.first()) {
            self.pick_draft_branch(Some((repo.repo.name.clone(), false)), window, cx);
        } else {
            self.draft_repository_menu(window, cx);
        }
    }

    pub(super) fn reset_draft_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home.draft.starting || self.home.draft_agent.as_deref().is_none() {
            return;
        }
        let mut defaults = TaskDraft::for_agent(&self.state, self.home.draft_agent.as_deref());
        let draft = &mut self.home.draft;
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
        self.focus_draft(window, cx);
    }

    pub(super) fn save_draft_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home.draft.starting {
            return;
        }
        let Some(slug) = self.home.draft_agent.as_deref().map(str::to_owned) else {
            return;
        };
        let Some(index) = self.state.agents.iter().position(|a| a.slug == slug) else {
            return;
        };
        let previous = self.state.agents[index].clone();
        let (model, effort) = self.home.draft.saved_settings(&previous);
        let repos = self
            .home
            .draft
            .repos
            .iter()
            .map(|r| r.repo.name.clone())
            .collect();
        let effective = self.home.draft.defaults(&self.state, Some(&slug)).0;
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
        let draft = &mut self.home.draft;
        draft.model = None;
        draft.effort = None;
        draft.follow_app = false;
        draft.repos_edited = false;
        self.refresh_draft_defaults(&slug, cx);
        self.focus_draft(window, cx);
    }

    pub(super) fn toggle_draft_plan(&mut self, cx: &mut Context<Self>) {
        if self.home.asking {
            return;
        }
        let draft = &mut self.home.draft;
        if !draft.starting && !draft.repos.is_empty() {
            draft.plan = !draft.plan;
            cx.notify();
        }
    }

    fn draft_settings_source(&self) -> String {
        let draft = &self.home.draft;
        let default_agent = self
            .home
            .draft_agent
            .as_deref()
            .and_then(|slug| self.agent(slug));
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
        if self.home.draft.starting || self.home.asking {
            return;
        }
        let (model, effort) = self
            .home
            .draft
            .defaults(&self.state, self.home.draft_agent.as_deref());
        let mut entries: Vec<_> = MODELS
            .iter()
            .map(|&id| {
                menu::item(
                    None,
                    model_label(id),
                    on_shark(cx, move |s, _, cx| {
                        let draft = &mut s.home.draft;
                        draft.model = Some(id.into());
                        if draft
                            .effort
                            .as_deref()
                            .is_some_and(|e| !efforts(id).contains(&e))
                        {
                            draft.effort = None;
                        }
                        s.refresh_draft_sources(cx);
                        cx.notify();
                    }),
                )
                .checked(model == id)
            })
            .collect();
        let (inherited_model, inherited_effort) =
            self.home.draft.inherited_defaults(&self.state, None);
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
                    let draft = &mut s.home.draft;
                    draft.model = None;
                    draft.effort = None;
                    draft.follow_app = true;
                    s.refresh_draft_sources(cx);
                    cx.notify();
                }),
            )
            .checked(self.home.draft.follow_app),
        );
        entries.insert(1, Entry::Sep);
        entries.push(Entry::Sep);
        entries.extend(efforts(&model).iter().map(|&level| {
            menu::item(
                None,
                effort_label(level),
                on_shark(cx, move |s, _, cx| {
                    s.home.draft.effort = Some(level.into());
                    cx.notify();
                }),
            )
            .checked(effort.as_deref().unwrap_or(crate::models::EFFORT) == level)
        }));
        if self.home.draft.model.is_some()
            || self.home.draft.effort.is_some()
            || self.home.draft.follow_app
        {
            let (model, effort) = self
                .home
                .draft
                .inherited_defaults(&self.state, self.home.draft_agent.as_deref());
            entries.push(Entry::Sep);
            entries.push(menu::item(
                None,
                format!(
                    "Use agent default ({} · {})",
                    model_label(&model),
                    effort_label(effort.as_deref().unwrap_or(crate::models::EFFORT))
                ),
                on_shark(cx, move |s, _, cx| {
                    let draft = &mut s.home.draft;
                    draft.model = None;
                    draft.effort = None;
                    draft.follow_app = false;
                    s.refresh_draft_sources(cx);
                    cx.notify();
                }),
            ));
        }
        if self.home.footer_density >= 2 {
            entries.push(Entry::Sep);
            if self.home.draft.repos.is_empty() {
                entries.push(Entry::Heading(
                    "Plan only".into(),
                    self.draft_plan_tooltip().into(),
                ));
            } else {
                entries.push(
                    menu::item(
                        IconName::ListTodo,
                        "Plan before implementing",
                        on_shark(cx, |s, _, cx| s.toggle_draft_plan(cx)),
                    )
                    .checked(self.home.draft.plan)
                    .hint("⇧Tab"),
                );
            }
        }
        entries[0] =
            std::mem::replace(&mut entries[0], Entry::Sep).hint(self.draft_settings_source());
        self.show_menu(window.mouse_position(), entries, window, cx);
    }

    pub(super) fn submit_task_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home.draft.starting || self.home.draft.staging > 0 {
            return;
        }
        self.draft_composer_event(&ComposerEvent::Change, window, cx);
        let text = self.home.prompt.read(cx).value().to_string();
        let mut draft = self.home.draft.clone();
        draft.text = agent_prompt(text.trim(), &self.state.agents).1.to_string();
        // Captured now: the prompt can change while the task is created.
        let link = (self.home.draft_agent.as_deref())
            .and_then(|slug| self.agent(slug))
            .map(crate::state::AgentLink::from);
        self.home.draft.starting = true;
        let composer = self.home.composer.clone();
        composer.update(cx, |c, cx| c.set_read_only(true, cx));
        self.start_task_draft(
            draft,
            link,
            Rc::new(move |s, success, w, cx| {
                if success {
                    s.home.draft = TaskDraft::default();
                    s.home.draft_agent = None;
                    s.home.prompt.update(cx, |p, cx| p.set_value("", w, cx));
                } else {
                    s.home.draft.starting = false;
                }
                composer.update(cx, |c, cx| c.set_read_only(false, cx));
                s.refresh_draft_sources(cx);
                cx.notify();
            }),
            window,
            cx,
        );
        cx.notify();
    }

    pub(super) fn pick_draft_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add to task".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |s, w, cx| s.attach_draft_files(paths, None, w, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn attach_draft_files(
        &mut self,
        paths: Vec<PathBuf>,
        image: Option<(Vec<u8>, &'static str)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.home.draft;
        if draft.starting {
            return;
        }
        draft.staging += 1;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { super::task_draft::stage_files(paths, image) })
                .await;
            this.update_in(cx, |s, window, cx| {
                let draft = &mut s.home.draft;
                draft.staging = draft.staging.saturating_sub(1);
                match result {
                    Ok(files) => {
                        draft.files.extend(files);
                        s.focus_draft(window, cx);
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
        let Some(path) = self.home.draft.files.get(index).cloned() else {
            return;
        };
        let Ok(url) = url::Url::from_file_path(&path) else {
            return;
        };
        let stop = DraftStop::File(path);
        let remove = stop.clone();
        self.show_menu(
            self.draft_control_position(&stop.anchor(), window),
            vec![
                menu::item(IconName::File, "Open file", move |_, cx| {
                    cx.open_url(url.as_str())
                }),
                Entry::Sep,
                menu::item(
                    IconName::X,
                    "Remove file",
                    on_shark(cx, move |s, w, cx| s.remove_draft_stop(&remove, w, cx)),
                ),
            ],
            window,
            cx,
        );
    }

    fn draft_issue_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(issue) = self.home.draft.issues.get(index) else {
            return;
        };
        let stop = DraftStop::Issue(issue.org.clone(), issue.id.clone());
        let remove = stop.clone();
        let url = issue.url.clone();
        self.show_menu(
            self.draft_control_position(&stop.anchor(), window),
            vec![
                menu::item(IconName::ExternalLink, "Open in Linear", move |_, cx| {
                    cx.open_url(&url)
                }),
                Entry::Sep,
                menu::item(
                    IconName::X,
                    "Remove issue",
                    on_shark(cx, move |s, w, cx| s.remove_draft_stop(&remove, w, cx)),
                ),
            ],
            window,
            cx,
        );
    }

    fn draft_branch_label(&self, repo: &DraftRepo) -> String {
        match &repo.branch {
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
            BranchSpec::Existing(branch) => branch.clone(),
            BranchSpec::Pr(pr) => repo
                .pr_label
                .clone()
                .unwrap_or_else(|| format!("Pull request #{}", pr.number)),
        }
    }

    fn draft_repo_label(&self, repo: &DraftRepo, changed: bool) -> String {
        let suffix = match &repo.branch {
            BranchSpec::New { base }
                if base.is_empty() || self.draft_bases.get(&repo.repo.path) == Some(base) =>
            {
                String::new()
            }
            BranchSpec::New { base } | BranchSpec::Existing(base) => format!(" · {base}"),
            BranchSpec::Pr(pr) => format!(" · #{}", pr.number),
        };
        format!(
            "{}{}{suffix}",
            if changed { "● " } else { "" },
            repo.repo.name
        )
    }

    fn draft_plan_tooltip(&self) -> String {
        if self.home.draft.repos.is_empty() {
            format!(
                "Attach a repository to let {} implement",
                self.home
                    .draft_agent
                    .as_deref()
                    .and_then(|slug| self.agent(slug))
                    .map_or("Shark", |a| a.name.as_str())
            )
        } else {
            "Plan before implementing (⇧Tab)".into()
        }
    }

    fn draft_edited_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = &self.home.draft;
        let Some(default) = self
            .home
            .draft_agent
            .as_deref()
            .and_then(|slug| self.agent(slug))
        else {
            return;
        };
        if draft.starting {
            return;
        }
        let mut changes: Vec<_> = default
            .repos
            .iter()
            .filter(|name| !draft.repos.iter().any(|r| &r.repo.name == *name))
            .map(|name| format!("Removed {name}"))
            .collect();
        changes.extend(
            draft
                .repos
                .iter()
                .filter(|r| !default.repos.contains(&r.repo.name))
                .map(|r| format!("Added {}", r.repo.name)),
        );
        if draft.saved_settings(default) != (default.model.clone(), default.effort.clone()) {
            let (model, effort) = draft.defaults(&self.state, self.home.draft_agent.as_deref());
            changes.push(format!(
                "Model {} · {}",
                model_label(&model),
                effort_label(effort.as_deref().unwrap_or(crate::models::EFFORT))
            ));
        }
        let name = default.name.clone();
        let entries = vec![
            Entry::Heading(
                format!("Differs from {name}'s defaults").into(),
                changes.join(" · ").into(),
            ),
            menu::item(
                IconName::RotateCcw,
                format!("Reset to {name}'s defaults"),
                on_shark(cx, |s, w, cx| s.reset_draft_defaults(w, cx)),
            ),
            menu::item(
                IconName::Save,
                format!("Save as {name}'s defaults"),
                on_shark(cx, |s, w, cx| s.save_draft_defaults(w, cx)),
            ),
        ];
        self.show_menu(
            self.draft_control_position("draft-edited", window),
            entries,
            window,
            cx,
        );
    }

    pub(super) fn draft_chip_stops(&self) -> Vec<DraftStop> {
        let draft = &self.home.draft;
        std::iter::once(DraftStop::Agent)
            .chain(
                draft
                    .repos
                    .iter()
                    .map(|r| DraftStop::Repo(r.repo.name.clone())),
            )
            .chain(
                draft
                    .issues
                    .iter()
                    .map(|i| DraftStop::Issue(i.org.clone(), i.id.clone())),
            )
            .chain(draft.files.iter().cloned().map(DraftStop::File))
            .collect()
    }

    pub(super) fn draft_stops(&self) -> Vec<DraftStop> {
        let chips = self.draft_chip_stops();
        let mut visible: Vec<_> = chips.iter().take(self.home.tray_shown).cloned().collect();
        if visible.len() < chips.len() {
            visible.push(DraftStop::More);
        }
        visible.push(DraftStop::Attach);
        if self
            .home
            .draft
            .settings_edited(&self.state, self.home.draft_agent.as_deref())
        {
            visible.push(DraftStop::Edited);
        }
        visible
    }

    fn fit_draft_stops(&mut self, shown: usize, cx: &mut Context<Self>) {
        self.home.tray_shown = shown;
        let visible = self.draft_stops();
        if self
            .home
            .tray_stop
            .as_ref()
            .is_some_and(|stop| !visible.contains(stop))
        {
            self.home.tray_stop = if self
                .draft_chip_stops()
                .iter()
                .any(|stop| Some(stop) == self.home.tray_stop.as_ref())
            {
                Some(DraftStop::More)
            } else {
                visible.first().cloned()
            };
        }
        cx.notify();
    }

    fn open_draft_stop(&mut self, stop: &DraftStop, window: &mut Window, cx: &mut Context<Self>) {
        let draft = &self.home.draft;
        match stop {
            DraftStop::Agent => self.pick_home_agent(window, cx),
            DraftStop::Repo(name) => {
                if let Some(i) = draft.repos.iter().position(|r| &r.repo.name == name) {
                    self.draft_repo_menu(i, window, cx);
                }
            }
            DraftStop::Issue(org, id) => {
                if let Some(i) = draft
                    .issues
                    .iter()
                    .position(|r| &r.org == org && &r.id == id)
                {
                    self.draft_issue_menu(i, window, cx);
                }
            }
            DraftStop::File(path) => {
                if let Some(i) = draft.files.iter().position(|p| p == path) {
                    self.draft_file_menu(i, window, cx);
                }
            }
            DraftStop::Attach => self.draft_add_menu(window, cx),
            DraftStop::Edited => self.draft_edited_menu(window, cx),
            DraftStop::More => self.draft_overflow_menu(window, cx),
        }
    }

    fn draft_overflow_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = &self.home.draft;
        let entries = self
            .draft_chip_stops()
            .into_iter()
            .skip(self.home.tray_shown)
            .filter_map(|stop| {
                let (icon, label) = match &stop {
                    DraftStop::Agent => (
                        IconName::AtSign,
                        self.home
                            .draft_agent
                            .as_deref()
                            .and_then(|slug| self.agent(slug))
                            .map_or("Agent", |a| a.name.as_str())
                            .to_string(),
                    ),
                    DraftStop::Repo(name) => (IconName::FolderGit2, name.clone()),
                    DraftStop::Issue(org, id) => (
                        IconName::Tag,
                        draft
                            .issues
                            .iter()
                            .find(|i| &i.org == org && &i.id == id)?
                            .identifier
                            .clone(),
                    ),
                    DraftStop::File(path) => (
                        IconName::File,
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    _ => return None,
                };
                Some(menu::item(
                    icon,
                    label,
                    on_shark(cx, move |s, w, cx| {
                        let bounds = s.draft_anchors.borrow().get("draft-more").copied();
                        if let Some(bounds) = bounds {
                            s.draft_anchors.borrow_mut().insert(stop.anchor(), bounds);
                        }
                        s.open_draft_stop(&stop, w, cx);
                    }),
                ))
            })
            .collect();
        self.show_menu(
            self.draft_control_position("draft-more", window),
            entries,
            window,
            cx,
        );
    }

    fn remove_draft_stop(&mut self, stop: &DraftStop, window: &mut Window, cx: &mut Context<Self>) {
        if self.home.draft.starting {
            return;
        }
        if matches!(stop, DraftStop::Agent) {
            self.set_home_agent(None, window, cx);
            return;
        }
        let draft = &mut self.home.draft;
        match stop {
            DraftStop::Repo(name) => {
                draft.repos.retain(|r| &r.repo.name != name);
                draft.repos_edited = true;
            }
            DraftStop::Issue(org, id) => draft.issues.retain(|i| &i.org != org || &i.id != id),
            DraftStop::File(path) => {
                draft.files.retain(|p| p != path);
                super::task_draft::discard_staged_files(std::slice::from_ref(path));
            }
            _ => return,
        }
        self.refresh_draft_sources(cx);
        self.fit_draft_stops(self.home.tray_shown, cx);
        if self.draft_chip_stops().is_empty() {
            self.focus_draft(window, cx);
        }
    }

    fn draft_tray_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.home.tray_focus.is_focused(window) || self.home.draft.starting || self.home.asking
        {
            return;
        }
        self.fit_draft_stops(self.home.tray_shown, cx);
        let stops = self.draft_stops();
        let index = self
            .home
            .tray_stop
            .as_ref()
            .and_then(|s| stops.iter().position(|stop| stop == s))
            .unwrap_or(0);
        match event.keystroke.key.as_str() {
            "left" => self.home.tray_stop = stops.get(index.saturating_sub(1)).cloned(),
            "right" => self.home.tray_stop = stops.get((index + 1).min(stops.len() - 1)).cloned(),
            "backspace" | "delete" => self.remove_draft_stop(&stops[index], window, cx),
            "enter" | "space" => self.open_draft_stop(&stops[index], window, cx),
            "escape" | "down" | "tab" => self.focus_draft(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_task_composer(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Chip identities can change without changing any measured widths.
        if self.home.tray_focus.is_focused(window)
            && self
                .home
                .tray_stop
                .as_ref()
                .is_some_and(|stop| !self.draft_stops().contains(stop))
        {
            let target = cx.entity().downgrade();
            cx.defer(move |cx| {
                target
                    .update(cx, |s, cx| {
                        s.fit_draft_stops(s.home.tray_shown, cx);
                    })
                    .ok();
            });
        }
        let theme = cx.theme();
        let draft = &self.home.draft;
        let asking = self.home.asking;
        let input = &self.home.prompt;
        let composer = self.home.composer.clone();
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let empty = input.read(cx).value().is_empty();
        let default_agent = self
            .home
            .draft_agent
            .as_deref()
            .and_then(|slug| self.agent(slug));
        let edited = draft.settings_edited(&self.state, self.home.draft_agent.as_deref());
        let selected = |stop: &DraftStop| {
            self.home.tray_focus.is_focused(window) && self.home.tray_stop.as_ref() == Some(stop)
        };
        let quiet = self.menu.is_some() || self.draft_picker.is_some();
        let mut chips = Vec::new();
        let mut first_chip = None;
        if !asking {
            let picked = self
                .home
                .draft_agent
                .as_deref()
                .and_then(|slug| self.agent(slug));
            let label = picked.map_or("Agent", |a| a.name.as_str()).to_string();
            first_chip = Some((IconName::AtSign, label.clone()));
            let button = match picked {
                Some(a) => Button::new("home-agent-picker")
                    .ghost()
                    .flex_none()
                    .h(px(26.))
                    .min_w(px(26.))
                    .rounded(px(13.))
                    .px(px(9.))
                    .max_w(px(200.))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(super::agent_icon_view::agent_icon(&a.icon))
                            .child(div().min_w_0().truncate().child(label.clone())),
                    ),
                None => draft_chip("home-agent-picker", IconName::AtSign, label.clone())
                    .text_color(theme.muted_foreground),
            };
            let button = button
                .accessibility_label(label)
                .tooltip("Pick an agent, or type @")
                .disabled(draft.starting)
                .selected(selected(&DraftStop::Agent))
                .on_click(cx.listener(|s, _, w, cx| s.pick_home_agent(w, cx)));
            chips.push(self.anchor_draft_control(
                DraftStop::Agent.anchor(),
                crate::ui::toolbar("home-agent-picker", button),
            ));
        }
        if !asking {
            for (i, repo) in draft.repos.iter().enumerate() {
                let stop = DraftStop::Repo(repo.repo.name.clone());
                let changed = default_agent.is_some_and(|a| !a.repos.contains(&repo.repo.name));
                let label = self.draft_repo_label(repo, changed);
                first_chip.get_or_insert((IconName::FolderGit2, label.clone()));
                let branch = self.draft_branch_label(repo);
                let base = match &repo.branch {
                    BranchSpec::New { base } if !base.is_empty() => base.clone(),
                    BranchSpec::Pr(pr) => pr.base.clone(),
                    _ => self
                        .draft_bases
                        .get(&repo.repo.path)
                        .cloned()
                        .unwrap_or_else(|| "Default branch".into()),
                };
                let card = super::attach::ChipCard {
                    mark: Icon::new(IconName::FolderGit2),
                    title: repo.repo.name.clone(),
                    rows: vec![
                        ("Repository", repo.repo.path.display().to_string()),
                        ("Branch", branch.clone()),
                        ("Base", base),
                    ],
                };
                let action = stop.clone();
                let button = draft_chip(("draft-repo", i), IconName::FolderGit2, label)
                    .accessibility_label(format!(
                        "{} · {branch}{}",
                        repo.repo.name,
                        if changed {
                            ", changed from agent default"
                        } else {
                            ""
                        }
                    ))
                    .disabled(draft.starting)
                    .selected(selected(&stop))
                    .on_click(cx.listener(move |s, _, w, cx| s.open_draft_stop(&action, w, cx)));
                chips.push(card.around(
                    &format!("draft-repo-{i}"),
                    self.anchor_draft_control(
                        stop.anchor(),
                        crate::ui::toolbar(("draft-repo", i), button),
                    ),
                    quiet,
                ));
            }
            for (i, issue) in draft.issues.iter().enumerate() {
                let stop = DraftStop::Issue(issue.org.clone(), issue.id.clone());
                first_chip.get_or_insert((IconName::Tag, issue.identifier.clone()));
                let disconnected = self.linear_org_id() != Some(issue.org.as_str());
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
                let icon = super::linear_issues::status_icon(&issue.state_type, theme)
                    .text_color(theme.muted_foreground);
                let card = super::attach::ChipCard {
                    mark: icon.clone(),
                    title: issue.title.clone(),
                    rows,
                };
                let action = stop.clone();
                let button = draft_chip(("draft-issue", i), icon, issue.identifier.clone())
                    .accessibility_label(format!(
                        "{} · {}{}",
                        issue.identifier,
                        issue.title,
                        if disconnected {
                            " · Linear disconnected"
                        } else {
                            ""
                        }
                    ))
                    .disabled(draft.starting)
                    .selected(selected(&stop))
                    .on_click(cx.listener(move |s, _, w, cx| s.open_draft_stop(&action, w, cx)));
                chips.push(card.around(
                    &format!("draft-issue-{i}"),
                    self.anchor_draft_control(
                        stop.anchor(),
                        crate::ui::toolbar(("draft-issue", i), button),
                    ),
                    quiet,
                ));
            }
            for (i, path) in draft.files.iter().enumerate() {
                let stop = DraftStop::File(path.clone());
                let label = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                first_chip.get_or_insert((IconName::File, label.clone()));
                let action = stop.clone();
                let button = draft_chip(("draft-file", i), IconName::File, label)
                    .tooltip(path.display().to_string())
                    .disabled(draft.starting)
                    .selected(selected(&stop))
                    .on_click(cx.listener(move |s, _, w, cx| s.open_draft_stop(&action, w, cx)));
                chips.push(self.anchor_draft_control(
                    stop.anchor(),
                    crate::ui::toolbar(("draft-file", i), button),
                ));
            }
        }
        let (model, effort) = draft.defaults(&self.state, self.home.draft_agent.as_deref());
        let overridden = default_agent
            .is_some_and(|a| draft.saved_settings(a) != (a.model.clone(), a.effort.clone()));
        let short_model = format!(
            "{}{}",
            if overridden { "● " } else { "" },
            model_label(&model)
        );
        let full_model = format!(
            "{short_model} · {}",
            effort_label(effort.as_deref().unwrap_or(crate::models::EFFORT))
        );
        let model_button = |id: &'static str, label: String| {
            Button::new(id)
                .flex_none()
                .h(px(26.))
                .rounded(px(13.))
                .px(px(9.))
                .dropdown_caret(true)
                .label(label)
        };
        let plan_button = |id: &'static str| {
            Button::new(id)
                .xsmall()
                .ghost()
                .flex_none()
                .h(px(26.))
                .px(px(8.))
                .label(if draft.repos.is_empty() {
                    "Plan only"
                } else {
                    "Plan"
                })
                .when(draft.plan || draft.repos.is_empty(), |b| {
                    b.icon(IconName::Check)
                })
        };
        let start_button = |id: &'static str| {
            Button::new(id)
                .primary()
                .small()
                .loading(draft.starting || draft.staging > 0)
                .flex_none()
                .h(px(28.))
                .rounded(px(14.))
                .px(px(12.))
                .label(if asking {
                    "Ask ↵"
                } else if draft.staging > 0 {
                    "Adding files…"
                } else if draft.starting {
                    "Starting…"
                } else {
                    "Start ↵"
                })
        };
        let edited_button = |id: &'static str| {
            Button::new(id)
                .ghost()
                .flex_none()
                .h(px(26.))
                .rounded(px(13.))
                .px(px(9.))
                .label("● Edited")
                .dropdown_caret(true)
        };
        let density = self.home.footer_density;
        let mut footer = h_flex()
            .id("draft-tray")
            .test_support()
            .relative()
            .w_full()
            .gap_1()
            .track_focus(&self.home.tray_focus)
            .on_key_down(cx.listener(|s, e, w, cx| s.draft_tray_key(e, w, cx)));
        if !asking {
            let disconnected = draft
                .issues
                .iter()
                .filter(|i| self.linear_org_id() != Some(i.org.as_str()))
                .count();
            let indicator = (disconnected > 0).then(|| {
                let tip = format!("{disconnected} disconnected Linear issues. Opens Settings.");
                let button = Button::new("draft-disconnected")
                    .h(px(26.))
                    .min_w(px(26.))
                    .rounded(px(13.))
                    .icon(IconName::CircleDashed)
                    .accessibility_label(tip.clone())
                    .tooltip(tip)
                    .on_click(
                        cx.listener(|s, _, w, cx| crate::settings::open_linear(&s.state, w, cx)),
                    );
                crate::ui::toolbar("draft-disconnected", button).into_any_element()
            });
            let target = cx.entity().downgrade();
            let add_target = target.clone();
            let more_target = target.clone();
            let ids = self.draft_chip_stops();
            let count = ids.len();
            let row = super::attach::AttachRow {
                key: "draft-Home".into(),
                attach_id: "draft-add".into(),
                more_id: "draft-more".into(),
                height: px(26.),
                chips,
                failed: 0,
                disconnected: indicator,
                linear: true,
                disabled: draft.starting,
                attach_focused: selected(&DraftStop::Attach),
                more_focused: selected(&DraftStop::More),
                attach_tooltip: "Attach (⌘I)".into(),
                on_attach: Rc::new(move |bounds, w, cx| {
                    add_target
                        .update(cx, |s, cx| {
                            s.draft_anchors
                                .borrow_mut()
                                .insert("draft-add".into(), bounds);
                            s.draft_add_menu(w, cx);
                        })
                        .ok();
                }),
                on_more: Rc::new(move |shown, bounds, w, cx| {
                    more_target
                        .update(cx, |s, cx| {
                            s.home.tray_shown = shown;
                            s.draft_anchors
                                .borrow_mut()
                                .insert("draft-more".into(), bounds);
                            s.draft_overflow_menu(w, cx);
                        })
                        .ok();
                }),
                on_bounds: Some({
                    let anchors = self.draft_anchors.clone();
                    Rc::new(move |id, bounds, _| {
                        anchors.borrow_mut().insert(id.to_string(), bounds);
                    })
                }),
                on_fit: Some(Rc::new(move |shown, _, cx| {
                    target
                        .update(cx, |s, cx| {
                            if s.draft_chip_stops() == ids {
                                s.fit_draft_stops(shown, cx);
                            }
                        })
                        .ok();
                })),
            };
            footer = footer.child(row);
            if edited {
                let button = edited_button("draft-edited")
                    .disabled(draft.starting)
                    .selected(selected(&DraftStop::Edited))
                    .tooltip("Changes from agent defaults")
                    .on_click(cx.listener(|s, _, w, cx| s.draft_edited_menu(w, cx)));
                footer = footer.child(self.anchor_draft_control(
                    "draft-edited".into(),
                    crate::ui::toolbar("draft-edited", button),
                ));
            }
            let source = self.draft_settings_source();
            footer = footer.child(div().flex_1().min_w_0()).child(
                model_button(
                    "draft-model",
                    if density == 0 {
                        full_model.clone()
                    } else {
                        short_model.clone()
                    },
                )
                .when(density >= 2, |b| b.flex_shrink_1().min_w(px(48.)))
                .disabled(draft.starting)
                .accessibility_label(format!("{full_model}, {source}"))
                .tooltip(format!("{source} · Model and thinking for this task (⌘/)"))
                .on_click(cx.listener(|s, _, w, cx| s.draft_model_menu(w, cx))),
            );
            if density < 2 {
                footer = footer.child(
                    plan_button("draft-plan")
                        .disabled(draft.starting || draft.repos.is_empty())
                        .tooltip(self.draft_plan_tooltip())
                        .on_click(cx.listener(|s, _, _, cx| s.toggle_draft_plan(cx))),
                );
            }
            // Measure canonical controls even when shortened, so resizing back cannot oscillate.
            let target = cx.entity().downgrade();
            let attach_bounds = self.draft_anchors.clone();
            let probe = h_flex()
                .absolute()
                .left_0()
                .top_0()
                .w_full()
                .invisible()
                .gap_1()
                .child(canvas(|_, _, _| {}, |_, _, _, _| {}).absolute().size_full())
                .child(match first_chip {
                    Some((icon, label)) => {
                        draft_chip("draft-probe-first", icon, label).into_any_element()
                    }
                    None => div().w_0().into_any_element(),
                })
                .child(model_button("draft-probe-full", full_model))
                .child(model_button("draft-probe-short", short_model))
                .child(plan_button("draft-probe-plan"))
                .child(start_button("draft-probe-start"))
                .child(if edited {
                    edited_button("draft-probe-edited").into_any_element()
                } else {
                    div().w_0().into_any_element()
                })
                .on_children_prepainted(move |b, _, cx| {
                    if b.len() != 7 {
                        return;
                    }
                    let left = if count == 0 {
                        attach_bounds
                            .borrow()
                            .get("draft-add")
                            .map_or(px(32.), |b| b.size.width)
                            + px(4.)
                    } else {
                        px(32.)
                            + b[1].size.width
                            + px(4.)
                            + if count > 1 { px(72.) } else { px(0.) }
                            + if disconnected > 0 { px(36.) } else { px(0.) }
                    };
                    let fixed = left
                        + b[5].size.width
                        + b[6].size.width
                        + px(if edited { 20. } else { 16. });
                    let next = if fixed + b[2].size.width + b[4].size.width <= b[0].size.width {
                        0
                    } else if fixed + b[3].size.width + b[4].size.width <= b[0].size.width {
                        1
                    } else {
                        2
                    };
                    if next != density {
                        let target = target.clone();
                        cx.defer(move |cx| {
                            target
                                .update(cx, |s, cx| {
                                    if s.home.footer_density != next {
                                        s.home.footer_density = next;
                                        cx.notify();
                                    }
                                })
                                .ok();
                        });
                    }
                });
            footer = footer.child(probe);
        } else {
            footer = footer.child(div().flex_1());
        }
        footer = footer.child(
            start_button("draft-start")
                .disabled(draft.starting || draft.staging > 0)
                .on_click(cx.listener(|s, _, w, cx| s.submit_prompt(w, cx))),
        );
        let hits = self.mentions(cx);
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
                    .id("home-composer")
                    .test_support()
                    .key_context("HomePrompt")
                    .on_action(cx.listener(|s, _: &PickModel, w, cx| s.draft_model_menu(w, cx)))
                    .on_action(cx.listener(|s, _: &AddToDraft, w, cx| s.draft_add_menu(w, cx)))
                    .on_action(
                        cx.listener(|s, _: &ChangeDraftBranch, w, cx| s.change_draft_branch(w, cx)),
                    )
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
                    .child(footer),
            )
            .when(focused && empty, |d| {
                d.child(
                    h_flex()
                        .id("draft-hints")
                        .test_support()
                        .gap_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(crate::ui::hint("shift-enter", "new line"))
                        .when(!asking, |d| {
                            d.child(crate::ui::hint("@", "agent / file"))
                                .child(crate::ui::hint("#", "issue or PR"))
                                .child(crate::ui::hint("/", "skill"))
                        })
                        .child(crate::ui::hint(
                            "tab",
                            if asking { "new task" } else { "ask Shark" },
                        )),
                )
            })
            .into_any_element()
    }
}
