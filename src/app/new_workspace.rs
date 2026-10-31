//! The New workspace dialog: a branch name and a filterable repo or branch picker.
use super::*;
use gpui_kit::base::Selectable as _;
use std::time::{Duration, Instant};

/// Contents of the New workspace dialog: a branch name and a filterable repo picker.
/// Starts with the last-used repos ticked and floated to the top, so the common case
/// is type a branch, enter. cmd-b swaps the list for a repo's branches, to check one out
/// there instead of the new branch, or (alt-enter) to branch from it: a stack.
pub(super) struct NewWorkspaceForm {
    pub(super) branch: Entity<InputState>,
    /// Home's prompt, sent once the workspace is created.
    first: Option<String>,
    picker: Entity<CommandState>,
    repos: Vec<Repo>,
    picked: Vec<bool>,
    /// Per repo: the existing branch picked for it, instead of the new one.
    own: Vec<Option<String>>,
    /// Per repo: its branches, `None` until listed.
    branches: Vec<Option<Vec<git::Branch>>>,
    prs: Vec<Option<Vec<complete::Pr>>>,
    chosen_pr: Vec<Option<complete::Pr>>,
    /// Per repo: the branch the new branch is based on (stacked on), instead of the trunk.
    from: Vec<Option<String>>,
    /// "Insert workspace below…": the workspace to stack on the new one once it exists.
    insert: Option<String>,
    refreshed_at: Vec<[Option<Instant>; 2]>,
    loading: Vec<[bool; 2]>,
    errors: Vec<[Option<String>; 2]>,
    mode: Mode,
    pending_selection: Option<Option<IndexPath>>,
    shark: WeakEntity<Shark>,
    _sub: Subscription,
}

/// What the picker lists: the repos, or the branches of repo `n`.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Repos,
    Branches(usize),
    Prs(usize),
}

impl Mode {
    /// The repo index and the `[branches, prs]` slot the mode lists; `None` for the repos.
    fn slot(self) -> Option<(usize, usize)> {
        match self {
            Mode::Repos => None,
            Mode::Branches(ix) => Some((ix, 0)),
            Mode::Prs(ix) => Some((ix, 1)),
        }
    }
}

impl NewWorkspaceForm {
    /// `from` bases repos' new branch on another branch, `(repo, parent)`; `insert` names the
    /// workspace that moves on top of the new one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        mut repos: Vec<Repo>,
        last: &[String],
        first: Option<String>,
        from: &[(String, String)],
        insert: Option<String>,
        shark: WeakEntity<Shark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch =
            cx.new(|cx| InputState::new(window, cx).placeholder("branch, e.g. you/eng-123-login"));
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
                .spawn(async move { crate::proc::par_map(&srcs, |s| git::branches(s).ok()) })
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
        let from = (repos.iter())
            .map(|r| {
                from.iter()
                    .find(|(n, _)| *n == r.name)
                    .map(|(_, p)| p.clone())
            })
            .collect();
        NewWorkspaceForm {
            from,
            insert,
            branch,
            first,
            picker: cx.new(|cx| CommandState::new(window, cx)),
            repos,
            picked,
            own: vec![None; n],
            branches: vec![None; n],
            prs: vec![None; n],
            chosen_pr: vec![None; n],
            refreshed_at: vec![[None; 2]; n],
            loading: vec![[false; 2]; n],
            errors: vec![[None, None]; n],
            mode: Mode::Repos,
            pending_selection: None,
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
        self.pending_selection = None;
        self.picker.update(cx, |p, cx| {
            p.set_query("", window, cx);
            let row = if row < p.matched_count() { row } else { 0 };
            p.set_selected_index(Some(IndexPath::new(row)), window, cx);
            p.focus(window, cx)
        });
        cx.notify();
    }

