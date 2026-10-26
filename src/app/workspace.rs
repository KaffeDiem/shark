//! Creating, opening, growing and archiving workspaces.
use super::*;

/// Contents of the New workspace dialog: a branch name and a filterable repo picker.
/// Starts with the last-used repos ticked and floated to the top, so the common case
/// is type a branch, enter. cmd-b swaps the list for a repo's branches, to check one out
/// there instead of the new branch.
pub(super) struct NewWorkspaceForm {
    branch: Entity<InputState>,
    /// Home's prompt, sent once the workspace is created.
    first: Option<String>,
    picker: Entity<CommandState>,
    repos: Vec<Repo>,
    picked: Vec<bool>,
    /// Per repo: the existing branch picked for it, instead of the new one.
    own: Vec<Option<String>>,
    /// Per repo: its branches, `None` until listed.
    branches: Vec<Option<Vec<git::Branch>>>,
    /// Per repo: fetched once its branch list was opened.
    fetched: Vec<bool>,
    mode: Mode,
    shark: WeakEntity<Shark>,
    _sub: Subscription,
}

/// What the picker lists: the repos, or the branches of repo `n`.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Repos,
    Branches(usize),
}

impl NewWorkspaceForm {
    fn new(
        mut repos: Vec<Repo>,
        last: &[String],
        first: Option<String>,
        shark: WeakEntity<Shark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch = cx
            .new(|cx| InputState::new(window, cx).placeholder("branch, e.g. you/eng-123-login"));
        // Branch names can't contain spaces; turn them into dashes as you type.
        let _sub = cx.subscribe_in(&branch, window, |_, input, ev: &InputEvent, window, cx| {
            if let InputEvent::Change = ev {
                let value = input.read(cx).value();
                if value.contains(' ') {
                    let fixed = value.replace(' ', "-");
                    input.update(cx, |i, cx| i.set_value(fixed, window, cx));
                }
                cx.notify(); // the worktree path line and each repo's new/existing
            }
        });
        let only = repos.len() == 1;
        let was_picked = |r: &Repo| only || last.contains(&r.name);
        repos.sort_by_key(|r| !was_picked(r)); // stable: picked first, otherwise in added order
        let picked = repos.iter().map(was_picked).collect();
        // Local refs only, so it's quick: whether the typed name exists, and the lists.
        let srcs: Vec<PathBuf> = repos.iter().map(|r| r.path.clone()).collect();
        cx.spawn(async move |this, cx| {
            let lists = (cx.background_executor())
                .spawn(async move { git::par_map(&srcs, |s| git::branches(s).ok()) })
                .await;
            this.update(cx, |f, cx| {
                for (slot, l) in f.branches.iter_mut().zip(lists) {
                    // A fetched list may have landed first.
                    if slot.is_none() {
                        *slot = l;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        let n = repos.len();
        NewWorkspaceForm {
            branch,
            first,
            picker: cx.new(|cx| CommandState::new(window, cx)),
            repos,
            picked,
            own: vec![None; n],
            branches: vec![None; n],
            fetched: vec![false; n],
            mode: Mode::Repos,
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

    /// Switches the list to `mode` with `row` highlighted, or the first row when the list
    /// shown now is shorter. The highlight keeps its row number into the next list, so it is
    /// aimed here, against the current list with its filter cleared.
    fn show(&mut self, mode: Mode, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.picker.update(cx, |p, cx| {
            p.set_query("", window, cx);
            let row = if row < p.matched_count() { row } else { 0 };
            p.set_selected_index(Some(IndexPath::new(row)), window, cx);
            p.focus(window, cx)
        });
        cx.notify();
    }

    /// Lists repo `ix`'s branches; a fetch refreshes them the first time.
    fn pick_branch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.show(Mode::Branches(ix), 0, window, cx);
        if !std::mem::replace(&mut self.fetched[ix], true) {
            let src = self.repos[ix].path.clone();
            cx.spawn_in(window, async move |this, cx| {
                let list = (cx.background_executor())
                    .spawn(async move {
                        let fetch = ["fetch", "--quiet", "--prune", "origin"];
                        let _ = git::cmd_bounded(&src, "git", &fetch, 30);
                        git::branches(&src).ok()
                    })
                    .await;
                this.update_in(cx, |f, window, cx| f.refreshed(ix, list, window, cx))
                    .ok();
            })
            .detach();
        }
    }

    /// Repo `ix`'s fetched branches. Newer ones may come first, so a highlighted branch is
    /// followed to its new row; enter must not pick whatever moved under it.
    fn refreshed(
        &mut self,
        ix: usize,
        list: Option<Vec<git::Branch>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(list) = list else { return };
        let row = self.picker.read(cx).selected_index().map(|i| i.row);
        let was = (row.and_then(|r| r.checked_sub(1)))
            .and_then(|i| self.branches[ix].as_ref()?.get(i))
            .map(|b| b.name.clone());
        if let (Mode::Branches(shown), Some(was)) = (self.mode, was)
            && shown == ix
        {
            // Resolved against the list shown now; a row it filters out highlights nothing.
            let at = list.iter().position(|b| b.name == was);
            let at = at.map(|i| IndexPath::new(i + 1));
            self.picker
                .update(cx, |p, cx| p.set_selected_index(at, window, cx));
        }
        self.branches[ix] = Some(list);
        cx.notify();
    }

    /// Back to the repo list, on the repo whose branches were listed.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::Branches(ix) = self.mode else { return };
        self.show(Mode::Repos, ix, window, cx);
    }

    /// Enter on row `row`: ticks a repo, or picks a branch for the listed repo and goes back.
    fn choose(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Mode::Branches(ix) = self.mode else {
            self.toggle(row, cx);
            return self.picker.update(cx, |p, cx| p.set_query("", window, cx));
        };
        // Row 0 is the new branch.
        let b = (row.checked_sub(1))
            .and_then(|i| self.branches[ix].as_ref()?.get(i))
            .cloned();
        if let Some(git::Branch {
            name,
            checked_out: Some(at),
            ..
        }) = &b
        {
            let msg = format!(
                "{}: `{name}` is checked out at {at}. Free it first (e.g. git -C {at} switch main), or pick another branch.",
                self.repos[ix].name
            );
            return notify_err(window, cx, msg);
        }
        self.own[ix] = b.map(|b| b.name);
        self.picked[ix] = true;
        self.back(window, cx);
    }

    /// The workspace's name and branch: the typed one, else the first picked existing branch
    /// when every ticked repo has one.
    fn primary(&self, cx: &App) -> Option<String> {
        let typed = self.branch.read(cx).value().trim().to_string();
        if !typed.is_empty() {
            return Some(typed);
        }
        let mut own = (self.picked.iter().zip(&self.own)).filter(|(p, _)| **p);
        let first = own.next()?.1.clone();
        own.all(|(_, o)| o.is_some()).then_some(first).flatten()
    }

    /// What repo `ix` will be on, as its row says: the picked branch, or the typed one, `new`
    /// or `existing` there.
    fn gets(&self, ix: usize, cx: &App) -> String {
        if let Some(b) = &self.own[ix] {
            return b.clone();
        }
        let typed = self.branch.read(cx).value();
        let typed = typed.trim();
        let exists = (self.branches[ix].as_ref())
            .is_some_and(|l| !typed.is_empty() && l.iter().any(|b| b.name == typed));
        if exists { "existing" } else { "new" }.into()
    }

    /// Returns whether the form was valid (and the dialog may close).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let primary = self.primary(cx);
        let picked: Vec<(Repo, Option<String>)> = (self.repos.iter())
            .zip(&self.picked)
            .zip(&self.own)
            .filter(|((_, p), _)| **p)
            .map(|((r, _), o)| (r.clone(), o.clone()))
            .collect();
        let (Some(branch), false) = (primary.clone(), picked.is_empty()) else {
            let (msg, focus_branch) = match primary {
                None => ("Name the branch first", true),
                Some(_) => ("Pick at least one repo (space or enter ticks it)", false),
            };
            notify_err(window, cx, msg);
            match focus_branch {
                true => self.branch.update(cx, |i, cx| i.focus(window, cx)),
                false => self.picker.update(cx, |p, cx| p.focus(window, cx)),
            }
            return false;
        };
        let repos = (picked.into_iter())
            .map(|(r, o)| (r, o.unwrap_or_else(|| branch.clone())))
            .collect();
        let prompt = self.first.clone();
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
        let in_branch = self.branch.read(cx).focus_handle(cx).is_focused(window);
        let entity = cx.entity().downgrade();
        let items: Vec<CommandItem> = match self.mode {
            // A checkbox on every row, so it reads as multi-select before anything is ticked.
            // The trailing label says what the repo gets; clicking it lists its branches.
            Mode::Repos => (self.repos.iter().zip(&self.picked).enumerate())
                .map(|(i, (r, &p))| {
                    let name = r.name.clone();
                    let gets = self.gets(i, cx);
                    let entity = entity.clone();
                    CommandItem::new()
                        .label(r.name.clone())
                        .keywords([r.path.to_string_lossy().to_string()])
                        .child(move |_, _| {
                            let entity = entity.clone();
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .items_center()
                                .child(Checkbox::new(("repo-pick", i)).small().checked(p))
                                .child(name.clone())
                                .child(
                                    div()
                                        .id(("repo-branch", i))
                                        .ml_auto()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(muted)
                                        .cursor_pointer()
                                        .child(gets.clone())
                                        .aria_label(format!("{name} branch: {gets}"))
                                        .on_click(move |_, window, cx| {
                                            cx.stop_propagation();
                                            entity
                                                .update(cx, |f, cx| f.pick_branch(i, window, cx))
                                                .ok();
                                        })
                                        .test_support(),
                                )
                        })
                })
                .collect(),
            Mode::Branches(ix) => {
                let list = self.branches[ix].clone().unwrap_or_default();
                let new = CommandItem::new().label("New branch (workspace name)");
                (std::iter::once(new))
                    .chain(list.into_iter().map(|b| {
                        let tag = match &b.checked_out {
                            Some(at) => format!("checked out at {}", tilde(Path::new(at))),
                            None => (b.local.then_some("local").into_iter())
                                .chain(b.origin.then_some("origin"))
                                .collect::<Vec<_>>()
                                .join(" · "),
                        };
                        let name = b.name.clone();
                        CommandItem::new().label(b.name).child(move |_, _| {
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(div().min_w_0().truncate().child(name.clone()))
                                .child(
                                    div()
                                        .ml_auto()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(tag.clone()),
                                )
                        })
                    }))
                    .collect()
            }
        };
        let count = self.picked.iter().filter(|p| **p).count();
        let hint = |keys: Vec<Kbd>, label: &'static str| {
            h_flex()
                .gap_1()
                .children(keys)
                .child(div().pl_0p5().child(label))
        };
        let (heading, placeholder) = match self.mode {
            Mode::Repos => (
                "Repos".to_string(),
                "Filter repos…".to_string(),
            ),
            Mode::Branches(ix) => (
                format!("Branch for {}", self.repos[ix].name),
                match self.branches[ix].is_some() {
                    true => "Filter branches…".to_string(),
                    false => "Loading branches…".to_string(),
                },
            ),
        };
        v_flex()
            .key_context("NewWorkspace")
            .gap_3()
            .on_action(cx.listener(|this, _: &SwitchField, window, cx| {
                // branch <-> repos; out of a branch list, back to the repos
                if this.mode != Mode::Repos {
                    this.back(window, cx)
                } else if this.branch.read(cx).focus_handle(cx).is_focused(window) {
                    this.picker.update(cx, |p, cx| p.focus(window, cx))
                } else {
                    this.branch.update(cx, |i, cx| i.focus(window, cx))
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleRepo, _, cx| {
                if this.mode == Mode::Repos
                    && let Some(ix) = this.picker.read(cx).selected_index()
                {
                    this.toggle(ix.row, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PickBranch, window, cx| {
                if this.mode == Mode::Repos
                    && let Some(ix) = this.picker.read(cx).selected_index()
                {
                    this.pick_branch(ix.row, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SubmitForm, window, cx| {
                if this.submit(window, cx) {
                    window.close_dialog(cx);
                }
            }))
            .child(Input::new(&self.branch))
            .child({
                // As `create_workspace` names it.
                let name = self.primary(cx).unwrap_or_default().replace('/', "-");
                let dir = workspaces_dir().join(if name.is_empty() { "<name>" } else { &name });
                div()
                    .text_xs()
                    .text_color(faint)
                    .truncate()
                    .child(format!("Worktrees go in {}", super::menus::tilde(&dir)))
            })
            .child(
                h_flex()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().child(heading))
                    .when(self.mode == Mode::Repos, |d| {
                        d.child(format!("{count} of {}", self.repos.len()))
                    }),
            )
            .child(
                Command::new(&self.picker)
                    .items(items)
                    .placeholder(placeholder)
                    .bordered(true)
                    .max_h(px(240.))
                    // Enter ticks the highlighted repo and clears the filter for the next one,
                    // or picks the highlighted branch.
                    .on_confirm(move |ix, window, cx| {
                        entity.update(cx, |f, cx| f.choose(ix.row, window, cx)).ok();
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(faint)
                    .map(|d| match (self.mode, in_branch) {
                        (Mode::Branches(_), _) => d
                            .child(hint(vec![kbd("enter")], "pick branch"))
                            .child(hint(vec![kbd("tab")], "back")),
                        (Mode::Repos, true) => d
                            .child(hint(vec![kbd("tab")], "switch field"))
                            .child(hint(vec![kbd("enter")], "create")),
                        (Mode::Repos, false) => d
                            .child(hint(vec![kbd("tab")], "switch field"))
                            .child(hint(vec![kbd("space")], "tick repo"))
                            .child(hint(vec![kbd("cmd-b")], "branch")),
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
        if self.active.as_ref() == Some(&name) || (self.editor.is_none() && self.editing.is_none())
        {
            return self.open_workspace_now(ix, window, cx);
        }
        self.confirm_discard(true, true, window, cx, move |s, window, cx| {
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
            self.editing = None;
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
            // They act on the open workspace's comments: not another one's.
            self.review.composer = None;
            self.review.sheet = None;
            self.close_share(window, cx);
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
                    SyncEvent::PrsCreated => this.state.mark(|s| &mut s.opened_pr),
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
    /// `first` fills in the first message (Home's prompt box).
    pub(super) fn new_workspace(
        &mut self,
        picked: Vec<String>,
        first: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.repos.is_empty() {
            return notify_err(window, cx, "Add a repo first (Add repo in cmd-k)");
        }
        let shark = cx.entity().downgrade();
        let repos = self.state.repos.clone();
        let form = cx.new(|cx| NewWorkspaceForm::new(repos, &picked, first, shark, window, cx));
        let input = form.read(cx).branch.clone();
        window.open_dialog(cx, move |d, _, _| {
            let f = form.clone();
            // Dialog binds enter to Confirm, which runs on_ok and closes when it returns true.
            // In the repo picker, the Command list takes enter first (it ticks a repo).
            d.font_features(inter_features())
                .entrance(DialogEntrance::None)
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
        repos: Vec<(Repo, String)>,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = branch.replace('/', "-");
        // Nothing is saved this run, so the worktrees would be orphaned on restart.
        if self.state.read_only {
            return notify_err(window, cx, "Can't create workspaces: state.json couldn't be read");
        }
        if self.state.workspaces.iter().any(|w| w.name == name) {
            return notify_err(window, cx, format!("workspace `{name}` already exists"));
        }
        self.state.last_repos = repos.iter().map(|(r, _)| r.name.clone()).collect();
        self.state.save();
        let dir = workspaces_dir().join(&name);
        window.push_notification(format!("Creating {}…", tilde(&dir)), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { git::create_workspace(&dir, &name, &branch, &repos) })
                .await;
            this.update_in(cx, |s, window, cx| match result {
                Ok(mut ws) => {
                    ws.fill_models(&s.state.new_model(), s.state.last_effort.as_ref());
                    let name = ws.name.clone();
                    s.state.workspaces.push(ws);
                    s.state.save();
                    s.open_workspace(s.state.workspaces.len() - 1, window, cx);
                    s.home_prompt_used(prompt.as_deref(), window, cx);
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
    /// Nothing to reopen, or Home was shown at quit: Home.
    pub(super) fn restore_last_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.state.last_open.clone();
        let ix = last
            .as_ref()
            .and_then(|(name, _)| self.state.workspaces.iter().position(|w| &w.name == name));
        let Some((ix, (_, tab))) = ix.zip(last) else {
            return self.enter_home(window, cx);
        };
        self.open_workspace_now(ix, window, cx);
        match tab.as_str() {
            "diff" => self.show(Tab::Diff, window, cx),
            "home" => self.enter_home(window, cx),
            _ => {}
        }
    }

    /// Remembers the open workspace and tab in state.json, for the next launch.
    pub(super) fn remember_open(&mut self) {
        let tab = match self.tab {
            _ if self.home.on => "home",
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

    /// Every chat of an opened workspace that has (or had) a conversation gets `note` and the
    /// new repo list; one never started reads the new root CLAUDE.md at its session start.
    fn tell_chats(&mut self, ws: &Workspace, note: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.syncs.contains_key(&ws.name) {
            return;
        }
        let metas: Vec<ChatMeta> = (ws.chats.iter())
            .filter(|m| self.chats.contains_key(&m.id) || m.session_id.is_some())
            .cloned()
            .collect();
        for m in metas {
            let chat = self.ensure_chat(ws, &m, window, cx);
            chat.update(cx, |c, cx| c.repos_changed(ws, note.clone(), cx));
        }
    }

    /// "Change base…": which repos (when there are several), then the new base, then whether
    /// the agent rebases onto it.
    pub(super) fn change_base(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.ws(&name).cloned() else {
            return;
        };
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
    fn pick_base(&mut self, name: String, repos: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.ws(&name).cloned() else {
            return;
        };
        let dir = ws.dir();
        // ponytail: for-each-ref on the UI thread; it's a local ref read, move to the background if it lags.
        let per_repo: Vec<(String, Vec<(i64, String)>)> = (repos.iter())
            .map(|r| (r.clone(), git::remote_branches(&dir.join(r))))
            .collect();
        let choices = git::base_choices(&per_repo, &ws.branch);
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

    /// Records the new bases (`(repo, old, new)`), retargets open PRs on GitHub, tells the
    /// chats, and with `rebase` asks the agent to move the commits.
    pub(super) fn set_base(
        &mut self,
        name: &str,
        moves: &[(String, String, String)],
        rebase: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sync = self.syncs.get(name).cloned();
        if rebase {
            let Some(sync) = &sync else {
                return notify_err(window, cx, "Open the workspace to rebase it");
            };
            if let Some(e) = sync.read(cx).blocked(true, cx) {
                return notify_err(window, cx, e);
            }
        }
        let Some(slot) = self.ws_mut(name) else {
            return;
        };
        for (repo, _, new) in moves {
            if let Some(r) = slot.repos.iter_mut().find(|r| &r.name == repo) {
                r.base = new.clone();
            }
        }
        let ws = slot.clone();
        self.state.save();
        git::write_claude_md(&ws);
        let changed = (moves.iter())
            .map(|(r, o, n)| format!("`{r}` now targets `{n}` (was `{o}`)"))
            .collect::<Vec<_>>()
            .join(", ");
        self.tell_chats(&ws, format!("[shark] New base: {changed}."), window, cx);
        if let Some(sync) = &sync {
            let prs: Vec<(PathBuf, String, String)> = (moves.iter())
                .filter_map(|(r, _, n)| {
                    let url = sync.read(cx).pr_for(r)?.0.url;
                    let b = n.strip_prefix("origin/").unwrap_or(n).to_string();
                    Some((ws.dir().join(r), url, b))
                })
                .collect();
            sync.update(cx, |g, cx| g.set_workspace(ws.clone(), cx));
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
                        cx.update(|w, cx| notify_err(w, cx, format!("Retargeting PRs: {}", errs.join("; "))))
                            .ok();
                    }
                })
                .detach();
            }
            if rebase {
                sync.update(cx, |g, cx| g.rebase_onto(moves, window, cx));
            }
        }
        if self.active.as_deref() == Some(name) {
            self.refresh_diff(cx);
        }
        window.push_notification(format!("New base: {changed}"), cx);
        cx.notify();
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
            ws.branch_of(&r),
            r.base
        );
        self.tell_chats(&ws, note, window, cx);
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
            // A heading or repo row is under the cursor: not the open workspace (cmd-backspace).
            (true, _) => return None,
            _ if self.home.focus.is_focused(window) => self.home.sel.clone()?,
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
            && (self.editor.as_ref().is_some_and(|e| e.read(cx).dirty)
                || self.editing.as_ref().is_some_and(|e| e.dirty))
        {
            let name = name.to_string();
            return self.confirm_discard(true, true, window, cx, move |s, window, cx| {
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
                self.attn.forget(name);
                self.review.forget(name);
                self.state.workspaces.retain(|w| w.name != name);
                for s in &mut self.state.sections {
                    s.workspaces.retain(|w| w != name);
                }
                self.state.save();
                if self.active.as_deref() == Some(name) {
                    self.active = None;
                    self.drop_editor();
                    self.editing = None;
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
