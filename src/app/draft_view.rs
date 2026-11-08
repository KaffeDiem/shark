//! Shared Home and agent-page draft controls. Pickers only change the draft.
use super::home::{Destination, agent_prompt};
use super::task_draft::{BranchSpec, DraftRepo, TaskDraft};
use super::*;
use crate::models::{effort_label, efforts, model_label};
use crate::task_composer::{Event as ComposerEvent, Source};
use gpui_kit::component::Disableable as _;

impl Shark {
    fn draft(&self, agent: bool) -> &TaskDraft {
        if agent {
            &self.home.agent_draft
        } else {
            &self.home.draft
        }
    }

    fn draft_mut(&mut self, agent: bool) -> &mut TaskDraft {
        if agent {
            &mut self.home.agent_draft
        } else {
            &mut self.home.draft
        }
    }

    fn draft_slug(&self, agent: bool) -> Option<&str> {
        if agent {
            self.home.agent_prompt_for.as_deref()
        } else {
            self.home.draft_agent.as_deref()
        }
    }

    fn draft_target(&self) -> bool {
        matches!(self.home.destination, Destination::Agent(_))
    }

    pub(super) fn refresh_draft_defaults(&mut self, slug: &str, cx: &mut Context<Self>) {
        let defaults = TaskDraft::for_agent(&self.state, Some(slug));
        if self.home.agent_prompt_for.as_deref() == Some(slug)
            && !self.home.agent_draft.repos_edited
        {
            self.home.agent_draft.repos = defaults.repos.clone();
            self.refresh_draft_sources(true, cx);
        }
        if self.home.draft_agent.as_deref() == Some(slug) && !self.home.draft.repos_edited {
            self.home.draft.repos = defaults.repos.clone();
            self.refresh_draft_sources(false, cx);
        }
        if let Some(draft) = self.home.agent_drafts.get_mut(slug)
            && !draft.repos_edited
        {
            draft.repos = defaults.repos;
        }
    }

