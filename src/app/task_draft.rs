//! The complete task context stays in the composer until creation succeeds.
use super::*;
use crate::state::{AgentLink, TaskRepo};

#[derive(Clone, PartialEq, Eq)]
pub(super) enum BranchSpec {
    New { base: String },
    Existing(String),
    Pr(git::PullRequest),
}

#[derive(Clone)]
pub(super) struct DraftRepo {
    pub repo: Repo,
    pub branch: BranchSpec,
    pub pr_label: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct TaskDraft {
    pub text: String,
    pub content: Option<gpui_kit::component::input::InputContent>,
    pub repos: Vec<DraftRepo>,
    pub issues: Vec<Snapshot>,
    pub files: Vec<PathBuf>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Bypass agent defaults so saving can restore repository/app inheritance.
    pub follow_app: bool,
    pub plan: bool,
    pub repos_edited: bool,
    pub starting: bool,
    pub staging: usize,
}

pub(super) type PickRepos =
    Rc<dyn Fn(&mut Shark, Vec<DraftRepo>, &mut Window, &mut Context<Shark>)>;
pub(super) type PickIssues =
    Rc<dyn Fn(&mut Shark, Vec<Snapshot>, &mut Window, &mut Context<Shark>)>;
pub(super) type Finished = Rc<dyn Fn(&mut Shark, bool, &mut Window, &mut Context<Shark>)>;

impl TaskDraft {
    pub fn for_agent(state: &State, slug: Option<&str>) -> Self {
        let agent = slug.and_then(|slug| state.agents.iter().find(|a| a.slug == slug));
        Self {
            repos: agent
                .into_iter()
                .flat_map(|a| &a.repos)
                .filter_map(|name| {
                    state
                        .repos
                        .iter()
                        .find(|r| &r.name == name)
                        .cloned()
                        .map(|repo| DraftRepo {
                            repo,
                            pr_label: None,
                            branch: BranchSpec::New {
                                base: String::new(),
                            },
                        })
                })
                .collect(),
            ..Default::default()
        }
    }

    pub fn defaults(&self, state: &State, slug: Option<&str>) -> (String, Option<String>) {
        self.resolve_defaults(state, slug, true)
    }

    pub fn inherited_defaults(
        &self,
        state: &State,
        slug: Option<&str>,
    ) -> (String, Option<String>) {
        self.resolve_defaults(state, slug, false)
    }

    fn resolve_defaults(
        &self,
        state: &State,
        slug: Option<&str>,
        overrides: bool,
    ) -> (String, Option<String>) {
        let task = Task {
            agent: slug
                .filter(|_| !(overrides && self.follow_app))
                .map(|slug| AgentLink {
                    slug: slug.into(),
                    ..Default::default()
                }),
            repos: self
                .repos
                .iter()
                .map(|r| TaskRepo {
                    name: r.repo.name.clone(),
                    src: r.repo.path.clone(),
                    base: String::new(),
                    branch: None,
                })
                .collect(),
            ..Default::default()
        };
        let (model, effort) = state.defaults_for(&task);
        let model = self.model.clone().filter(|_| overrides).unwrap_or(model);
        let effort = self.effort.clone().filter(|_| overrides).or(effort);
        let resolved = crate::models::effort_of(&ChatMeta {
            model: Some(model.clone()),
            effort,
            ..Default::default()
        });
        (model, Some(resolved.to_string()))
    }

    pub fn saved_settings(&self, agent: &crate::state::Agent) -> (Option<String>, Option<String>) {
        if self.follow_app {
            (self.model.clone(), self.effort.clone())
        } else {
            (
                self.model.clone().or_else(|| agent.model.clone()),
                self.effort.clone().or_else(|| agent.effort.clone()),
            )
        }
    }

