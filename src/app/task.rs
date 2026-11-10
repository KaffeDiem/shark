//! Creating, opening, growing and deleting tasks.
use super::new_task::NewTaskForm;
use super::*;
use crate::state::{AgentLink, AttachStage, Sibling};

impl Shark {
    pub(super) fn can_backlog(&self, name: &str, cx: &App) -> bool {
        !self.busy(name, cx) && !self.asking_chat(name, cx)
    }

    pub(super) fn set_backlog(
        &mut self,
        name: &str,
        on: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if on && !self.can_backlog(name, cx) {
            return;
        }
        let before = self.cursors(cx);
        let Some(task) = self.state.tasks.iter_mut().find(|t| t.name == name) else {
            return;
        };
        if task.backlog == on {
            return;
        }
        task.backlog = on;
        let title = task.display_title().to_string();
        let _ = self.state.save();
        if window.is_some() {
            self.record_interaction(name, cx);
        }
        if on {
            self.restore_cursors(name, before, cx);
            self.fix_sel();
        }
        self.refresh_attention(cx);
        if on && let Some(window) = window {
            window.push_notification(format!("Moved “{title}” to backlog"), cx);
        }
        cx.notify();
    }

    pub(crate) fn sync_agent(
        &mut self,
        slug: &str,
        docs: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(agent) = self.state.agents.iter().find(|a| a.slug == slug).cloned() else {
            return Ok(());
        };
        let tasks: Vec<_> = self
            .state
            .tasks
            .iter()
            .filter(|t| t.agent.as_ref().is_some_and(|a| a.slug == slug))
            .cloned()
            .collect();
        let mut errors = vec![];
        for task in &tasks {
            let siblings = tasks
                .iter()
                .filter(|other| other.name != task.name)
                .map(|other| {
                    let dirs: Vec<_> = other
                        .repos
                        .iter()
                        .map(|r| other.dir().join(&r.name).to_string_lossy().into_owned())
                        .collect();
                    let files = task
                        .agent
                        .as_ref()
                        .and_then(|a| {
                            a.siblings
                                .iter()
                                .find(|s| s.branch == other.branch && s.dirs == dirs)
                        })
                        .or_else(|| {
                            tasks.iter().find_map(|known| {
                                known
                                    .agent
                                    .as_ref()?
                                    .siblings
                                    .iter()
                                    .find(|s| s.branch == other.branch && s.dirs == dirs)
                            })
                        })
                        .map(|s| s.files.clone())
                        .unwrap_or_default();
                    Sibling {
                        title: other.display_title().into(),
                        branch: other.branch.clone(),
                        dirs,
                        files,
                    }
                })
                .collect();
            let mut link = task.agent.clone().unwrap();
            // Instruction snapshots change only once the root docs can deliver them too.
            if docs {
                link.name = agent.name.clone();
                link.instructions = agent.instructions.clone();
            }
            link.siblings = siblings;
            let mut current = task.clone();
            current.agent = Some(link);
            if docs && let Err(e) = git::write_claude_md(&current) {
                errors.push(format!("{}: {e}", task.display_title()));
                continue;
            }
            self.task_mut(&task.name).unwrap().agent = current.agent;
            self.publish_task(&task.name, cx);
        }
        if let Err(e) = self.state.save() {
            errors.push(e.to_string());
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    pub(crate) fn open_task(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.state.tasks.get(ix).map(|w| w.name.clone()) else {
            return;
        };
        self.open_task_named(&name, window, cx);
    }

    pub(super) fn open_task_named(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_task_then(name, None, window, cx, |_, _, _| {});
    }

    /// Opens `name`, then runs `then`: at once, or once a discard dialog is confirmed.
    pub(super) fn open_task_then(
        &mut self,
        name: &str,
        chat: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Shark, &mut Window, &mut Context<Shark>) + 'static,
    ) {
        if self.archiving.contains(name) || self.task(name).is_none() {
            return notify_err(window, cx, "The selected task is no longer available");
        }
        let instance = self.task(name).unwrap().instance_id.clone();
        // Pick after confirmation, before showing anything: the previous chat must stay unread.
        let prepare_chat = move |s: &mut Self, name: &str| {
            let Some(id) = chat else { return true };
            let Some(ws) = s.task_mut(name) else {
                return false;
            };
            let Some(ix) = ws.chats.iter().position(|c| c.id == id).or_else(|| {
                let ix = ws.closed.iter().position(|c| c.id == id)?;
                let meta = ws.closed.remove(ix);
                Some(ws.push_chat(meta))
            }) else {
                return false;
            };
            if ws.active_chat != ix {
                ws.active_chat = ix;
                let _ = s.state.save();
            }
            true
        };
        if self.active.as_deref() == Some(name) {
            if !prepare_chat(self, name) {
                return notify_err(window, cx, "The selected chat is no longer available");
            }
            if self.home.on {
                self.leave_home(window, cx);
            }
            self.record_interaction(name, cx);
            return then(self, window, cx);
        }
        let name = name.to_string();
        // Resolve identity before dropping *or marking clean* any buffer: the destination may
        // disappear while the discard dialog is open.
        let editors = self.file_tabs.iter().map(|t| t.editor.clone()).collect();
        self.confirm_editors(editors, true, false, window, cx, move |s, window, cx| {
            let Some(ix) = s
                .state
                .tasks
                .iter()
                .position(|w| w.name == name && w.instance_id == instance)
                .filter(|_| !s.archiving.contains(&name))
            else {
                return notify_err(window, cx, "The selected task is no longer available");
            };
            if !prepare_chat(s, &name) {
                return notify_err(window, cx, "The selected chat is no longer available");
            }
            s.open_task_now(ix, window, cx);
            s.record_interaction(&name, cx);
            then(s, window, cx);
        });
    }

    pub(super) fn open_task_now(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.state.tasks.get(ix).cloned() else {
            return;
        };
        let changed = self.active.as_ref() != Some(&ws.name);
        if changed {
            self.drop_editor(cx);
        }
        self.sel = SideRow::Task(ix);
        if let Some(agent) = &ws.agent {
            self.collapsed_agents.remove(&agent.slug);
        }
        self.ensure_sync(&ws, window, cx);
        for (name, sync) in &self.syncs {
            let on = *name == ws.name;
            sync.update(cx, |g, cx| g.set_active(on, cx));
        }
        self.attn.opened(&ws.name);
        self.active = Some(ws.name);
        self.reset_differ_task(window, cx);
        self.refresh_attention(cx);
        self.refresh_diff(cx);
        self.show(Tab::Chat, window, cx);
        self.select_chat(ws.active_chat, window, cx);
        if changed {
            self.unpark_sessions(window, cx);
            self.restore_file_tabs(window, cx);
        }
    }

    /// Creates `ws`'s GitSync, and the subscriptions that route its events, if it has none.
    fn ensure_sync(&mut self, ws: &Task, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncs.contains_key(&ws.name) {
            return;
        }
        let sync = cx.new(|cx| {
            let mut sync = GitSync::new(ws.clone(), window, cx);
            sync.restore_settings(&self.state);
            sync
        });
        let name = ws.name.clone();
        self._subs.push(cx.subscribe_in(
            &sync,
            window,
            move |this, _, ev: &SyncEvent, window, cx| this.sync_event(&name, ev, window, cx),
        ));
        let name = ws.name.clone();
        self._subs
            .push(cx.observe_in(&sync, window, move |s, sync, w, cx| {
                if s.active_sync() == Some(&sync) {
                    s.configure_differ(cx);
                    s.configure_git(w, cx);
                    s.differ.update(cx, |_, cx| cx.notify());
                }
                s.advance_attachment(&name, w, cx); // a git step may be what it waited for
            }));
        self.syncs.insert(ws.name.clone(), sync);
    }

    fn sync_event(
        &mut self,
        name: &str,
        ev: &SyncEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            SyncEvent::Interacted => self.record_interaction(name, cx),
            SyncEvent::Reviewed(ids) => {
                self.state.reviewed.insert(name.to_string(), ids.clone());
                let _ = self.state.save();
                cx.notify();
            }
            SyncEvent::MergeOrder(order) => {
                self.state
                    .merge_order
                    .insert(name.to_string(), order.clone());
                let _ = self.state.save();
                cx.notify();
            }
            SyncEvent::DeleteRemote(on) => self.set_delete_remote(*on, cx),
            SyncEvent::Archive => self.archive_task(name.to_string(), false, window, cx),
            SyncEvent::PrsCreated => self.state.mark(|s| &mut s.opened_pr),
            SyncEvent::PrBaseAdopted(moves) => {
                if let Some(ws) = self.task_mut(name) {
                    for (repo, old, new) in moves {
                        if let Some(r) =
                            (ws.repos.iter_mut()).find(|r| r.name == *repo && r.base == *old)
                        {
                            r.base = new.clone();
                        }
                    }
                    let _ = git::write_claude_md(ws);
                }
                let _ = self.state.save();
                self.refresh_diff_if_active(name, cx);
            }
            SyncEvent::DiffBaseChanged => self.refresh_diff_if_active(name, cx),
            SyncEvent::Restack { moves, prompt } => {
                self.restack(name, moves, prompt.clone(), window, cx)
            }
        }
    }

