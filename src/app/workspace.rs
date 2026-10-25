//! Creating, opening, growing and archiving workspaces.
use super::*;

/// Contents of the New workspace dialog: a branch name and a filterable repo picker.
/// Starts with the last-used repos ticked and floated to the top, so the common case
/// is type a branch, enter.
pub(super) struct NewWorkspaceForm {
    branch: Entity<InputState>,
    /// Optional first message, sent once the workspace is created.
    prompt: Entity<InputState>,
    picker: Entity<CommandState>,
    repos: Vec<Repo>,
    picked: Vec<bool>,
    shark: WeakEntity<Shark>,
    _sub: Subscription,
}

impl NewWorkspaceForm {
    fn new(
        mut repos: Vec<Repo>,
        last: &[String],
        shark: WeakEntity<Shark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch = cx
            .new(|cx| InputState::new(window, cx).placeholder("branch, e.g. kasper/eng-123-login"));
        // Branch names can't contain spaces; turn them into dashes as you type.
        let _sub = cx.subscribe_in(&branch, window, |_, input, ev: &InputEvent, window, cx| {
            if let InputEvent::Change = ev {
                let value = input.read(cx).value();
                if value.contains(' ') {
                    let fixed = value.replace(' ', "-");
                    input.update(cx, |i, cx| i.set_value(fixed, window, cx));
                }
            }
        });
        let only = repos.len() == 1;
        let was_picked = |r: &Repo| only || last.contains(&r.name);
        repos.sort_by_key(|r| !was_picked(r)); // stable: picked first, otherwise in added order
        let picked = repos.iter().map(was_picked).collect();
        NewWorkspaceForm {
            branch,
            prompt: cx.new(|cx| {
                InputState::new(window, cx).placeholder("First message to the agent (optional)")
            }),
            picker: cx.new(|cx| CommandState::new(window, cx)),
            repos,
            picked,
            shark,
            _sub,
        }
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.picked.get_mut(ix) {
            *p = !*p;
            cx.notify();
        }
    }

    fn picked_repos(&self) -> Vec<Repo> {
        self.repos
            .iter()
            .zip(&self.picked)
            .filter(|(_, p)| **p)
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// Returns whether the form was valid (and the dialog may close).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let branch = self.branch.read(cx).value().trim().to_string();
        let repos = self.picked_repos();
        if branch.is_empty() || repos.is_empty() {
            let (msg, focus_branch) = match branch.is_empty() {
                true => ("Name the branch first", true),
                false => ("Pick at least one repo (space or enter ticks it)", false),
            };
            notify_err(window, cx, msg);
            match focus_branch {
                true => self.branch.update(cx, |i, cx| i.focus(window, cx)),
                false => self.picker.update(cx, |p, cx| p.focus(window, cx)),
            }
            return false;
        }
        let prompt = self.prompt.read(cx).value().trim().to_string();
        let prompt = (!prompt.is_empty()).then_some(prompt);
        self.shark
            .update(cx, |s, cx| {
                s.create_workspace(branch, repos, prompt, window, cx)
            })
            .ok();
        true
    }
}