    fn source_repo(&self, cx: &App) -> Option<usize> {
        match self.mode.slot() {
            None => self.picker.read(cx).selected_index().map(|i| i.row),
            Some((ix, _)) => Some(ix),
        }
    }

    fn pick_branch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.show(Mode::Branches(ix), 0, window, cx);
        self.refresh(false, window, cx);
    }

    fn pick_pr(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.prs[ix].is_none() {
            let repo = &self.repos[ix];
            self.prs[ix] = Some(complete::cached_repo_prs(&repo.name, &repo.path, true));
        }
        self.show(Mode::Prs(ix), 0, window, cx);
        self.refresh(false, window, cx);
    }

    fn refresh(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, kind)) = self.mode.slot() else {
            return;
        };
        if self.loading[ix][kind]
            || (!force
                && self.refreshed_at[ix][kind]
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(60)))
        {
            return;
        }
        self.loading[ix][kind] = true;
        self.errors[ix][kind] = None;
        let repo = self.repos[ix].clone();
        cx.spawn_in(window, async move |this, cx| {
            let (branches, prs) = cx
                .background_executor()
                .spawn(async move {
                    if kind == 0 {
                        let fetch_error = git::cmd_bounded(
                            &repo.path,
                            "git",
                            &["fetch", "--quiet", "--prune", "origin"],
                            30,
                        )
                        .err();
                        (Some((git::branches(&repo.path), fetch_error)), None)
                    } else {
                        (
                            None,
                            Some(complete::fetch_repo_prs(&repo.name, &repo.path, true)),
                        )
                    }
                })
                .await;
            this.update_in(cx, |f, window, cx| {
                f.loading[ix][kind] = false;
                f.refreshed_at[ix][kind] = Some(Instant::now());
                if let Some((result, fetch_error)) = branches {
                    f.errors[ix][kind] = fetch_error;
                    match result {
                        Ok(list) => f.refreshed(ix, Some(list), window, cx),
                        Err(e) => f.errors[ix][kind] = Some(e),
                    }
                }
                if let Some(result) = prs {
                    match result {
                        Ok(list) => {
                            if f.mode == Mode::Prs(ix) {
                                let was = f
                                    .picker
                                    .read(cx)
                                    .selected_index()
                                    .and_then(|i| f.prs[ix].as_ref()?.get(i.row))
                                    .map(|p| p.number);
                                if let Some(number) = was {
                                    let at = list
                                        .iter()
                                        .position(|p| p.number == number)
                                        .map(IndexPath::new);
                                    f.select_after_refresh(at, window, cx);
                                }
                            }
                            f.prs[ix] = Some(list);
                        }
                        Err(e) => f.errors[ix][kind] = Some(e),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Restore selection only after Command installs the refreshed model.
    fn select_after_refresh(
        &mut self,
        at: Option<IndexPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker
            .update(cx, |p, cx| p.set_selected_index(None, window, cx));
        self.pending_selection = Some(at);
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
            self.select_after_refresh(at, window, cx);
        }
        self.branches[ix] = Some(list);
        cx.notify();
    }

    /// alt-enter on a branch or PR: the repo's new branch starts on it and its PR will target
    /// it. The source must be on origin and in this repo (not a fork's PR).
    fn branch_from(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.picker.read(cx).selected_index().map(|i| i.row) else {
            return;
        };
        let (ix, parent) = match self.mode {
            Mode::Repos => return,
            Mode::Branches(ix) => {
                let Some(b) = (row.checked_sub(1)).and_then(|i| self.branches[ix].as_ref()?.get(i))
                else {
                    return;
                };
                if !b.origin {
                    let msg = format!("`{}` isn't on GitHub yet; push it first", b.name);
                    return notify_err(window, cx, msg);
                }
                (ix, b.name.clone())
            }
            Mode::Prs(ix) => {
                let Some(pr) = self.prs[ix].as_ref().and_then(|l| l.get(row)) else {
                    return;
                };
                if pr.is_cross_repository {
                    let msg = format!(
                        "#{} is from a fork; its branch isn't in this repo, so nothing can stack on it. Press enter to review it instead.",
                        pr.number
                    );
                    return notify_err(window, cx, msg);
                }
                (ix, pr.head_ref_name.clone())
            }
        };
        self.from[ix] = Some(parent);
        self.own[ix] = None;
        self.chosen_pr[ix] = None;
        self.picked[ix] = true;
        self.back(window, cx);
        // Stacking needs a new branch; the name field is where it goes.
        if self.branch.read(cx).value().trim().is_empty() {
            self.branch.update(cx, |i, cx| i.focus(window, cx));
        }
    }

    /// Back to the repo list, on the repo whose branches were listed.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Mode::Branches(ix) | Mode::Prs(ix) = self.mode {
            self.show(Mode::Repos, ix, window, cx);
        }
    }

    /// Enter on row `row`: ticks a repo, or picks a branch for the listed repo and goes back.
    fn choose(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Mode::Branches(ix) | Mode::Prs(ix) = self.mode {
            self.from[ix] = None;
        }
        let ix = match self.mode {
            Mode::Repos => {
                self.toggle(row, cx);
                return self.picker.update(cx, |p, cx| p.set_query("", window, cx));
            }
            Mode::Prs(ix) => {
                let Some(pr) = self.prs[ix]
                    .as_ref()
                    .and_then(|list| list.get(row))
                    .cloned()
                else {
                    return;
                };
                self.own[ix] = Some(format!("pr/{}", pr.number));
                self.chosen_pr[ix] = Some(pr);
                self.picked[ix] = true;
                return self.back(window, cx);
            }
            Mode::Branches(ix) => ix,
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
        self.chosen_pr[ix] = None;
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
        if let Some(pr) = &self.chosen_pr[ix] {
            return format!("#{} · {}", pr.number, pr.head_ref_name);
        }
        if let Some(b) = &self.own[ix] {
            return b.clone();
        }
        if let Some(p) = &self.from[ix] {
            return format!("new · on {p}");
        }
        let typed = self.branch.read(cx).value();
        let typed = typed.trim();
        let exists = (self.branches[ix].as_ref())
            .is_some_and(|l| !typed.is_empty() && l.iter().any(|b| b.name == typed));
        if exists { "existing" } else { "new" }.into()
    }

    /// Returns whether the form was valid (and the dialog may close).
    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let primary = self.primary(cx);
        let typed = self.branch.read(cx).value().trim().to_string();
        let picked: Vec<(Repo, Option<String>)> = (self.repos.iter())
            .zip(&self.picked)
            .zip(&self.own)
            .zip(&self.chosen_pr)
            .filter(|(((_, p), _), _)| **p)
            .map(|(((r, _), o), pr)| {
                let own = if pr.is_some() && !typed.is_empty() {
                    Some(typed.clone())
                } else {
                    o.clone()
                };
                (r.clone(), own)
            })
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
        let prs = self
            .repos
            .iter()
            .zip(&self.picked)
            .zip(&self.chosen_pr)
            .filter_map(|((repo, picked), pr)| {
                let pr = pr.as_ref().filter(|_| *picked)?;
                Some((
                    repo.name.clone(),
                    git::PullRequest {
                        number: pr.number,
                        url: pr.url.clone(),
                        base: pr.base_ref_name.clone(),
                    },
                ))
            })
            .collect();
        let parents = (self.repos.iter().zip(&self.picked).zip(&self.from))
            .filter_map(|((r, p), f)| Some((r.name.clone(), f.clone().filter(|_| *p)?)))
            .collect();
        let prompt = self.first.clone();
        let insert = self.insert.clone();
        self.shark
            .update(cx, |s, cx| {
                s.create_workspace_on(branch, repos, prs, parents, insert, prompt, window, cx)
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
                            let pr_entity = entity.clone();
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
                                .child(
                                    Button::new(("repo-prs", i))
                                        .ghost()
                                        .xsmall()
                                        .label("Open PRs")
                                        .tooltip("Open pull requests (cmd-p)")
                                        .on_click(move |_, window, cx| {
                                            cx.stop_propagation();
                                            pr_entity
                                                .update(cx, |f, cx| f.pick_pr(i, window, cx))
                                                .ok();
                                        }),
                                )
                        })
                })
                .collect(),
            Mode::Branches(ix) => {
                let list = self.branches[ix].clone().unwrap_or_default();
                let new = CommandItem::new().label("New branch (workspace name)");
                (std::iter::once(new))
                    .chain(list.into_iter().enumerate().map(|(i, b)| {
                        let tag = match &b.checked_out {
                            Some(at) => format!("checked out at {}", tilde(Path::new(at))),
                            None => (b.local.then_some("local").into_iter())
                                .chain(b.origin.then_some("origin"))
                                .collect::<Vec<_>>()
                                .join(" · "),
                        };
                        let name = b.name.clone();
                        CommandItem::new().label(b.name).child(move |_, _| {
                            let tip = format!("{name}\n{tag}");
                            v_flex()
                                .id(("source-branch", i))
                                .flex_1()
                                .min_w_0()
                                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                                .child(div().w_full().truncate().child(name.clone()))
                                .child(
                                    div()
                                        .w_full()
                                        .truncate()
                                        .text_xs()
                                        .text_color(muted)
                                        .child(tag.clone()),
                                )
                        })
                    }))
                    .collect()
            }
            Mode::Prs(ix) => self.prs[ix]
                .clone()
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(i, pr)| {
                    let status = pr.status();
                    let label = format!("#{} {}", pr.number, pr.title);
                    let author = pr
                        .author
                        .as_ref()
                        .map(|a| a.login.as_str())
                        .unwrap_or("unknown author")
                        .to_string();
                    let detail = format!(
                        "{} · {} · {} → {}",
                        status.label(),
                        author,
                        pr.head_ref_name,
                        pr.base_ref_name
                    );
                    CommandItem::new()
                        .label(label.clone())
                        .keywords([
                            pr.head_ref_name,
                            pr.base_ref_name,
                            author,
                            pr.number.to_string(),
                        ])
                        .child(move |_, cx| {
                            let tip = format!("{label}\n{detail}");
                            h_flex()
                                .gap_2()
                                .items_center()
                                .flex_1()
                                .min_w_0()
                                .child(status.icon(cx.theme()))
                                .child(
                                    v_flex()
                                        .id(("source-pr", i))
                                        .flex_1()
                                        .min_w_0()
                                        .aria_label(tip.clone())
                                        .test_support()
                                        .tooltip(move |w, cx| {
                                            Tooltip::new(tip.clone()).build(w, cx)
                                        })
                                        .child(div().w_full().truncate().child(label.clone()))
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(detail.clone()),
                                        ),
                                )
                        })
                })
                .collect(),
        };
        let count = self.picked.iter().filter(|p| **p).count();
        let hint = |keys: Vec<Kbd>, label: &'static str| {
            h_flex()
                .gap_1()
                .children(keys)
                .child(div().pl_0p5().child(label))
        };
        let (heading, placeholder) = match self.mode {
            Mode::Repos => ("Repos".to_string(), "Filter repos…".to_string()),
            Mode::Branches(ix) => (
                format!("Branch for {}", self.repos[ix].name),
                "Filter branches…".to_string(),
            ),
            Mode::Prs(ix) => (
                format!("Pull requests for {}", self.repos[ix].name),
                "Filter pull requests…".into(),
            ),
        };
        let picker = Command::new(&self.picker)
            .items(items)
            .placeholder(placeholder)
            .bordered(true)
            .max_h(px(240.))
            .on_confirm(move |ix, window, cx| {
                entity.update(cx, |f, cx| f.choose(ix.row, window, cx)).ok();
            });
        // Install the new rows before resolving their filtered selection.
        let picker = RenderOnce::render(picker, window, cx).into_any_element();
        if let Some(at) = self.pending_selection.take() {
            self.picker
                .update(cx, |p, cx| p.set_selected_index(at, window, cx));
        }
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
                if let Some(ix) = this.source_repo(cx) {
                    this.pick_branch(ix, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PickPullRequest, window, cx| {
                if let Some(ix) = this.source_repo(cx) {
                    this.pick_pr(ix, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &BranchFromSource, window, cx| this.branch_from(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &RefreshWorkspaceSources, window, cx| {
                    this.refresh(true, window, cx);
                }),
            )
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
            .children(self.insert.as_ref().map(|child| {
                div().text_xs().text_color(muted).child(format!(
                    "Inserted below `{child}`: {child} will be stacked on it"
                ))
            }))
            .child(
                h_flex()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().child(heading))
                    .when(self.mode == Mode::Repos, |d| {
                        d.child(format!("{count} of {}", self.repos.len()))
                    })
                    .when(self.mode != Mode::Repos, |d| {
                        d.gap_1()
                            .child(
                                Button::new("source-branches")
                                    .ghost()
                                    .xsmall()
                                    .label("Branches")
                                    .selected(matches!(self.mode, Mode::Branches(_)))
                                    .tooltip("Branches (cmd-b)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(ix) = this.source_repo(cx) {
                                            this.pick_branch(ix, window, cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("source-prs")
                                    .ghost()
                                    .xsmall()
                                    .label("Pull requests")
                                    .selected(matches!(self.mode, Mode::Prs(_)))
                                    .tooltip("Pull requests (cmd-p)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        if let Some(ix) = this.source_repo(cx) {
                                            this.pick_pr(ix, window, cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("refresh-sources")
                                    .ghost()
                                    .xsmall()
                                    .label("Refresh")
                                    .tooltip("Refresh (cmd-r)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.refresh(true, window, cx)
                                    })),
                            )
                    }),
            )
            .child(picker)
            .when_some(self.mode.slot(), |d, (ix, kind)| {
                let status = if self.loading[ix][kind] {
                    Some("Refreshing…".to_string())
                } else if let Some(error) = &self.errors[ix][kind] {
                    Some(format!("Refresh failed: {error}. Showing cached results."))
                } else if kind == 1 && self.prs[ix].as_ref().is_some_and(|p| p.is_empty()) {
                    Some("No open pull requests".to_string())
                } else {
                    None
                };
                d.children(status.map(|text| {
                    div()
                        .id("source-status")
                        .text_xs()
                        .text_color(muted)
                        .aria_label(text.clone())
                        .child(text)
                        .test_support()
                }))
            })
            .child(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(faint)
                    .map(|d| match (self.mode, in_branch) {
                        (Mode::Branches(_), _) => d
                            .child(hint(vec![kbd("enter")], "check out"))
                            .child(hint(vec![kbd("alt-enter")], "branch from it"))
                            .child(hint(vec![kbd("tab")], "back")),
                        (Mode::Prs(_), _) => d
                            .child(hint(vec![kbd("enter")], "review"))
                            .child(hint(vec![kbd("alt-enter")], "branch from it"))
                            .child(hint(vec![kbd("tab")], "back")),
                        (Mode::Repos, true) => d
                            .child(hint(vec![kbd("tab")], "switch field"))
                            .child(hint(vec![kbd("enter")], "create")),
                        (Mode::Repos, false) => d
                            .child(hint(vec![kbd("tab")], "switch field"))
                            .child(hint(vec![kbd("space")], "tick repo"))
                            .child(hint(vec![kbd("cmd-b")], "branch"))
                            .child(hint(vec![kbd("cmd-p")], "PR")),
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