    /// Refreshes the diff when `name` is the open task.
    pub(super) fn refresh_diff_if_active(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.active.as_deref() == Some(name) {
            self.refresh_diff(cx);
        }
    }

    pub(super) fn add_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add repo".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update_in(cx, |s, window, cx| s.add_repo_paths(&paths, window, cx))
                .ok();
        })
        .detach();
    }

    /// From the folder picker or folders dropped on the sidebar. Returns the repos it added.
    pub(super) fn add_repo_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Repo> {
        let mut added = vec![];
        for p in paths {
            match git::add_repo(p, &self.state.repos) {
                Ok(r) => {
                    self.state.repos.push(r.clone());
                    added.push(r);
                }
                Err(e) => notify_err(window, cx, e),
            }
        }
        let _ = self.state.save();
        cx.notify();
        added
    }

    /// New task: an empty conversation, no dialog, no repository. `first`, when set, titles it
    /// and goes out as its first message. `issue`, a title and a snapshot, is attached before that
    /// and gives the title instead. Unsaved edits are asked about before anything exists, so
    /// cancelling leaves nothing behind.
    pub(super) fn new_task(
        &mut self,
        first: Option<String>,
        issue: Option<(String, Snapshot)>,
        agent: Option<AgentLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editors = self.file_tabs.iter().map(|t| t.editor.clone()).collect();
        self.confirm_editors(editors, true, false, window, cx, move |s, window, cx| {
            s.create_task(first, issue, agent, window, cx)
        });
    }

    /// Saved before any provider starts: a failed save leaves neither agent nor directory. A
    /// failed first send leaves the agent, with the message back in its composer (`Chat::ask`).
    fn create_task(
        &mut self,
        first: Option<String>,
        issue: Option<(String, Snapshot)>,
        agent: Option<AgentLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(link) = &agent
            && !self.state.agents.iter().any(|a| a.slug == link.slug)
        {
            return notify_err(window, cx, "This agent was deleted. Choose an agent again.");
        }
        // Nothing is saved this run, so the agent would be gone on restart.
        if self.state.read_only {
            return notify_err(
                window,
                cx,
                "Can't create tasks: state.json couldn't be read",
            );
        }
        let name = crate::state::task_name(interaction::now_ms(), |n| {
            self.creating.contains_key(n)
                || self.task(n).is_some()
                || tasks_dir().join(n).exists()
                || crate::transcript::transcript_path(n).exists()
        });
        let from_issue = issue.is_some();
        let title = match (&issue, first.as_deref()) {
            (Some((title, _)), _) => title.clone(),
            (None, Some(first)) => crate::chat::title_of(first),
            (None, None) => crate::state::NEW_TASK.into(),
        };
        let dir = tasks_dir().join(&name);
        let mut ws = match git::create_empty_task(&dir, &name, &title, agent) {
            Ok(ws) => ws,
            Err(e) => return notify_err(window, cx, e),
        };
        if let Some((_, snapshot)) = issue {
            // ponytail: one small write on the UI thread, like the agent's other first writes.
            if let Err(e) = crate::linear::put_snapshot(&name, &snapshot) {
                let _ = git::remove_task(&ws, &dir, false);
                return notify_err(window, cx, format!("Couldn't save the issue: {e}"));
            }
            ws.linear.attach((&snapshot).into());
            ws.issues.push(snapshot);
        }
        let (model, effort) = self.state.defaults_for(&ws);
        ws.fill_models(&model, effort.as_ref());
        self.state.tasks.push(ws.clone());
        if let Err(e) = self.state.save() {
            self.state.tasks.pop();
            let _ = git::remove_task(&ws, &dir, false);
            crate::linear::delete_snapshots(&name);
            return notify_err(window, cx, format!("Couldn't save the new task: {e}"));
        }
        if let Some(agent) = &ws.agent {
            let _ = self.sync_agent(&agent.slug, false, cx);
            self.refresh_agent_files(&agent.slug, cx);
        }
        self.open_task_now(self.state.tasks.len() - 1, window, cx);
        self.record_interaction(&name, cx);
        self.home_prompt_used(
            first.as_deref(),
            ws.agent.as_ref().map(|a| a.slug.as_str()),
            window,
            cx,
        );
        let Some(first) = first else { return };
        if !from_issue {
            self.auto_title(&name, &first, cx);
        }
        if let Some(chat) = self.active_chat().cloned() {
            chat.update(cx, |c, cx| c.ask(&first, window, cx));
        }
    }

    /// `name`'s title, for messages; `name` itself once it's gone.
    pub(crate) fn task_title(&self, name: &str) -> String {
        self.task(name)
            .map_or(name.into(), |w| w.display_title().into())
    }

    /// "Rename task…": a field with the title.
    pub(super) fn rename_task(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.task_title(&name);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let (field, shark) = (input.clone(), cx.entity().downgrade());
        window.open_dialog(cx, move |d, _, _| {
            let (input, shark, name) = (input.clone(), shark.clone(), name.clone());
            crate::ui::dialog(d)
                .title("Rename task")
                .w(px(440.))
                .child(Input::new(&input))
                .on_ok(move |_, _, cx| {
                    let title = input.read(cx).value().trim().to_string();
                    shark.update(cx, |s, cx| s.set_title(&name, title, cx)).ok();
                    true
                })
        });
        field.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
    }

    /// Only the title changes: identity is `name`, so directories, transcripts and branches
    /// stay. An empty title changes nothing, nor does one left as shown: untitled stays untitled.
    pub(super) fn set_title(&mut self, name: &str, title: String, cx: &mut Context<Self>) {
        let edited = |w: &&mut Task| !title.is_empty() && title != w.display_title();
        if let Some(ws) = self.task_mut(name).filter(edited) {
            ws.title = title;
            let agent = ws.agent.as_ref().map(|a| a.slug.clone());
            if let Some(slug) = agent {
                let _ = self.sync_agent(&slug, false, cx);
            }
            let _ = self.state.save();
            cx.notify();
        }
    }

    /// With an OpenRouter key (Settings > General), a prompt of more than four words gets a 3-4
    /// word title. A rename made meanwhile wins; any failure keeps the prompt's first line.
    pub(super) fn auto_title(&mut self, name: &str, prompt: &str, cx: &mut Context<Self>) {
        let Some(task) = self.task(name) else {
            return;
        };
        let before = task.title.clone();
        let instance = task.instance_id.clone();
        // A creation hook precedes the first send; its Title event will request this once.
        if before == crate::state::NEW_TASK && task.chats.iter().all(|c| c.title.is_empty()) {
            return;
        }
        if !crate::title::wanted(prompt) || self.openrouter_key == Some(None) {
            return;
        }
        let cached = self.openrouter_key.clone();
        let read = cached
            .is_none()
            .then(|| cx.read_credentials(crate::title::KEYCHAIN));
        let (name, prompt) = (name.to_string(), prompt.to_string());
        cx.spawn(async move |this, cx| {
            let key = match (cached, read) {
                (Some(key), _) => key,
                (None, Some(read)) => {
                    // ponytail: a declined Keychain prompt reads as "no key" until relaunch.
                    let key = crate::title::key_from(read.await);
                    // A key saved or removed in Settings meanwhile wins.
                    this.update(cx, |s, _| s.openrouter_key.get_or_insert(key).clone())
                        .ok()
                        .flatten()
                }
                (None, None) => None,
            };
            let Some(key) = key else { return };
            let title = cx
                .background_executor()
                .spawn(async move { crate::title::generate(&key, &prompt) })
                .await;
            let Ok(title) = title else { return };
            this.update(cx, |s, cx| {
                if s.task(&name)
                    .is_some_and(|t| t.instance_id == instance && t.title == before)
                {
                    s.set_title(&name, title, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// `picked` repos start ticked: the last-used ones, or the repo clicked in the sidebar.
    /// `first` fills in the first message (Home's prompt box).
    pub(super) fn new_task_from_repos(
        &mut self,
        picked: Vec<String>,
        first: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.new_task_from(picked, first, vec![], None, None, window, cx)
    }

    /// The branch and PR dialog with `from` `(repo, parent)` prefilled; `insert` names the
    /// task to stack on the new one. It checks out repos, so it needs one registered.
    pub(super) fn new_task_from(
        &mut self,
        picked: Vec<String>,
        first: Option<String>,
        from: Vec<(String, String)>,
        insert: Option<String>,
        agent: Option<AgentLink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.repos.is_empty() {
            return notify_err(window, cx, "Add a repo first (Add repo in cmd-k)");
        }
        let shark = cx.entity().downgrade();
        let repos = self.state.repos.clone();
        let form = cx.new(|cx| {
            NewTaskForm::new(
                repos, &picked, first, agent, &from, insert, None, shark, window, cx,
            )
        });
        let input = form.read(cx).branch.clone();
        window.open_dialog(cx, move |d, _, _| {
            let f = form.clone();
            // Dialog binds enter to Confirm, which runs on_ok and closes when it returns true.
            // In the repo picker, the Command list takes enter first (it ticks a repo).
            crate::ui::dialog(d)
                .title("New task from branch or PR")
                .w(px(640.))
                .child(form.clone())
                .on_ok(move |_, window, cx| f.update(cx, |f, cx| f.submit(window, cx)))
        });
        input.update(cx, |i, cx| i.focus(window, cx));
    }

    /// `prompt`, when set, is sent to the agent once the task is open.
    #[cfg(test)]
    pub(super) fn create_task_for_test(
        &mut self,
        branch: String,
        repos: Vec<(Repo, String)>,
        prs: Vec<(String, git::PullRequest)>,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = prompt
            .as_deref()
            .map(crate::chat::title_of)
            .unwrap_or_default();
        self.create_task_on(
            branch,
            repos,
            prs,
            vec![],
            None,
            prompt,
            title,
            None,
            false,
            window,
            cx,
        )
    }

    /// `create_workspace` with repos' new branches stacked on `parents` `(repo, branch)`. With
    /// `insert`, that task's repos based on the same parent move on top of the new one:
    /// its (still empty) branch is pushed so their PRs can target it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_task_on(
        &mut self,
        branch: String,
        repos: Vec<(Repo, String)>,
        prs: Vec<(String, git::PullRequest)>,
        parents: Vec<(String, String)>,
        insert: Option<String>,
        prompt: Option<String>,
        title: String,
        agent: Option<AgentLink>,
        fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(link) = &agent
            && !self.state.agents.iter().any(|a| a.slug == link.slug)
        {
            return notify_err(window, cx, "This agent was deleted. Choose an agent again.");
        }
        let moves = (insert.as_deref())
            .and_then(|n| self.task(n))
            .map(|child| insert_moves(child, &parents, &repos))
            .unwrap_or_default();
        if insert.is_some() && moves.is_empty() {
            return notify_err(
                window,
                cx,
                "Nothing to insert below: no repo shares its parent",
            );
        }
        let name = branch.replace('/', "-");
        // Nothing is saved this run, so the worktrees would be orphaned on restart.
        if self.state.read_only {
            return notify_err(
                window,
                cx,
                "Can't create tasks: state.json couldn't be read",
            );
        }
        if self.creating.contains_key(&name) || self.state.tasks.iter().any(|w| w.name == name) {
            return notify_err(window, cx, format!("task `{name}` already exists"));
        }
        self.creating
            .insert(name.clone(), agent.as_ref().map(|a| a.slug.clone()));
        self.state.last_repos = repos.iter().map(|(r, _)| r.name.clone()).collect();
        let _ = self.state.save();
        let dir = tasks_dir().join(&name);
        window.push_notification(format!("Creating {}…", tilde(&dir)), cx);
        let pending_name = name.clone();
        let pushes: Vec<(String, String)> = (moves.iter())
            .filter_map(|(r, ..)| {
                Some((
                    r.clone(),
                    repos.iter().find(|(x, _)| &x.name == r)?.1.clone(),
                ))
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let (result, pushed) = cx
                .background_executor()
                .spawn(async move {
                    let ws = git::create_task_with_prs(
                        &dir, &name, &branch, &repos, &prs, &parents, &title, agent, fresh,
                    );
                    // The base a PR targets must exist on GitHub.
                    let pushed = ws.is_ok().then(|| {
                        (pushes.iter())
                            .map(|(r, b)| {
                                let args = ["push", "-q", "-u", "origin", b.as_str()];
                                git::cmd_bounded(&dir.join(r), "git", &args, 120)
                                    .map_err(|e| format!("{r}: {e}"))
                            })
                            .collect::<Result<Vec<_>, _>>()
                    });
                    (ws, pushed)
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.creating.remove(&pending_name);
                match result {
                    Ok(mut ws) => {
                        match (&insert, pushed) {
                            (Some(child), Some(Ok(_))) => {
                                s.set_base(child, &moves, false, window, cx)
                            }
                            (Some(child), Some(Err(e))) => notify_err(
                                window,
                                cx,
                                format!(
                                    "Created, but `{}` stays where it was; pushing failed: {e}",
                                    s.task_title(child)
                                ),
                            ),
                            _ => {}
                        }
                        let (model, effort) = s.state.defaults_for(&ws);
                        ws.fill_models(&model, effort.as_ref());
                        let name = ws.name.clone();
                        s.state.tasks.push(ws.clone());
                        if let Some(agent) = &ws.agent {
                            // Settings may have changed while the worktrees were created.
                            let docs = s
                                .state
                                .agents
                                .iter()
                                .find(|a| a.slug == agent.slug)
                                .is_some_and(|a| {
                                    a.name != agent.name || a.instructions != agent.instructions
                                });
                            if let Err(e) = s.sync_agent(&agent.slug, docs, cx) {
                                notify_err(
                                    window,
                                    cx,
                                    format!("Couldn't refresh agent instructions: {e}"),
                                );
                            }
                            let current = s.task(&name).unwrap().clone();
                            s.refresh_agent_files(&agent.slug, cx);
                            ws = current;
                        }
                        let _ = s.state.save();
                        if let Some(p) = &prompt {
                            s.auto_title(&name, p, cx);
                        }
                        // A dirty file tab in the task left behind asks first: send once opened.
                        let deferred = Rc::new(std::cell::Cell::new(false));
                        let (later, setup_ws, key) = (deferred.clone(), ws.clone(), name.clone());
                        s.open_task_then(&key, None, window, cx, move |s, window, cx| {
                            s.home_prompt_used(
                                prompt.as_deref(),
                                ws.agent.as_ref().map(|a| a.slug.as_str()),
                                window,
                                cx,
                            );
                            let chat = (s.active.as_ref() == Some(&name))
                                .then(|| s.active_chat().cloned())
                                .flatten();
                            let then = prompt.zip(chat);
                            if later.get() {
                                // Setup started without us: still running, the prompt waits typed.
                                if let Some((p, chat)) = then {
                                    match s.setting_up.contains(&name) {
                                        true => chat.update(cx, |c, cx| {
                                            c.composer
                                                .update(cx, |t, cx| t.set_value(p, window, cx))
                                        }),
                                        false => chat.update(cx, |c, cx| _ = c.ask(&p, window, cx)),
                                    }
                                }
                            } else if !s.run_setup(&ws, None, then.clone(), window, cx)
                                && let Some((p, chat)) = then
                            {
                                chat.update(cx, |c, cx| c.ask(&p, window, cx));
                            }
                        });
                        // Waiting on a dialog, or refused: the new task still gets set up.
                        if s.active.as_ref() != Some(&key) {
                            deferred.set(true);
                            s.run_setup(&setup_ws, None, None, window, cx);
                        }
                    }
                    Err(e) => notify_err(
                        window,
                        cx,
                        if fresh {
                            format!("{e}\nRetry from Home to generate a new branch name.")
                        } else {
                            e
                        },
                    ),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Launch: reopen the task and tab that were open at quit.
    /// Nothing to reopen, or Home was shown at quit: Home.
    pub(super) fn restore_last_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.state.last_open.clone();
        let ix = last
            .as_ref()
            .and_then(|(name, _)| self.state.tasks.iter().position(|w| &w.name == name));
        let Some((ix, (_, tab))) = ix.zip(last) else {
            return self.enter_home(window, cx);
        };
        self.open_task_now(ix, window, cx);
        match tab.as_str() {
            "diff"
                if self
                    .active_task()
                    .is_some_and(|ws| Feature::Diff.is_open(ws)) =>
            {
                self.show(Tab::Feature(Feature::Diff), window, cx)
            }
            "git"
                if self
                    .active_task()
                    .is_some_and(|ws| Feature::Git.is_open(ws)) =>
            {
                self.show(Tab::Feature(Feature::Git), window, cx)
            }
            "home" => self.enter_home(window, cx),
            _ => {}
        }
    }

    /// Remembers the open task and tab in state.json, for the next launch.
    pub(super) fn remember_open(&mut self) {
        let tab = match self.tab {
            _ if self.home.on => "home",
            Tab::Feature(Feature::Diff) => "diff",
            Tab::Feature(Feature::Git) => "git",
            Tab::Chat | Tab::File => "chat",
        };
        let open = self.active.clone().map(|n| (n, tab.to_string()));
        if self.state.last_open != open {
            self.state.last_open = open;
            let _ = self.state.save();
        }
    }

    /// "Attach repository…" on the target task.
    pub(super) fn pick_repo_for_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_target(window, cx, |s, ws, window, cx| {
            s.attach_repository(ws.name, window, cx)
        })
    }

    /// `refuse_unready` said no. A toast because it can come from anywhere, Zen and the plan
    /// pane too; it names `name` because another agent may be open by the time it's clicked.
    pub(super) fn offer_attach(&self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        struct AttachOffer;
        let (shark, name) = (cx.entity().downgrade(), name.to_string());
        let toast = Notification::warning("Building needs a repository")
            .id::<AttachOffer>()
            .action(move |_, _, cx| {
                let (shark, name) = (shark.clone(), name.clone());
                Button::new("attach-offer")
                    .primary()
                    .label("Attach repository")
                    .on_click(cx.listener(move |toast, _, w, cx| {
                        toast.dismiss(w, cx);
                        shark
                            .update(cx, |s, cx| s.attach_repository(name.clone(), w, cx))
                            .ok();
                    }))
            })
            .autohide(true);
        window.push_notification(toast, cx);
    }

    /// "New task on top…": the New task dialog with this task's repos ticked,
    /// each new branch based on this one's. With `insert` ("Insert task below…") they're
    /// based on this one's base instead, and this task moves on top of the new one.
    pub(super) fn stack_from(
        &mut self,
        name: String,
        insert: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(&name).cloned() else {
            return;
        };
        if ws.repos.is_empty() {
            return notify_err(window, cx, NO_REPO);
        }
        // A review checkout's base is a `refs/shark/…` ref, not a branch to stack on.
        let from: Vec<(String, String)> = (ws.repos.iter())
            .filter(|r| r.base.starts_with("origin/"))
            .map(|r| match insert {
                true => (r.name.clone(), crate::git::short(&r.base).to_string()),
                false => (r.name.clone(), ws.branch_of(r).to_string()),
            })
            .collect();
        if from.is_empty() {
            return notify_err(window, cx, "A local PR review can't be stacked on");
        }
        let picked = from.iter().map(|(r, _)| r.clone()).collect();
        self.new_task_from(picked, None, from, insert.then_some(name), None, window, cx);
    }

    /// "Go to stack…": every branch of the task's stacks, newest first; a pick opens its
    /// task, else its PR.
    pub(super) fn go_to_stack(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(&name).cloned() else {
            return;
        };
        let stacks = menus::stacks(self, &ws, cx);
        let several = stacks.len() > 1;
        let rows: Vec<(String, Option<String>, Option<String>, bool)> = (stacks.iter())
            .flat_map(|s| {
                (s.nodes.iter().enumerate().rev()).map(move |(i, n)| {
                    let pr =
                        n.pr.as_ref()
                            .map(|p| format!(" · #{} {}", p.number, p.title));
                    let repo = if several {
                        format!("{} · ", s.repo)
                    } else {
                        String::new()
                    };
                    let label = format!("{repo}{}{}", n.branch, pr.unwrap_or_default());
                    (
                        label,
                        n.ws.clone(),
                        n.pr.as_ref().map(|p| p.url.clone()),
                        i == s.me,
                    )
                })
            })
            .collect();
        if rows.is_empty() {
            return notify_err(window, cx, "Not part of a stack");
        }
        let items = (rows.iter())
            .map(|(label, ws, ..)| {
                CommandItem::new()
                    .label(label.clone())
                    .icon(match ws {
                        Some(_) => IconName::GitBranch,
                        None => IconName::GitPullRequest,
                    })
                    .checked(rows.iter().any(|r| r.3 && &r.0 == label))
            })
            .collect();
        let selected = rows.iter().position(|r| r.3).unwrap_or(0);
        open_menu(
            cx.entity().downgrade(),
            "Go to a branch of the stack…",
            items,
            selected,
            move |s, row, w, cx| match &rows[row] {
                (_, Some(n), ..) => s.open_task_named(n, w, cx),
                (_, None, Some(u), _) => cx.open_url(u),
                _ => {}
            },
            window,
            cx,
        );
    }

    /// "Change base…": which repos (when there are several), then the new base, then whether
    /// the agent rebases onto it.
    pub(super) fn change_base(
        &mut self,
        name: String,
        repo: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(&name).cloned() else {
            return;
        };
        if ws.repos.is_empty() {
            return notify_err(window, cx, NO_REPO);
        }
        if let Some(repo) = repo {
            if !ws.repos.iter().any(|r| r.name == repo) {
                return notify_err(window, cx, "Repository is no longer attached");
            }
            return self.pick_base(name, vec![repo], window, cx);
        }
        let repos: Vec<String> = ws.repos.iter().map(|r| r.name.clone()).collect();
        if repos.len() < 2 {
            return self.pick_base(name, repos, window, cx);
        }
        let items = std::iter::once(
            CommandItem::new()
                .label(format!("All repos · {}", menus::base_hint(&ws)))
                .icon(IconName::Layers),
        )
        .chain(ws.repos.iter().map(|r| {
            CommandItem::new()
                .label(format!("{} · {}", r.name, r.base))
                .icon(IconName::FolderGit2)
        }))
        .collect();
        open_menu(
            cx.entity().downgrade(),
            "Change the base of…",
            items,
            0,
            move |_, row, w, cx| {
                let picked = match row {
                    0 => repos.clone(),
                    i => vec![repos[i - 1].clone()],
                };
                let name = name.clone();
                // A new dialog once this one has closed.
                cx.defer_in(w, move |s, w, cx| s.pick_base(name, picked, w, cx));
            },
            window,
            cx,
        );
    }

    /// The `origin/*` branches of `repos`, newest first; a branch only some have moves only those.
    fn pick_base(
        &mut self,
        name: String,
        repos: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.task(&name).cloned() else {
            return;
        };
        let dir = ws.dir();
        // ponytail: for-each-ref on the UI thread; it's a local ref read, move to the background if it lags.
        let per_repo: Vec<git::RepoBranches> = (ws.repos.iter())
            .filter(|r| repos.contains(&r.name))
            .map(|r| {
                let own = ws.branch_of(r).to_string();
                (
                    r.name.clone(),
                    own,
                    git::remote_branches(&dir.join(&r.name)),
                )
            })
            .collect();
        let choices = git::base_choices(&per_repo);
        if choices.is_empty() {
            return notify_err(window, cx, "No origin branches found; fetch first");
        }
        let current: Vec<&str> = (ws.repos.iter())
            .filter(|r| repos.contains(&r.name))
            .map(|r| r.base.as_str())
            .collect();
        let items = (choices.iter())
            .map(|(b, have)| {
                let label = match have.len() == repos.len() {
                    true => b.clone(),
                    false => format!("{b} · only {}", have.join(", ")),
                };
                CommandItem::new()
                    .label(label)
                    .icon(IconName::GitBranch)
                    .checked(current.iter().all(|c| c == b))
            })
            .collect();
        let selected = (choices.iter())
            .position(|(b, _)| current.first() == Some(&b.as_str()))
            .unwrap_or(0);
        let old: Vec<(String, String)> = (ws.repos.iter())
            .map(|r| (r.name.clone(), r.base.clone()))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "New base…",
            items,
            selected,
            move |_, row, w, cx| {
                let (new, have) = &choices[row];
                let moves: Vec<(String, String, String)> = (old.iter())
                    .filter(|(r, o)| have.contains(r) && o != new)
                    .map(|(r, o)| (r.clone(), o.clone(), new.clone()))
                    .collect();
                let name = name.clone();
                cx.defer_in(w, move |s, w, cx| s.pick_rebase(name, moves, w, cx));
            },
            window,
            cx,
        );
    }

    fn pick_rebase(
        &mut self,
        name: String,
        moves: Vec<(String, String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, _, new)) = moves.first().cloned() else {
            return notify_err(window, cx, "Already the base");
        };
        let items = vec![
            CommandItem::new()
                .label(format!("Rebase onto {new} with the agent"))
                .icon(IconName::Sparkles),
            CommandItem::new()
                .label(format!("Only target {new}; leave the commits as they are"))
                .icon(IconName::Target),
        ];
        open_menu(
            cx.entity().downgrade(),
            "Move the branch too?",
            items,
            0,
            move |s, row, w, cx| s.set_base(&name, &moves, row == 0, w, cx),
            window,
            cx,
        );
    }

    /// Records the new bases (`(repo, old, new)`), retargets open PRs on GitHub, and with `rebase`
    /// asks the agent to move the commits. The chats hear of it with their next normal turn.
    pub(super) fn set_base(
        &mut self,
        name: &str,
        moves: &[(String, String, String)],
        rebase: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.task(name).is_some_and(|w| !w.attaching.is_empty()) {
            return notify_err(window, cx, "Wait for the repository attachment to finish");
        }
        let sync = self.syncs.get(name).cloned();
        if rebase {
            let Some(sync) = &sync else {
                return notify_err(window, cx, "Open the task to rebase it");
            };
            if let Some(e) = sync.read(cx).blocked(true, cx) {
                return notify_err(window, cx, e);
            }
        }
        let Some(slot) = self.task_mut(name) else {
            return;
        };
        for (repo, _, new) in moves {
            if let Some(r) = slot.repos.iter_mut().find(|r| &r.name == repo) {
                r.base = new.clone();
            }
        }
        let ws = slot.clone();
        self.record_interaction(&ws.name, cx);
        let _ = self.state.save();
        let _ = git::write_claude_md(&ws);
        let changed = (moves.iter())
            .map(|(r, o, n)| format!("`{r}` now targets `{n}` (was `{o}`)"))
            .collect::<Vec<_>>()
            .join(", ");
        self.publish_task(name, cx);
        if let Some(sync) = &sync {
            let prs: Vec<(PathBuf, String, String)> = (moves.iter())
                .filter_map(|(r, _, n)| {
                    let url = sync.read(cx).pr_for(r)?.0.url;
                    let b = crate::git::short(n).to_string();
                    Some((ws.dir().join(r), url, b))
                })
                .collect();
            sync.update(cx, |g, cx| g.set_task(ws.clone(), cx));
            if !prs.is_empty() {
                cx.spawn_in(window, async move |_, cx| {
                    let errs = (cx.background_executor())
                        .spawn(async move {
                            (prs.iter())
                                .filter_map(|(wt, url, b)| {
                                    let args = ["pr", "edit", url.as_str(), "--base", b.as_str()];
                                    git::cmd_bounded(wt, "gh", &args, 30).err()
                                })
                                .collect::<Vec<_>>()
                        })
                        .await;
                    if !errs.is_empty() {
                        cx.update(|w, cx| {
                            notify_err(w, cx, format!("Retargeting PRs: {}", errs.join("; ")))
                        })
                        .ok();
                    }
                })
                .detach();
            }
            if rebase {
                sync.update(cx, |g, cx| g.rebase_onto(moves, window, cx));
            }
        }
        self.refresh_diff_if_active(name, cx);
        window.push_notification(format!("New base: {changed}"), cx);
        cx.notify();
    }

    /// A parent PR merged: its stacked repos target the parent's base from now on (their PRs
    /// too), and the agent moves their own commits there.
    pub(super) fn restack(
        &mut self,
        name: &str,
        moves: &[(String, String, String)],
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sync) = self.syncs.get(name).cloned() else {
            return;
        };
        if let Some(e) = sync.read(cx).blocked(true, cx) {
            return notify_err(window, cx, e);
        }
        self.set_base(name, moves, false, window, cx);
        sync.update(cx, |g, cx| {
            g.run_kind(sync::Kind::Agent(prompt), "Restack", window, cx)
        });
    }

    /// Row actions hit the task under the sidebar cursor when the sidebar is focused, else
    /// the highlighted Home row while Home shows, else the open one.
    pub(super) fn target(&self, window: &Window) -> Option<Task> {
        let name = match (self.sidebar_focus.is_focused(window), self.sel) {
            (true, SideRow::Task(ix)) => self.state.tasks.get(ix)?.name.clone(),
            // A heading or repo row is under the cursor: not the open task (cmd-backspace).
            (true, _) => return None,
            _ if self.home_shown() => self.home.sel.clone()?,
            _ => self.active.clone()?,
        };
        let task = self.task(&name)?;
        if self.sidebar_focus.is_focused(window) && !self.side_rows(false).contains(&self.sel) {
            return None;
        }
        if !self.sidebar_focus.is_focused(window)
            && self.home_shown()
            && (!self.home_task_matches(task) || (task.backlog && !self.home.backlog_open))
        {
            return None;
        }
        Some(task.clone())
    }

    pub(super) fn with_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, Task, &mut Window, &mut Context<Self>),
    ) {
        match self.target(window) {
            Some(ws) => f(self, ws, window, cx),
            None => notify_err(window, cx, "Open a task first"),
        }
    }

    pub(super) fn archive(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.target(window) {
            self.archive_task(ws.name, force, window, cx)
        }
    }

    pub(super) fn archive_task(
        &mut self,
        name: String,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.task_title(&name);
        if let Some(msg) = self.chats_at_work(&name, cx) {
            return notify_err(window, cx, msg);
        }
        let heading = match force {
            true => format!("Force delete {title}?"),
            false => format!("Delete {title}?"),
        };
        let body = self.delete_copy(&name, force);
        confirm(
            heading,
            body,
            "Delete",
            move |s, window, cx| s.remove_task(&name, force, window, cx),
            window,
            cx,
        );
    }

    /// What Delete task says it removes. Linear comes up only for an agent with issues attached.
    pub(super) fn delete_copy(&self, name: &str, force: bool) -> &'static str {
        let issues = (self.task(name)).is_some_and(|w| w.linear.primary().is_some());
        match (force, issues) {
            (true, true) => {
                "Removes its conversations, plans, repository and Linear attachments and worktrees, including uncommitted changes. Branches are kept. Nothing changes in Linear."
            }
            (true, false) => {
                "Removes its conversations, plans, repository attachments and worktrees, including uncommitted changes. Branches are kept."
            }
            (false, true) => {
                "Removes its conversations, plans, repository and Linear attachments and worktrees. Uncommitted changes stop this. Merged branches are deleted; other branches are kept. Nothing changes in Linear."
            }
            (false, false) => {
                "Removes its conversations, plans, repository attachments and worktrees. Uncommitted changes stop this. Merged branches are deleted; other branches are kept."
            }
        }
    }

    /// What to say while a chat of `name` works in its worktrees. An agent waiting on answers is
    /// mid-turn too: it would go on in a removed worktree.
    fn chats_at_work(&self, name: &str, cx: &App) -> Option<String> {
        if self.started_chats(name).iter().all(|c| c.read(cx).idle()) {
            return None;
        }
        // ctrl-c stops the shown chat's agent only.
        Some(match self.active.as_deref() == Some(name) {
            true => "A chat is running; stop it first (ctrl-c in each running chat)".into(),
            false => format!(
                "A chat in {} is running; open it and stop it first (ctrl-c)",
                self.task_title(name)
            ),
        })
    }

    pub(super) fn remove_task(
        &mut self,
        name: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active.as_deref() == Some(name);
        let editors = self
            .file_tabs
            .iter()
            .filter(|_| active)
            .map(|t| t.editor.clone())
            .collect();
        let name = name.to_string();
        // Keep buffers dirty until removal succeeds; another save or Git may block archiving.
        self.confirm_editors(editors, active, false, window, cx, move |s, window, cx| {
            s.archive_worktrees(&name, force, window, cx)
        });
    }

    fn archive_worktrees(
        &mut self,
        name: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(git) = self.git_features.get(name) {
            if git.read(cx).busy() {
                return notify_err(
                    window,
                    cx,
                    "Wait for the Git operation to finish before deleting",
                );
            }
            git.update(cx, |g, cx| g.retry_save(cx));
            if git.read(cx).saving_drafts() {
                return notify_err(
                    window,
                    cx,
                    "Wait for Git drafts to finish saving before deleting",
                );
            }
            if git.read(cx).storage_error().is_some() {
                return notify_err(window, cx, "Save Git drafts before deleting (Retry save)");
            }
        }
        if self.active.as_deref() == Some(name)
            && (self.file_tabs.iter().any(|t| t.editor.read(cx).saving)
                || self
                    .differ
                    .read(cx)
                    .edit_snapshot()
                    .is_some_and(|e| e.saving))
        {
            return notify_err(window, cx, "Wait for file saves before deleting");
        }
        // A failed setup's repo goes with the agent; what recovery couldn't identify isn't ours to.
        if self.adding_repos.contains(name)
            || (self.task(name)).is_some_and(|w| {
                w.attaching
                    .iter()
                    .any(|a| a.stage != AttachStage::SetupFailed)
            })
        {
            return notify_err(
                window,
                cx,
                "Finish or cancel the repository attachment before deleting",
            );
        }
        if self.setting_up.iter().any(|n| n == name) {
            return notify_err(window, cx, "Wait for setup scripts before deleting");
        }
        self.retry_saves(cx);
        if !self.unsaved_data(Some(name), cx).is_empty() {
            return notify_err(
                window,
                cx,
                "Unsaved data: Retry save before deleting this task",
            );
        }
        let Some(ws) = self.task(name).cloned() else {
            return;
        };
        // Asked again in the update that sets `archiving`: the dialogs on the way here stay open
        // for as long as the user likes, and a message queued behind a Git step can go out meanwhile.
        if let Some(msg) = self.chats_at_work(name, cx) {
            return notify_err(window, cx, msg);
        }
        if self.syncs.get(name).is_some_and(|g| g.read(cx).running()) {
            return notify_err(
                window,
                cx,
                "Wait for the Git operation to finish before deleting",
            );
        }
        // The merge step and the menu can both ask while a removal runs.
        if !self.archiving.insert(ws.name.clone()) {
            return;
        }
        for chat in self.started_chats(name) {
            chat.update(cx, |c, _| c.archiving = true);
        }
        if let Some(git) = self.git_features.get(name) {
            git.update(cx, |g, cx| {
                g.set_archiving(true, cx);
                cx.notify();
            });
        }
        // Force-archive keeps every branch, as its dialog says.
        let merged = match force {
            true => vec![],
            false => self
                .syncs
                .get(name)
                .map_or(vec![], |g| g.read(cx).merged_branches()),
        };
        let overview = self.overview_repos(name, cx);
        let known = overview.is_some_and(|repos| {
            !repos.is_empty()
                && repos.len() == ws.repos.len()
                && ws.repos.iter().all(|expected| {
                    repos
                        .iter()
                        .any(|r| r.name == expected.name && matches!(r.pr, Some(Ok(_))))
                })
        });
        let prs: Vec<_> = overview
            .into_iter()
            .flatten()
            .filter_map(|r| r.pr().map(|pr| (r.name.clone(), pr.url.clone(), pr.state)))
            .collect();
        // Its dev servers would run on in a removed worktree: stopped (and waited for) first.
        let run = self.runs.remove(name);
        let jobs = crate::scripts::jobs(&self.state, &ws, crate::scripts::Phase::Archive, None);
        // `worktree remove` deletes node_modules, target and the like: seconds, off the UI thread.
        window.push_notification(format!("Deleting {}…", ws.display_title()), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = ws.dir();
            let name = ws.name.clone();
            let (scripts, result, run) = cx
                .background_executor()
                .spawn(async move {
                    // A refused archive must not stop servers or run the archive script.
                    if let Err(e) = git::archive_preflight(&ws, &dir, force) {
                        return (None, Err(e), run);
                    }
                    crate::scripts::Running::stop_all(run, 5);
                    // And those cmd-r stopped just before.
                    crate::scripts::wait_stopping(Some(&ws.name));
                    // A failed archive script keeps the worktree, unless forced.
                    let scripts = crate::scripts::run_all(&ws.name, &jobs).err();
                    if scripts.is_some() && !force {
                        return (
                            scripts,
                            Err("Archive script failed; task kept.".into()),
                            None,
                        );
                    }
                    let removed = git::remove_task(&ws, &dir, force)
                        .map(|()| git::delete_merged_branches(&ws, &merged));
                    (scripts, removed, None)
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.archiving.remove(&name);
                if let Some(run) = run {
                    s.runs.insert(name.clone(), run);
                }
                if let Some(e) = scripts {
                    super::run::script_failed(&name, e, window, cx);
                }
                if let Ok(deleted) = &result
                    && let Some(task) = s.task(&name)
                    && let Some(link) = &task.agent
                {
                    let slug = link.slug.clone();
                    let record = crate::state::TaskRecord {
                        title: task.display_title().into(),
                        branch: task.branch.clone(),
                        prs: prs
                            .iter()
                            .map(|(repo, url, _)| (repo.clone(), url.clone()))
                            .collect(),
                        merged: if !task.repos.is_empty()
                            && task.repos.iter().all(|r| {
                                deleted.contains(&r.name)
                                    || prs.iter().any(|(repo, _, state)| {
                                        repo == &r.name && *state == sync::PrState::Merged
                                    })
                            }) {
                            Some(true)
                        } else {
                            known.then_some(false)
                        },
                        at: interaction::now_ms(),
                    };
                    if let Some(agent) = s.state.agents.iter_mut().find(|a| a.slug == slug) {
                        agent.history.insert(0, record);
                        agent.history.truncate(100);
                    }
                }
                s.archived(&name, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Drops the entities and caches of an archived task.
    fn forget_task_views(&mut self, name: &str, cx: &mut Context<Self>) {
        // A later task with the same name must not replay or resume these chats.
        if let Some(ws) = self.state.tasks.iter().find(|w| w.name == name) {
            for c in ws.chats.iter().chain(&ws.closed) {
                self.chats.remove(&c.id);
                self.unread.remove(&c.id);
                crate::transcript::forget(&c.id);
                crate::plan::delete(&c.id);
            }
        }
        self.sibling_refreshes.remove(name);
        self.syncs.remove(name);
        self.parked_sessions.remove(name);
        self.git_features.remove(name);
        self.attach_results.remove(name);
        self.attn.forget(name);
        self.differ.update(cx, |d, _| d.forget_task(name));
    }

    pub(super) fn archived(
        &mut self,
        name: &str,
        result: Result<Vec<String>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(git) = self.git_features.get(name) {
            git.update(cx, |g, cx| {
                g.set_archiving(false, cx);
                cx.notify();
            });
        }
        match result {
            Ok(deleted) => {
                let title = self.task_title(name);
                let repoless = self.task(name).is_some_and(|w| w.repos.is_empty());
                let before = self.cursors(cx);
                self.forget_task_views(name, cx);
                let agent = self
                    .task(name)
                    .and_then(|t| t.agent.as_ref())
                    .map(|a| a.slug.clone());
                self.state.remove_task(name);
                if let Some(slug) = agent {
                    let _ = self.sync_agent(&slug, false, cx);
                }
                crate::linear::delete_snapshots(name);
                let _ = std::fs::remove_file(crate::scripts::log_path(name));
                // Names come back around, and a later agent must not find these.
                let _ = std::fs::remove_file(crate::features::git::drafts_path(name));
                let _ = self.state.save();
                if self.active.as_deref() == Some(name) {
                    self.active = None;
                    self.zen = false;
                    self.drop_editor(cx);
                    self.reset_differ_task(window, cx);
                    self.tab = Tab::Chat;
                    self.sidebar_focus.focus(window, cx);
                }
                self.restore_cursors(name, before, cx);
                let branch = match deleted.is_empty() {
                    true if repoless => String::new(),
                    true => " (branch kept)".to_string(),
                    false => format!(" (merged branch deleted in {})", deleted.join(", ")),
                };
                window.push_notification(
                    Notification::success(format!("Deleted {title}{branch}")),
                    cx,
                );
            }
            Err(e) => {
                for chat in self.started_chats(name) {
                    chat.update(cx, |c, _| c.archiving = false);
                }
                notify_err(
                    window,
                    cx,
                    format!("{e}\nUse Force delete task in cmd-k to discard them."),
                )
            }
        }
        cx.notify();
    }
}

/// A git action on an agent with nothing to act on.
pub(super) const NO_REPO: &str = "No repository attached; attach one first";

/// `(repo, old base, new base)` for the repos of the task inserted below that sit on
/// the parent branch the new task stacks on.
fn insert_moves(
    child: &Task,
    parents: &[(String, String)],
    repos: &[(Repo, String)],
) -> Vec<(String, String, String)> {
    (child.repos.iter())
        .filter_map(|cr| {
            let (_, p) = parents.iter().find(|(n, _)| *n == cr.name)?;
            let (_, b) = repos.iter().find(|(r, _)| r.name == cr.name)?;
            (cr.base == format!("origin/{p}"))
                .then(|| (cr.name.clone(), cr.base.clone(), format!("origin/{b}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TaskRepo;
    use core::prelude::v1::test; // the gpui_kit glob exports a `test` macro

    #[test]
    fn insert_moves_picks_repos_on_the_shared_parent() {
        let wr = |name: &str, base: &str| TaskRepo {
            name: name.into(),
            src: PathBuf::new(),
            base: base.into(),
            branch: None,
        };
        let child = Task {
            repos: vec![
                wr("a", "origin/p"),
                wr("b", "origin/other"),
                wr("c", "origin/p"),
            ],
            ..Default::default()
        };
        let repo = |n: &str| Repo {
            name: n.into(),
            path: PathBuf::new(),
        };
        let parents = vec![("a".to_string(), "p".to_string()), ("b".into(), "p".into())];
        let repos = vec![(repo("a"), "new".to_string()), (repo("b"), "new".into())];
        assert_eq!(
            insert_moves(&child, &parents, &repos),
            [(
                "a".to_string(),
                "origin/p".to_string(),
                "origin/new".to_string()
            )]
        );
        assert!(insert_moves(&child, &[], &repos).is_empty());
    }
}