    pub fn settings_edited(&self, state: &State, slug: Option<&str>) -> bool {
        let Some(agent) = slug.and_then(|slug| state.agents.iter().find(|a| a.slug == slug)) else {
            return false;
        };
        !self
            .repos
            .iter()
            .map(|r| &r.repo.name)
            .eq(agent.repos.iter())
            || self.saved_settings(agent) != (agent.model.clone(), agent.effort.clone())
    }

    /// Header edits apply to future drafts; keep the current task's effective choices.
    pub fn preserve_defaults(
        &mut self,
        state: &State,
        slug: &str,
        before: (String, Option<String>),
        changed: (bool, bool),
    ) {
        if changed.0 && self.defaults(state, Some(slug)).0 != before.0 {
            self.model = Some(before.0);
        }
        if (changed.0 || changed.1) && self.defaults(state, Some(slug)).1 != before.1 {
            self.effort = before.1;
        }
    }

    fn first(&self) -> Option<String> {
        let text = self.text.trim();
        if text.is_empty() {
            self.issues
                .first()
                .map(super::linear_issues::planning_request)
        } else {
            Some(text.to_string())
        }
    }
}

/// Capture transient clipboard files before their source disappears. Uses chat's copy rules.
pub(super) fn stage_files(
    paths: Vec<PathBuf>,
    image: Option<(Vec<u8>, &'static str)>,
) -> Result<Vec<PathBuf>, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "shark-draft-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
    let copied = paths
        .iter()
        .map(|p| complete::attach(&dir, p))
        .chain(
            image.map(|(b, ext)| complete::attach_bytes(&dir, &format!("Pasted image.{ext}"), &b)),
        )
        .collect::<Result<Vec<_>, _>>();
    match copied {
        Ok(paths) => Ok(paths.into_iter().map(|p| dir.join(p)).collect()),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(error)
        }
    }
}

pub(super) fn discard_staged_files(paths: &[PathBuf]) {
    let temp = std::env::temp_dir();
    for path in paths {
        let Some(root) = path.ancestors().nth(3) else {
            continue;
        };
        if root.parent() != Some(temp.as_path())
            || !root
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("shark-draft-"))
        {
            continue;
        }
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
            if let Some(parent) = parent.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
        let _ = std::fs::remove_dir(root);
    }
}