    pub(super) fn refresh_draft_sources(&mut self, agent: bool, cx: &mut Context<Self>) {
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
                let draft = self.draft(agent);
                if !draft.issues.is_empty() || !draft.repos.is_empty() || !draft.files.is_empty() {
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

    fn focus_draft(&mut self, agent: bool, window: &mut Window, cx: &mut Context<Self>) {
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

    fn draft_at_mut(&mut self, destination: &Destination) -> Option<&mut TaskDraft> {
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
        let destination = self.home.destination.clone();
        self.pick_draft_repos(
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
            window,
            cx,
        );
    }

    fn draft_repo_menu(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.draft_target();
        let Some(repo) = self.draft(agent).repos.get(index).cloned() else {
            return;
        };
        let destination = self.home.destination.clone();
        let name = repo.repo.name.clone();
        let pos = window.mouse_position();
        cx.spawn_in(window, async move |this, cx| {
            let base = cx
                .background_executor()
                .spawn(async move {
                    let base = match &repo.branch {
                        BranchSpec::New { base } => base.as_str(),
                        _ => "",
                    };
                    git::resolve_base(&repo.repo.path, base)
                })
                .await;
            this.update_in(cx, |s, w, cx| {
                if s.home.destination == destination
                    && !s.draft(agent).starting
                    && s.draft(agent)
                        .repos
                        .get(index)
                        .is_some_and(|r| r.repo.name == name)
                {
                    s.show_draft_repo_menu(index, base, pos, w, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn show_draft_repo_menu(
        &mut self,
        index: usize,
        base: String,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.draft_target();
        let repo = self.draft(agent).repos[index].clone();
        let name = repo.repo.name.clone();
        let hint = format!("From {base}");
        let name_pr = name.clone();
        let entries = vec![
            menu::item(
                None,
                "New branch",
                on_shark(cx, move |s, w, cx| {
                    s.draft_mut(agent).repos[index].branch = BranchSpec::New { base: base.clone() };
                    s.draft_mut(agent).repos_edited = true;
                    s.focus_draft(agent, w, cx);
                }),
            )
            .checked(matches!(repo.branch, BranchSpec::New { .. }))
            .hint(hint),
            menu::item(
                None,
                "Existing branch…",
                on_shark(cx, move |s, w, cx| {
                    s.pick_draft_branch(Some((name.clone(), false)), w, cx)
                }),
            ),
            menu::item(
                None,
                "Pull request…",
                on_shark(cx, move |s, w, cx| {
                    s.pick_draft_branch(Some((name_pr.clone(), true)), w, cx)
                }),
            ),
            Entry::Sep,
            menu::item(
                IconName::X,
                "Remove",
                on_shark(cx, move |s, w, cx| {
                    s.draft_mut(agent).repos.remove(index);
                    s.draft_mut(agent).repos_edited = true;
                    s.focus_draft(agent, w, cx);
                }),
            ),
        ];
        self.show_menu(pos, entries, window, cx);
    }

    fn draft_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut entries = vec![];
        if self.linear_in_use() {
            entries.push(menu::item(
                IconName::TagPlus,
                "Linear issue…",
                on_shark(cx, |s, w, cx| s.start_from_issue(w, cx)),
            ));
        }
        entries.push(menu::item(
            IconName::GitBranch,
            "Branch or pull request…",
            on_shark(cx, |s, w, cx| s.pick_draft_branch(None, w, cx)),
        ));
        entries.push(menu::item(
            IconName::FolderGit2,
            "Repository…",
            on_shark(cx, |s, w, cx| s.draft_repository_menu(w, cx)),
        ));
        entries.push(menu::item(
            IconName::Paperclip,
            "File…",
            on_shark(cx, |s, w, cx| s.pick_draft_files(w, cx)),
        ));
        self.show_menu(window.mouse_position(), entries, window, cx);
    }

    fn draft_repository_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.repos.is_empty() {
            return crate::settings::open_repos(&self.state, window, cx);
        }
        let agent = self.draft_target();
        let rows = self
            .state
            .repos
            .iter()
            .map(|repo| {
                let repo = repo.clone();
                let has = self
                    .draft(agent)
                    .repos
                    .iter()
                    .any(|r| r.repo.name == repo.name);
                menu::item(
                    IconName::FolderGit2,
                    repo.name.clone(),
                    on_shark(cx, move |s, w, cx| {
                        let draft = s.draft_mut(agent);
                        if !draft.repos.iter().any(|r| r.repo.name == repo.name) {
                            draft.repos.push(DraftRepo {
                                repo: repo.clone(),
                                branch: BranchSpec::New {
                                    base: String::new(),
                                },
                            });
                            draft.repos_edited = true;
                        }
                        s.focus_draft(agent, w, cx);
                    }),
                )
                .checked(has)
            })
            .collect();
        self.show_menu(window.mouse_position(), rows, window, cx);
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
        if self.draft(agent).model.is_some() || self.draft(agent).effort.is_some() {
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
                    s.refresh_draft_sources(agent, cx);
                    cx.notify();
                }),
            ));
        }
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

    fn pick_draft_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let (issues, repos) = (draft.issues.len(), draft.repos.len());
        let count = issues + repos + draft.files.len();
        if count == 0 {
            self.focus_draft(agent, window, cx);
            return;
        }
        let index = self.home.tray_index.min(count - 1);
        match event.keystroke.key.as_str() {
            "left" => self.home.tray_index = index.saturating_sub(1),
            "right" => self.home.tray_index = (index + 1).min(count - 1),
            "backspace" | "delete" => {
                let draft = self.draft_mut(agent);
                if index < issues {
                    draft.issues.remove(index);
                } else if index < issues + repos {
                    draft.repos.remove(index - issues);
                    draft.repos_edited = true;
                } else {
                    let removed = draft.files.remove(index - issues - repos);
                    super::task_draft::discard_staged_files(&[removed]);
                }
                self.home.tray_index = index.min(count.saturating_sub(2));
                self.refresh_draft_sources(agent, cx);
                if count == 1 {
                    self.focus_draft(agent, window, cx);
                }
            }
            "enter" if index >= issues && index < issues + repos => {
                self.draft_repo_menu(index - issues, window, cx)
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
                    BranchSpec::New { .. } => "new branch".into(),
                    BranchSpec::Existing(b) => b.clone(),
                    BranchSpec::Pr(pr) => format!("PR #{}", pr.number),
                };
                pills.push(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(("draft-repo", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::GitBranch)
                                .label(format!("{} · {branch}", repo.repo.name))
                                .disabled(draft.starting)
                                .on_click(
                                    cx.listener(move |s, _, w, cx| s.draft_repo_menu(i, w, cx)),
                                ),
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
                            div().text_xs().child(
                                path.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
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
            let overridden = (model.clone(), effort.clone())
                != draft.inherited_defaults(&self.state, self.draft_slug(agent));
            let label = format!(
                "{}{} · {}",
                if overridden { "● " } else { "" },
                model_label(&model),
                effort_label(effort.as_deref().unwrap_or(crate::models::EFFORT))
            );
            footer = footer
                .child(
                    Button::new("draft-add")
                        .xsmall()
                        .ghost()
                        .icon(IconName::Plus)
                        .accessibility_label("Add to task")
                        .tooltip("Add to task")
                        .disabled(draft.starting)
                        .on_click(cx.listener(|s, _, w, cx| s.draft_add_menu(w, cx))),
                )
                .child(
                    Button::new("draft-model")
                        .xsmall()
                        .label(label)
                        .disabled(draft.starting)
                        .tooltip("Model and thinking for this task (⌘/)")
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
                    .when(!pills.is_empty(), |d| {
                        d.child(
                            h_flex()
                                .id("draft-tray")
                                .track_focus(&self.home.tray_focus)
                                .on_key_down(cx.listener(|s, e, w, cx| s.draft_tray_key(e, w, cx)))
                                .gap_2()
                                .flex_wrap()
                                .children(pills.into_iter().enumerate().map(|(i, pill)| {
                                    div()
                                        .rounded(theme.radius)
                                        .border_1()
                                        .border_color(
                                            if self.home.tray_focus.is_focused(window)
                                                && self.home.tray_index == i
                                            {
                                                theme.primary
                                            } else {
                                                theme.border
                                            },
                                        )
                                        .child(pill)
                                })),
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
                        .child(crate::ui::hint("#", "PR"))
                        .child(crate::ui::hint("/", "skill"))
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