impl Render for NewWorkspaceForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, faint) = (theme.muted_foreground, faint(theme));
        let in_branch = self.branch.read(cx).focus_handle(cx).is_focused(window)
            || self.prompt.read(cx).focus_handle(cx).is_focused(window);
        let items = self.repos.iter().zip(&self.picked).map(|(r, p)| {
            CommandItem::new()
                .label(r.name.clone())
                .icon(IconName::FolderGit2)
                .keywords([r.path.to_string_lossy().to_string()])
                .checked(*p)
        });
        let entity = cx.entity().downgrade();
        let picker = self.picker.clone();
        let count = self.picked.iter().filter(|p| **p).count();
        let hint = |keys: Vec<Kbd>, label: &'static str| {
            h_flex()
                .gap_1()
                .children(keys)
                .child(div().pl_0p5().child(label))
        };
        v_flex()
            .key_context("NewWorkspace")
            .gap_3()
            .on_action(cx.listener(|this, _: &SwitchField, window, cx| {
                // branch -> repos -> first message -> branch
                if this.branch.read(cx).focus_handle(cx).is_focused(window) {
                    this.picker.update(cx, |p, cx| p.focus(window, cx))
                } else if this.prompt.read(cx).focus_handle(cx).is_focused(window) {
                    this.branch.update(cx, |i, cx| i.focus(window, cx))
                } else {
                    this.prompt.update(cx, |i, cx| i.focus(window, cx))
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRepo, _, cx| {
                if let Some(ix) = this.picker.read(cx).selected_index() {
                    this.toggle(ix.row, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SubmitForm, window, cx| {
                if this.submit(window, cx) {
                    window.close_dialog(cx);
                }
            }))
            .child(Input::new(&self.branch))
            .child(
                h_flex()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().child("Repos"))
                    .child(format!("{count} of {}", self.repos.len())),
            )
            .child(
                Command::new(&self.picker)
                    .items(items)
                    .placeholder("Filter repos…")
                    .bordered(true)
                    .max_h(px(240.))
                    // Enter ticks the highlighted repo and clears the filter for the next one.
                    .on_confirm(move |ix, window, cx| {
                        entity.update(cx, |f, cx| f.toggle(ix.row, cx)).ok();
                        picker.update(cx, |p, cx| p.set_query("", window, cx));
                    }),
            )
            .child(Input::new(&self.prompt))
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(faint)
                    .child(hint(vec![kbd("tab")], "switch field"))
                    .child(match in_branch {
                        true => hint(vec![kbd("enter")], "create"),
                        false => hint(vec![kbd("space")], "tick repo"),
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("create")
                            .primary()
                            .label("Create")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.submit(window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}

impl Shark {
    pub(crate) fn open_workspace(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.state.workspaces.get(ix).map(|w| w.name.clone()) else {
            return;
        };
        if self.active.as_ref() == Some(&name) || self.editor.is_none() {
            return self.open_workspace_now(ix, window, cx);
        }
        self.confirm_discard(window, cx, move |s, window, cx| {
            s.drop_editor();
            s.open_workspace_now(ix, window, cx)
        });
    }

    pub(super) fn open_workspace_now(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ws) = self.state.workspaces.get(ix).cloned() else {
            return;
        };
        if self.active.as_ref() != Some(&ws.name) {
            // A fresh tree and diff for another workspace.
            self.diff_fp = 0;
            self.diff_files.clear();
            self.diff_fps.clear();
            self.viewed_now = Rc::default();
            self.diff_file = 0;
            self.diff_loaded = false;
            self.diff_error = None;
            self.tree_rows.clear();
            self.collapsed_dirs.clear();
            self.tree_sel = 0;
            self.lose_view();
        }
        self.sel = SideRow::Ws(ix);
        if let Some(i) = self.section_of(&ws.name)
            && self.state.sections[i].collapsed
        {
            self.state.sections[i].collapsed = false;
            self.state.save();
        }
        if !self.syncs.contains_key(&ws.name) {
            let sync = cx.new(|cx| GitSync::new(ws.clone(), window, cx));
            let name = ws.name.clone();
            self._subs.push(cx.subscribe_in(
                &sync,
                window,
                move |this, _, ev: &SyncEvent, window, cx| match ev {
                    SyncEvent::Archive => this.remove_workspace(&name, false, window, cx),
                },
            ));
            self.syncs.insert(ws.name.clone(), sync);
        }
        for (name, sync) in &self.syncs {
            let on = *name == ws.name;
            sync.update(cx, |g, cx| g.set_active(on, cx));
        }
        self.attn.opened(&ws.name);
        self.active = Some(ws.name);
        self.refresh_attention(cx);
        self.refresh_diff(cx);
        self.show(Tab::Chat, window, cx);
        self.select_chat(ws.active_chat, window, cx);
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

    /// From the folder picker or folders dropped on the sidebar.
    pub(super) fn add_repo_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for p in paths {
            match git::add_repo(p, &self.state.repos) {
                Ok(r) => self.state.repos.push(r),
                Err(e) => notify_err(window, cx, e),
            }
        }
        self.state.save();
        cx.notify();
    }

    /// `picked` repos start ticked: the last-used ones, or the repo clicked in the sidebar.
    pub(super) fn new_workspace(
        &mut self,
        picked: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.repos.is_empty() {
            return notify_err(window, cx, "Add a repo first (Add repo in cmd-k)");
        }
        let shark = cx.entity().downgrade();
        let repos = self.state.repos.clone();
        let form = cx.new(|cx| NewWorkspaceForm::new(repos, &picked, shark, window, cx));
        let input = form.read(cx).branch.clone();
        window.open_dialog(cx, move |d, _, _| {
            let f = form.clone();
            // Dialog binds enter to Confirm, which runs on_ok and closes when it returns true.
            // In the repo picker, the Command list takes enter first (it ticks a repo).
            d.font_features(inter_features())
                .title("New workspace")
                .w(px(520.))
                .child(form.clone())
                .on_ok(move |_, window, cx| f.update(cx, |f, cx| f.submit(window, cx)))
        });
        input.update(cx, |i, cx| i.focus(window, cx));
    }

    /// `prompt`, when set, is sent to the agent once the workspace is open.
    pub(super) fn create_workspace(
        &mut self,
        branch: String,
        repos: Vec<Repo>,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = branch.replace('/', "-");
        if self.state.workspaces.iter().any(|w| w.name == name) {
            return notify_err(window, cx, format!("workspace `{name}` already exists"));
        }
        self.state.last_repos = repos.iter().map(|r| r.name.clone()).collect();
        self.state.save();
        window.push_notification(format!("Creating {name}…"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = workspaces_dir().join(&name);
            let result = cx
                .background_executor()
                .spawn(async move { git::create_workspace(&dir, &name, &branch, &repos) })
                .await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(ws) => {
                    let name = ws.name.clone();
                    s.state.workspaces.push(ws);
                    s.state.save();
                    s.open_workspace(s.state.workspaces.len() - 1, window, cx);
                    if let Some(p) = prompt
                        && s.active.as_ref() == Some(&name)
                        && let Some(chat) = s.active_chat().cloned()
                    {
                        chat.update(cx, |c, cx| c.ask(&p, window, cx));
                    }
                }
                Err(e) => notify_err(window, cx, e),
            })
            .ok();
        })
        .detach();
    }

    /// Launch: reopen the workspace and tab that were open at quit.
    pub(super) fn restore_last_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((name, tab)) = self.state.last_open.clone() else {
            return;
        };
        let Some(ix) = self.state.workspaces.iter().position(|w| w.name == name) else {
            return;
        };
        self.open_workspace_now(ix, window, cx);
        if tab == "diff" {
            self.show(Tab::Diff, window, cx);
        }
    }

    /// Remembers the open workspace and tab in state.json, for the next launch.
    pub(super) fn remember_open(&mut self) {
        let tab = match self.tab {
            Tab::Diff => "diff",
            Tab::Chat | Tab::File => "chat",
        };
        let open = self.active.clone().map(|n| (n, tab.to_string()));
        if self.state.last_open != open {
            self.state.last_open = open;
            self.state.save();
        }
    }

    /// "Add repo to workspace…": a menu of the repos the target workspace doesn't have yet.
    pub(super) fn pick_repo_for_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.target(window) else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let missing: Vec<Repo> = (self.state.repos.iter())
            .filter(|r| !ws.repos.iter().any(|w| w.name == r.name))
            .cloned()
            .collect();
        if missing.is_empty() {
            return notify_err(
                window,
                cx,
                format!("{} has every repo; add another first (Add repo in cmd-k)", ws.name),
            );
        }
        let items = missing
            .iter()
            .map(|r| {
                CommandItem::new()
                    .label(r.name.clone())
                    .icon(IconName::FolderGit2)
                    .keywords([r.path.to_string_lossy().to_string()])
            })
            .collect();
        let name = ws.name.clone();
        open_menu(
            cx.entity().downgrade(),
            "Add a repo to this workspace…",
            items,
            0,
            move |s, row, window, cx| {
                s.add_repo_to_workspace(name.clone(), missing[row].clone(), window, cx)
            },
            window,
            cx,
        );
    }

    pub(super) fn add_repo_to_workspace(
        &mut self,
        name: String,
        repo: Repo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.archiving.contains(&name) {
            return notify_err(window, cx, format!("{name} is being archived"));
        }
        let Some(ws) = self.ws(&name).cloned() else {
            return;
        };
        window.push_notification(format!("Adding {} to {name}…", repo.name), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = ws.dir();
            let result = cx
                .background_executor()
                .spawn(async move { git::add_to_workspace(&ws, &dir, &repo) })
                .await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(ws) => s.repo_added(ws, window, cx),
                Err(e) => notify_err(window, cx, e),
            })
            .ok();
        })
        .detach();
    }

    /// Saves the new repo list and tells the chat, the git button and the diff.
    fn repo_added(&mut self, ws: Workspace, window: &mut Window, cx: &mut Context<Self>) {
        let Some(slot) = self.ws_mut(&ws.name) else {
            return;
        };
        // Only the repos change; session, model and viewed may have moved on meanwhile.
        // Merge rather than replace: another add may have finished since this one started.
        let Some(r) = ws.repos.last().cloned() else {
            return;
        };
        let raced = slot.repos.len() + 1 != ws.repos.len();
        if !slot.repos.iter().any(|w| w.name == r.name) {
            slot.repos.push(r.clone());
        }
        let ws = slot.clone();
        self.state.save();
        if raced {
            git::write_claude_md(&ws);
        }
        let rules = match ["AGENTS.md", "CLAUDE.md"]
            .into_iter()
            .find(|f| ws.dir().join(&r.name).join(f).exists())
        {
            Some(f) => format!(" Read {}/{f} for its rules.", r.name),
            None => String::new(),
        };
        let note = format!(
            "[shark] Repo `{0}` joined this workspace: `{0}/` is a git worktree of {1} on branch `{2}` (base `{3}`), and the root CLAUDE.md and AGENTS.md now list it.{rules}",
            r.name,
            r.src.display(),
            ws.branch,
            r.base
        );
        // Every chat of an opened workspace that has (or had) a conversation is told; one never
        // started reads the new root CLAUDE.md at its session start.
        if self.syncs.contains_key(&ws.name) {
            let metas: Vec<ChatMeta> = (ws.chats.iter())
                .filter(|m| self.chats.contains_key(&m.id) || m.session_id.is_some())
                .cloned()
                .collect();
            for m in metas {
                let chat = self.ensure_chat(&ws, &m, window, cx);
                chat.update(cx, |c, cx| c.repos_changed(&ws, note.clone(), cx));
            }
        }
        if let Some(sync) = self.syncs.get(&ws.name) {
            sync.update(cx, |g, cx| g.set_workspace(ws.clone(), cx));
        }
        if self.active.as_ref() == Some(&ws.name) {
            self.refresh_diff(cx);
        }
        window.push_notification(
            Notification::success(format!("Added {} to {}", r.name, ws.name)),
            cx,
        );
        cx.notify();
    }

    /// Row actions hit the workspace under the sidebar cursor when the sidebar is focused, else the open one.
    pub(super) fn target(&self, window: &Window) -> Option<Workspace> {
        let name = match (self.sidebar_focus.is_focused(window), self.sel) {
            (true, SideRow::Ws(ix)) => self.state.workspaces.get(ix)?.name.clone(),
            _ => self.active.clone()?,
        };
        self.ws(&name).cloned()
    }

    pub(super) fn with_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&Workspace, &mut Window, &mut App),
    ) {
        match self.target(window) {
            Some(ws) => f(&ws, window, cx),
            None => notify_err(window, cx, "Open a workspace first"),
        }
    }

    pub(super) fn archive(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.target(window) {
            self.archive_ws(ws.name, force, window, cx)
        }
    }

    pub(super) fn archive_ws(
        &mut self,
        name: String,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running_chats(&name, cx) > 0 {
            // ctrl-c stops the shown chat's agent only.
            let msg = match self.active.as_deref() == Some(name.as_str()) {
                true => "An agent is running; stop it first (ctrl-c in each running chat)".into(),
                false => {
                    format!("An agent in {name} is running; open it and stop it first (ctrl-c)")
                }
            };
            return notify_err(window, cx, msg);
        }
        if !force {
            return self.remove_workspace(&name, false, window, cx);
        }
        confirm(
            format!("Force-archive {name}?"),
            "Removes its worktrees including uncommitted changes. Branches are kept.",
            "Archive",
            move |s, window, cx| s.remove_workspace(&name, true, window, cx),
            window,
            cx,
        );
    }

    pub(super) fn remove_workspace(
        &mut self,
        name: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Git only sees the disk; unsaved editor edits would vanish with the worktree.
        if self.active.as_deref() == Some(name)
            && self.editor.as_ref().is_some_and(|e| e.read(cx).dirty)
        {
            let name = name.to_string();
            return self.confirm_discard(window, cx, move |s, window, cx| {
                s.remove_workspace(&name, force, window, cx)
            });
        }
        let Some(ws) = self.ws(name).cloned() else {
            return;
        };
        // The merge step and the menu can both ask while a removal runs.
        if !self.archiving.insert(ws.name.clone()) {
            return;
        }
        for chat in self.started_chats(name) {
            chat.update(cx, |c, _| c.archiving = true);
        }
        // Force-archive keeps every branch, as its dialog says.
        let merged = match force {
            true => vec![],
            false => self
                .syncs
                .get(name)
                .map_or(vec![], |g| g.read(cx).merged_branches()),
        };
        // `worktree remove` deletes node_modules, target and the like: seconds, off the UI thread.
        window.push_notification(format!("Archiving {name}…"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let dir = ws.dir();
            let name = ws.name.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    git::remove_workspace(&ws, &dir, force)
                        .map(|()| git::delete_merged_branches(&ws, &merged))
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.archiving.remove(&name);
                s.archived(&name, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn archived(
        &mut self,
        name: &str,
        result: Result<Vec<String>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(deleted) => {
                let pos = self.side_rows(false).iter().position(|r| *r == self.sel);
                // A later workspace with the same name must not replay or resume these chats.
                if let Some(ws) = self.state.workspaces.iter().find(|w| w.name == name) {
                    for c in ws.chats.iter().chain(&ws.closed) {
                        self.chats.remove(&c.id);
                        self.unread.remove(&c.id);
                        let _ = std::fs::remove_file(crate::agent::transcript_path(&c.id));
                        crate::plan::delete(&c.id);
                    }
                }
                self.syncs.remove(name);
                self.state.workspaces.retain(|w| w.name != name);
                for s in &mut self.state.sections {
                    s.workspaces.retain(|w| w != name);
                }
                self.state.save();
                if self.active.as_deref() == Some(name) {
                    self.active = None;
                    self.drop_editor();
                    self.tab = Tab::Chat;
                    self.sidebar_focus.focus(window, cx);
                }
                // The cursor stays at the same height, on whatever row moved up into it.
                let rows = self.side_rows(false);
                self.sel = pos
                    .and_then(|p| rows.get(p.min(rows.len().saturating_sub(1))))
                    .copied()
                    .unwrap_or(SideRow::Ws(0));
                let branch = match deleted.is_empty() {
                    true => "branch kept".to_string(),
                    false => format!("merged branch deleted in {}", deleted.join(", ")),
                };
                window.push_notification(
                    Notification::success(format!("Archived {name} ({branch})")),
                    cx,
                );
            }
            Err(e) => {
                for chat in self.started_chats(name) {
                    chat.update(cx, |c, _| c.archiving = false);
                }
                notify_err(window, cx, format!("{e}\nUse Force archive workspace in cmd-k to discard them."))
            }
        }
        cx.notify();
    }
}