impl Shark {
    pub(super) fn start_task_draft(
        &mut self,
        draft: TaskDraft,
        agent: Option<AgentLink>,
        finished: Finished,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let error = if self.state.read_only {
            Some("Can't create tasks: state.json couldn't be read")
        } else if agent
            .as_ref()
            .is_some_and(|link| !self.state.agents.iter().any(|a| a.slug == link.slug))
        {
            Some("This agent was deleted. Choose an agent again.")
        } else {
            None
        };
        if let Some(error) = error {
            notify_err(window, cx, error);
            finished(self, false, window, cx);
            return;
        }
        let title = draft
            .issues
            .first()
            .map(|s| crate::chat::title_of(&s.title))
            .unwrap_or_else(|| crate::chat::title_of(&draft.text));
        let source = draft
            .issues
            .first()
            .map(|s| s.identifier.as_str())
            .unwrap_or(&title);
        let source = crate::state::slug(source);
        let prefix = agent
            .as_ref()
            .map(|a| format!("{}/", a.slug))
            .unwrap_or_default();
        let prefix = format!(
            "{prefix}{}",
            if source.is_empty() { "task" } else { &source }
        );
        let with_repos = !draft.repos.is_empty();
        let sea = crate::state::task_name(interaction::now_ms(), |sea| {
            let name = if with_repos {
                format!("{prefix}-{sea}").replace('/', "-")
            } else {
                sea.to_string()
            };
            self.creating.contains_key(&name)
                || self.task(&name).is_some()
                || tasks_dir().join(&name).exists()
                || crate::transcript::transcript_path(&name).exists()
        });
        let branch = format!("{prefix}-{sea}");
        let name = if with_repos {
            branch.replace('/', "-")
        } else {
            sea
        };
        let (model, effort) = draft.defaults(&self.state, agent.as_ref().map(|a| a.slug.as_str()));
        self.creating
            .insert(name.clone(), agent.as_ref().map(|a| a.slug.clone()));
        let pending = name.clone();
        let staged_files = draft.files.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    create_draft(&draft, agent, &name, &branch, &title, &model, effort)
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.creating.remove(&pending);
                let (ws, prompt) = match result {
                    Ok(result) => result,
                    Err(e) => {
                        notify_err(window, cx, e);
                        finished(s, false, window, cx);
                        cx.notify();
                        return;
                    }
                };
                s.state.tasks.push(ws.clone());
                let previous_repos = std::mem::replace(
                    &mut s.state.last_repos,
                    ws.repos.iter().map(|r| r.name.clone()).collect(),
                );
                if let Err(e) = s.state.save() {
                    s.state.tasks.pop();
                    s.state.last_repos = previous_repos;
                    let cleanup = git::remove_task(&ws, &ws.dir(), false);
                    crate::linear::delete_snapshots(&ws.name);
                    let suffix = cleanup
                        .err()
                        .map(|e| format!("; cleanup: {e}"))
                        .unwrap_or_default();
                    notify_err(
                        window,
                        cx,
                        format!("Couldn't save the new task: {e}{suffix}"),
                    );
                    finished(s, false, window, cx);
                    cx.notify();
                    return;
                }
                if let Some(agent) = &ws.agent {
                    let docs = s
                        .state
                        .agents
                        .iter()
                        .find(|a| a.slug == agent.slug)
                        .is_some_and(|a| {
                            a.name != agent.name || a.instructions != agent.instructions
                        });
                    if let Err(e) = s.sync_agent(&agent.slug, docs, cx) {
                        notify_err(window, cx, e);
                    }
                    s.refresh_agent_files(&agent.slug, cx);
                }
                s.record_interaction(&ws.name, cx);
                let ws = s.task(&ws.name).cloned().unwrap_or(ws);
                let chat = s.ensure_chat(&ws, &ws.chats[0], window, cx);
                let then = prompt.map(|p| (p, chat));
                if !s.run_setup(&ws, None, then.clone(), window, cx)
                    && let Some((p, chat)) = then
                {
                    chat.update(cx, |c, cx| {
                        if !c.ask(&p, window, cx) {
                            c.composer.update(cx, |t, cx| t.set_value(p, window, cx));
                        }
                    });
                }
                discard_staged_files(&staged_files);
                finished(s, true, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn create_draft(
    draft: &TaskDraft,
    agent: Option<AgentLink>,
    name: &str,
    branch: &str,
    title: &str,
    model: &str,
    effort: Option<String>,
) -> Result<(Task, Option<String>), String> {
    let dir = tasks_dir().join(name);
    let repos: Vec<_> = draft
        .repos
        .iter()
        .map(|r| {
            (
                r.repo.clone(),
                match &r.branch {
                    BranchSpec::Existing(branch) => branch.clone(),
                    _ => branch.to_string(),
                },
            )
        })
        .collect();
    let prs: Vec<_> = draft
        .repos
        .iter()
        .filter_map(|r| match &r.branch {
            BranchSpec::Pr(pr) => Some((r.repo.name.clone(), pr.clone())),
            _ => None,
        })
        .collect();
    let bases: Vec<_> = draft
        .repos
        .iter()
        .filter_map(|r| match &r.branch {
            BranchSpec::New { base } if !base.is_empty() => {
                Some((r.repo.name.clone(), base.clone()))
            }
            _ => None,
        })
        .collect();
    let fresh: Vec<_> = draft
        .repos
        .iter()
        .filter(|r| matches!(r.branch, BranchSpec::New { .. }))
        .map(|r| r.repo.name.clone())
        .collect();
    let mut ws = if repos.is_empty() {
        git::create_empty_task(&dir, name, title, agent)?
    } else {
        git::create_task_with_sources(
            &dir, name, branch, &repos, &prs, &bases, title, agent, &fresh,
        )?
    };
    let prepared = (|| {
        for snapshot in &draft.issues {
            crate::linear::put_snapshot(name, snapshot).map_err(|e| e.to_string())?;
            if ws.linear.attach(snapshot.into()) {
                ws.issues.push(snapshot.clone());
            }
        }
        let mut prompt = draft.first().unwrap_or_default();
        for path in &draft.files {
            let path = complete::attach(&dir, path)?;
            if !prompt.is_empty() {
                prompt.push('\n');
            }
            prompt.push_str(&complete::mention(&path));
        }
        ws.fill_models(model, effort.as_ref());
        ws.chats[0].plan = draft.plan || repos.is_empty();
        Ok::<_, String>((ws.clone(), (!prompt.is_empty()).then_some(prompt)))
    })();
    match prepared {
        Ok(result) => Ok(result),
        Err(e) => {
            let cleanup = git::remove_task(&ws, &dir, false);
            crate::linear::delete_snapshots(name);
            match cleanup {
                Ok(_) => Err(e),
                Err(cleanup) => Err(format!("{e}; cleanup: {cleanup}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[core::prelude::v1::test]
    fn typed_text_wins_over_issue_planning_request() {
        let mut draft = TaskDraft {
            issues: vec![Snapshot {
                identifier: "ENG-412".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(draft.first().unwrap().contains("ENG-412"));
        draft.text = "Keep my exact request".into();
        assert_eq!(draft.first().as_deref(), Some("Keep my exact request"));
        draft.issues.clear();
        draft.text.clear();
        assert_eq!(draft.first(), None);
    }
    #[core::prelude::v1::test]
    fn stage_captures_files_and_only_discards_its_copies() {
        let source =
            std::env::temp_dir().join(format!("shark-draft-source-{}.txt", std::process::id()));
        std::fs::write(&source, "captured").unwrap();
        let staged = stage_files(vec![source.clone()], Some((vec![1, 2, 3], "png"))).unwrap();
        std::fs::write(&source, "changed").unwrap();
        assert_eq!(std::fs::read_to_string(&staged[0]).unwrap(), "captured");
        assert_eq!(std::fs::read(&staged[1]).unwrap(), [1, 2, 3]);
        discard_staged_files(&staged);
        assert!(staged.iter().all(|p| !p.exists()));
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "changed");
        discard_staged_files(&[source.clone()]);
        assert!(source.exists());
        std::fs::remove_file(source).unwrap();
    }

    #[core::prelude::v1::test]
    fn repository_order_changes_are_saveable_settings() {
        let state = State {
            agents: vec![crate::state::Agent {
                slug: "worker".into(),
                repos: vec!["api".into(), "web".into()],
                ..Default::default()
            }],
            repos: ["api", "web"]
                .map(|name| Repo {
                    name: name.into(),
                    path: "/unused".into(),
                })
                .to_vec(),
            ..Default::default()
        };
        let mut draft = TaskDraft::for_agent(&state, Some("worker"));
        assert!(!draft.settings_edited(&state, Some("worker")));
        draft.repos.swap(0, 1);
        assert!(draft.settings_edited(&state, Some("worker")));
        draft.repos.swap(0, 1);
        assert!(!draft.settings_edited(&state, Some("worker")));
    }

    #[core::prelude::v1::test]
    fn saveable_settings_exclude_task_context_and_preserve_inheritance() {
        let mut state = State {
            last_model: Some("sonnet".into()),
            agents: vec![crate::state::Agent {
                slug: "worker".into(),
                model: Some("opus".into()),
                effort: Some("high".into()),
                repos: vec!["api".into()],
                ..Default::default()
            }],
            repos: vec![Repo {
                name: "api".into(),
                path: "/unused".into(),
            }],
            ..Default::default()
        };
        let mut draft = TaskDraft::for_agent(&state, Some("worker"));
        draft.repos[0].branch = BranchSpec::Existing("fix".into());
        draft.files.push("notes.txt".into());
        draft.issues.push(Snapshot::default());
        draft.plan = true;
        assert!(!draft.settings_edited(&state, Some("worker")));
        draft.follow_app = true;
        assert_eq!(draft.defaults(&state, Some("worker")).0, "sonnet");
        assert_eq!(draft.saved_settings(&state.agents[0]), (None, None));
        assert!(draft.settings_edited(&state, Some("worker")));
        state.agents[0].model = None;
        state.agents[0].effort = None;
        assert!(!draft.settings_edited(&state, Some("worker")));
        draft.repos.clear();
        assert!(draft.settings_edited(&state, Some("worker")));
        assert!(!draft.settings_edited(&state, None));
    }

    #[core::prelude::v1::test]
    fn overrides_resolve_without_changing_agent_defaults() {
        let state = State {
            agents: vec![crate::state::Agent {
                slug: "worker".into(),
                model: Some("opus".into()),
                effort: Some("high".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut draft = TaskDraft::for_agent(&state, Some("worker"));
        assert_eq!(
            draft.defaults(&state, Some("worker")),
            ("opus".into(), Some("high".into()))
        );
        draft.model = Some("codex:gpt-6.1-sol".into());
        draft.effort = Some("ultra".into());
        assert_eq!(
            draft.defaults(&state, Some("worker")),
            ("codex:gpt-6.1-sol".into(), Some("ultra".into()))
        );
        draft.model = Some("opus".into());
        assert_eq!(
            draft.defaults(&state, Some("worker")).1.as_deref(),
            Some("max")
        );
        assert_eq!(state.agents[0].effort.as_deref(), Some("high"));
    }
    #[core::prelude::v1::test]
    fn header_defaults_change_future_drafts_without_changing_existing_ones() {
        let mut state = State {
            agents: vec![crate::state::Agent {
                slug: "worker".into(),
                model: Some("opus".into()),
                effort: Some("high".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut draft = TaskDraft::for_agent(&state, Some("worker"));
        draft.text = "Existing task".into();
        let before = draft.defaults(&state, Some("worker"));
        state.agents[0].model = Some("codex:gpt-6.1-sol".into());
        state.agents[0].effort = Some("ultra".into());
        draft.preserve_defaults(&state, "worker", before.clone(), (true, true));
        assert_eq!(draft.defaults(&state, Some("worker")), before);
        assert_eq!(draft.text, "Existing task");
        assert_eq!(
            draft.inherited_defaults(&state, Some("worker")),
            ("codex:gpt-6.1-sol".into(), Some("ultra".into()))
        );
        let fresh = TaskDraft::for_agent(&state, Some("worker"));
        assert_eq!(
            fresh.defaults(&state, Some("worker")),
            draft.inherited_defaults(&state, Some("worker"))
        );

        let mut unchanged = fresh;
        let before = unchanged.defaults(&state, Some("worker"));
        unchanged.preserve_defaults(&state, "worker", before, (true, true));
        assert!(
            unchanged.model.is_none() && unchanged.effort.is_none(),
            "No overrides when effective values are unchanged"
        );
    }

    #[core::prelude::v1::test]
    fn header_model_change_preserves_thinking_after_agent_normalization() {
        let mut state = State {
            agents: vec![crate::state::Agent {
                slug: "worker".into(),
                model: Some("codex:gpt-6.1-sol".into()),
                effort: Some("ultra".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut draft = TaskDraft::for_agent(&state, Some("worker"));
        let before = draft.defaults(&state, Some("worker"));
        state.agents[0].model = Some("opus".into());
        state.agents[0].effort = None;
        draft.preserve_defaults(&state, "worker", before.clone(), (true, true));
        assert_eq!(draft.defaults(&state, Some("worker")), before);
    }

    #[cfg(feature = "test-support")]
    #[core::prelude::v1::test]
    fn combined_context_and_plan_are_ready_before_first_send() {
        let _guard = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = super::super::ui_tests::test_home();
        let path = home.join("source");
        super::super::ui_tests::init_repo(&path);
        let mut draft = TaskDraft {
            text: "Implement this carefully".into(),
            repos: vec![DraftRepo {
                pr_label: None,
                repo: Repo {
                    name: "api".into(),
                    path,
                },
                branch: BranchSpec::New {
                    base: "main".into(),
                },
            }],
            issues: vec![Snapshot {
                org: "org".into(),
                id: "issue".into(),
                identifier: "ENG-412".into(),
                title: "Issue title".into(),
                ..Default::default()
            }],
            plan: true,
            ..Default::default()
        };
        let (task, prompt) = create_draft(
            &draft,
            None,
            "draft-context",
            "eng-412-draft-context",
            "Issue title",
            "codex:gpt-6.1-sol",
            Some("medium".into()),
        )
        .unwrap();
        assert_eq!(prompt.as_deref(), Some("Implement this carefully"));
        assert_eq!(task.primary_issue().unwrap().identifier, "ENG-412");
        assert_eq!(task.repos.len(), 1);
        assert!(task.chats[0].plan);
        assert_eq!(task.chats[0].model.as_deref(), Some("codex:gpt-6.1-sol"));
        assert_eq!(task.chats[0].effort.as_deref(), Some("medium"));
        assert!(
            task.context()
                .keys()
                .any(|k| k.starts_with(crate::state::ISSUE_CONTEXT))
        );
        git::remove_task(&task, &task.dir(), false).unwrap();
        crate::linear::delete_snapshots(&task.name);
        draft.repos.clear();
        draft.issues.clear();
        draft.text.clear();
        draft.plan = false;
        let (empty, prompt) =
            create_draft(&draft, None, "draft-empty", "draft-empty", "", "opus", None).unwrap();
        assert!(empty.chats[0].plan, "No repositories always locks Plan on");
        assert!(prompt.is_none(), "Empty text starts an empty chat");
        git::remove_task(&empty, &empty.dir(), false).unwrap();
    }
    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn draft_creation_reconciles_agent_edits_before_completion(cx: &mut TestAppContext) {
        use gpui_kit::test::TestAppContextExt;
        use std::time::Duration;
        let _guard = super::super::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::super::ui_tests::test_home();
        let agent = crate::state::Agent {
            slug: "worker".into(),
            name: "Old name".into(),
            instructions: "Old instructions".into(),
            ..Default::default()
        };
        let (window, shark) = super::super::ui_tests::boot(
            cx,
            State {
                agents: vec![agent.clone()],
                ..Default::default()
            },
            WindowOptions::default(),
        );
        cx.update_window(window, |_, window, cx| {
            shark.update(cx, |s, cx| {
                s.start_task_draft(
                    TaskDraft::default(),
                    Some(AgentLink::from(&agent)),
                    Rc::new(|_, _, _, _| {}),
                    window,
                    cx,
                );
                s.state.agents[0].name = "Current name".into();
                s.state.agents[0].instructions = "Current instructions".into();
            });
        })
        .unwrap();
        cx.wait_for(window, Duration::from_secs(10), |_, cx| {
            let s = shark.read(cx);
            s.creating.is_empty() && !s.state.tasks.is_empty()
        })
        .await;
        cx.read(|cx| {
            let s = shark.read(cx);
            let task = &s.state.tasks[0];
            let link = task.agent.as_ref().unwrap();
            assert_eq!(link.name, "Current name");
            assert_eq!(link.instructions, "Current instructions");
            let docs = std::fs::read_to_string(task.dir().join("CLAUDE.md")).unwrap();
            assert!(docs.contains("Current name") && docs.contains("Current instructions"));
            let saved = State::load();
            assert_eq!(
                saved.tasks[0].agent.as_ref().unwrap().instructions,
                "Current instructions"
            );
        });
    }
}
