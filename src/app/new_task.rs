//! The branch and PR dialog: a branch name and a filterable repo or branch picker.
use super::*;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::dialog::Cancel;
use gpui_kit::component::input::Enter;
use std::time::{Duration, Instant};

/// Command's row padding brings this to 36px.
const ROW_H: f32 = 25.;

/// Attach repository's agent, and what its dialog lists about it.
pub(super) struct AttachTo {
    pub(super) name: String,
    pub(super) branch: String,
    /// The agent has a branch: new repositories follow it, and `branch` is that.
    pub(super) follows: bool,
    /// Its repositories, listed but not offered.
    pub(super) attached: Vec<String>,
    /// How long ago each repository was last used, by name.
    pub(super) used: HashMap<String, String>,
}

/// Contents of the New task dialog: a branch name and a filterable repo picker.
/// Starts with the last-used repos ticked and floated to the top, so the common case
/// is type a branch, enter. cmd-b swaps the list for a repo's branches, to check one out
/// there instead of the new branch, or (alt-enter) to branch from it: a stack.
pub(super) struct NewTaskForm {
    pub(super) branch: Entity<InputState>,
    /// Home's prompt, sent once the task is created.
    first: Option<String>,
    picker: Entity<CommandState>,
    repos: Vec<Repo>,
    picked: Vec<bool>,
    /// Per repo: the existing branch picked for it, instead of the new one.
    own: Vec<Option<String>>,
    /// Per repo: a new branch of its own, where the agent's branch collides there.
    renamed: Vec<Option<String>>,
    /// Per repo: its branches, `None` until listed.
    branches: Vec<Option<Vec<git::Branch>>>,
    prs: Vec<Option<Vec<complete::Pr>>>,
    chosen_pr: Vec<Option<complete::Pr>>,
    /// Per repo: the branch the new branch is based on (stacked on), instead of the trunk.
    from: Vec<Option<String>>,
    /// "Insert task below…": the task to stack on the new one once it exists.
    insert: Option<String>,
    /// Attach repository: the agent the repos join, instead of a new one. Enter attaches the
    /// marked repos, or the highlighted one with none marked; cmd-b swaps the list for its branch and base, and the
    /// last row adds a local repo.
    attach: Option<AttachTo>,
    /// Per repo: the base a new branch starts on unless `from` says otherwise; `None` until read.
    defaults: Vec<Option<String>>,
    refreshed_at: Vec<[Option<Instant>; 2]>,
    loading: Vec<[bool; 2]>,
    errors: Vec<[Option<String>; 2]>,
    mode: Mode,
    pending_selection: Option<Option<IndexPath>>,
    shark: WeakEntity<Shark>,
    _sub: Subscription,
}

/// What the picker lists: the repos, or the branches of repo `n`, to check out or as its base.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Repos,
    Branches(usize),
    Bases(usize),
    Prs(usize),
}

impl Mode {
    /// The repo index and the `[branches, prs]` slot the mode lists; `None` for the repos.
    fn slot(self) -> Option<(usize, usize)> {
        match self {
            Mode::Repos => None,
            Mode::Branches(ix) | Mode::Bases(ix) => Some((ix, 0)),
            Mode::Prs(ix) => Some((ix, 1)),
        }
    }
}

impl NewTaskForm {
    /// `from` bases repos' new branch on another branch, `(repo, parent)`; `insert` names the
    /// task that moves on top of the new one.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        mut repos: Vec<Repo>,
        last: &[String],
        first: Option<String>,
        from: &[(String, String)],
        insert: Option<String>,
        attach: Option<AttachTo>,
        shark: WeakEntity<Shark>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fresh = attach.as_ref().map(|a| a.branch.clone());
        let branch = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("branch, e.g. you/eng-123-login")
                .default_value(fresh.unwrap_or_default())
        });
        // Branch names can't contain spaces; turn them into dashes as you type.
        let _sub = cx.subscribe_in(&branch, window, |f, input, ev: &InputEvent, window, cx| {
            if let InputEvent::Change = ev {
                let value = input.read(cx).value();
                if value.contains(' ') {
                    let fixed = value.replace(' ', "-");
                    input.update(cx, |i, cx| i.set_value(fixed, window, cx));
                }
                if let (true, Mode::Branches(ix)) = (f.follows(), f.mode) {
                    f.renamed[ix] = Some(input.read(cx).value().trim().to_string());
                }
                cx.notify(); // the worktree path line and each repo's new/existing
            }
        });
        // Attach repository marks only what it's told to: enter takes the highlighted repo.
        let only = repos.len() == 1 && attach.is_none();
        let was_picked = |r: &Repo| {
            let attached = attach.iter().any(|a| a.attached.contains(&r.name));
            only || (last.contains(&r.name) && !attached)
        };
        repos.sort_by_key(|r| !was_picked(r)); // stable: picked first, otherwise in added order
        let picked = repos.iter().map(was_picked).collect();
        let n = repos.len();
        let from = (repos.iter())
            .map(|r| {
                from.iter()
                    .find(|(n, _)| *n == r.name)
                    .map(|(_, p)| p.clone())
            })
            .collect();
        let form = NewTaskForm {
            from,
            insert,
            attach,
            defaults: vec![None; n],
            branch,
            first,
            picker: cx.new(|cx| CommandState::new(window, cx)),
            repos,
            picked,
            own: vec![None; n],
            renamed: vec![None; n],
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
        };
        form.list(0, cx);
        form
    }

    /// Local refs only, so it's quick: whether the typed name exists, and the lists, of the repos
    /// from `first` on.
    fn list(&self, first: usize, cx: &mut Context<Self>) {
        let srcs: Vec<PathBuf> = self.repos[first..].iter().map(|r| r.path.clone()).collect();
        cx.spawn(async move |this, cx| {
            let lists = (cx.background_executor())
                .spawn(async move {
                    crate::proc::par_map(&srcs, |s| {
                        (git::branches(s).ok(), git::resolve_base(s, ""))
                    })
                })
                .await;
            this.update(cx, |f, cx| {
                for (i, (l, base)) in (first..).zip(lists) {
                    // A fetched list may have landed first.
                    if f.branches[i].is_none() {
                        f.branches[i] = l;
                    }
                    f.defaults[i] = Some(base);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            // Not the add-a-local-repo row, which is no repo.
            None => (self.picker.read(cx).selected_index())
                .map(|i| i.row)
                .filter(|r| *r < self.repos.len()),
            Some((ix, _)) => Some(ix),
        }
    }

    fn pick_branch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.show(Mode::Branches(ix), 0, window, cx);
        self.refresh(false, window, cx);
        if self.attach.is_some() && self.names_own(ix, cx) {
            let name = self.new_branch(ix, cx);
            self.branch.update(cx, |i, cx| {
                i.set_value(name, window, cx);
                i.focus(window, cx)
            });
        }
    }

    /// The base list, on the base repo `ix` has now.
    fn pick_base(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let at = (self.branches[ix].iter().flatten()).position(|b| match &self.from[ix] {
            Some(base) => base == &b.name,
            None => self.is_default(ix, &b.name),
        });
        self.show(Mode::Bases(ix), 0, window, cx);
        self.select_after_refresh(at.map(IndexPath::new), window, cx);
        self.refresh(false, window, cx);
    }

    /// `branch` is repo `ix`'s default base.
    fn is_default(&self, ix: usize, branch: &str) -> bool {
        (self.defaults[ix].as_deref())
            .is_some_and(|d| d.strip_prefix("origin/").unwrap_or(d) == branch)
    }

    /// The branch list's entry on picker row `row`: the check-out list starts with New branch.
    fn listed(&self, ix: usize, row: usize) -> Option<&git::Branch> {
        let i = match self.mode {
            Mode::Bases(_) => row,
            _ => row.checked_sub(1)?,
        };
        self.branches[ix].as_ref()?.get(i)
    }

    fn pick_pr(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.attach.is_some() {
            return;
        }
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
        let was = row.and_then(|r| self.listed(ix, r)).map(|b| b.name.clone());
        if let (Mode::Branches(shown) | Mode::Bases(shown), Some(was)) = (self.mode, was)
            && shown == ix
        {
            // Resolved against the list shown now; a row it filters out highlights nothing.
            let at = list.iter().position(|b| b.name == was);
            let first = matches!(self.mode, Mode::Branches(_)) as usize;
            let at = at.map(|i| IndexPath::new(i + first));
            self.select_after_refresh(at, window, cx);
        }
        self.branches[ix] = Some(list);
        cx.notify();
    }

    /// alt-enter on a branch or PR: the repo's new branch starts on it and its PR will target
    /// it. The source must be on origin and in this repo (not a fork's PR).
    fn branch_from(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Attach repository has the Base button for it.
        if self.attach.is_some() {
            return;
        }
        if let Some(row) = self.picker.read(cx).selected_index().map(|i| i.row) {
            self.branch_from_row(row, window, cx);
        }
    }

    fn branch_from_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (ix, parent) = match self.mode {
            Mode::Repos => return,
            Mode::Branches(ix) | Mode::Bases(ix) => {
                let Some(b) = self.listed(ix, row) else {
                    return;
                };
                // A base needn't be on GitHub, a parent to stack on must.
                if !b.origin && self.attach.is_none() {
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
        if let Mode::Branches(ix) | Mode::Bases(ix) | Mode::Prs(ix) = self.mode {
            self.show(Mode::Repos, ix, window, cx);
            // Enter attaches the highlighted row, so it must land on the repo, whatever the list
            // shown just now held.
            if self.attach.is_some() {
                self.select_after_refresh(Some(IndexPath::new(ix)), window, cx);
            }
        }
    }

    /// Enter on row `row`: ticks a repo (attaches it, in Attach repository), or picks a branch or
    /// base for the listed repo and goes back.
    fn choose(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Mode::Bases(_) = self.mode {
            return self.branch_from_row(row, window, cx);
        }
        // A new branch keeps the base picked for it in Attach repository.
        if let Mode::Branches(ix) | Mode::Prs(ix) = self.mode
            && (self.attach.is_none() || row > 0)
        {
            self.from[ix] = None;
        }
        let ix = match self.mode {
            Mode::Repos if row == self.repos.len() => return self.add_local(window, cx),
            Mode::Repos if self.attach.is_some() => {
                if self.submit_attach(Some(row), window, cx) {
                    window.close_dialog(cx);
                }
                return;
            }
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
            Mode::Branches(ix) | Mode::Bases(ix) => ix,
        };
        // Row 0 is the new branch.
        let b = self.listed(ix, row).cloned();
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

    /// The task's name and branch: the typed one, else the first picked existing branch
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
        if self.typed_exists(ix, cx) {
            "existing"
        } else {
            "new"
        }
        .into()
    }

    /// Repo `ix` has a branch of the name it would get new, here or on origin.
    fn typed_exists(&self, ix: usize, cx: &App) -> bool {
        let typed = self.new_branch(ix, cx);
        (self.branches[ix].as_ref())
            .is_some_and(|l| !typed.is_empty() && l.iter().any(|b| b.name == typed))
    }

    /// The new branch repo `ix` would get: the typed name, or for a repo joining an agent with a
    /// branch, that branch unless the repo has its own.
    fn new_branch(&self, ix: usize, cx: &App) -> String {
        match &self.attach {
            Some(a) if a.follows => self.renamed[ix].clone().unwrap_or(a.branch.clone()),
            _ => self.branch.read(cx).value().trim().to_string(),
        }
    }

    /// New repositories follow the agent's branch.
    fn follows(&self) -> bool {
        self.attach.as_ref().is_some_and(|a| a.follows)
    }

    /// Repo `ix`'s new branch is `new_branch`'s to name here: an agent's branch is read-only until
    /// it collides.
    fn names_own(&self, ix: usize, cx: &App) -> bool {
        !self.follows() || self.renamed[ix].is_some() || self.typed_exists(ix, cx)
    }

    fn attached(&self, ix: usize) -> bool {
        (self.attach.iter()).any(|a| a.attached.contains(&self.repos[ix].name))
    }

    fn all_attached(&self) -> bool {
        (0..self.repos.len()).all(|i| self.attached(i))
    }

    /// What Attach takes: exactly the marked repos, or with none marked `row`, as the issue
    /// picker does.
    fn targets(&self, row: Option<usize>) -> Vec<usize> {
        let marked = self.picked.iter().any(|p| *p);
        (0..self.repos.len())
            .filter(|i| self.picked[*i] || (!marked && row == Some(*i)))
            .collect()
    }

    /// The repo the summary row describes and cmd-b changes: the highlighted one, or with the
    /// highlight on no repo, the first marked or offered.
    fn change_repo(&self, cx: &App) -> Option<usize> {
        (self.source_repo(cx))
            .or_else(|| self.picked.iter().position(|p| *p))
            .or_else(|| (0..self.repos.len()).find(|i| !self.attached(*i)))
    }

    /// The first repo Attach would refuse for its name.
    fn collision(&self, cx: &App) -> Option<usize> {
        (self.targets(self.source_repo(cx)).into_iter())
            .find(|i| self.own[*i].is_none() && self.typed_exists(*i, cx))
    }

    /// A name no repo has yet, in place of the one taken in repo `ix`. Fresh attachments refuse
    /// an existing branch, so this offers another rather than reusing it.
    fn suggestion(&self, ix: usize, cx: &App) -> String {
        let name = self.new_branch(ix, cx);
        let taken = |c: &String| (self.branches.iter().flatten().flatten()).any(|b| b.name == *c);
        (2..)
            .map(|n| format!("{name}-{n}"))
            .find(|c| !taken(c))
            .unwrap_or(name)
    }

    /// Takes the suggestion for repo `ix`: the agent's branch stays as it is for the others, and
    /// a branch still to be made changes for all of them.
    fn use_suggestion(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.suggestion(ix, cx);
        match self.follows() {
            true => self.renamed[ix] = Some(name),
            false => self
                .branch
                .update(cx, |i, cx| i.set_value(name, window, cx)),
        }
        cx.notify();
    }

    /// What repo `ix` will be on, as the summary row says it.
    fn plan(&self, ix: usize, cx: &App) -> String {
        if let Some(b) = &self.own[ix] {
            return format!("Existing branch {b}");
        }
        let base = self.from[ix].as_ref().or(self.defaults[ix].as_ref());
        let base = base.map_or("default branch", |b| b.strip_prefix("origin/").unwrap_or(b));
        match self.new_branch(ix, cx).as_str() {
            "" => "Name the branch".into(),
            name => format!("{name} from {base}"),
        }
    }

    /// The list takes the keys, once there is one; until then the dialog does.
    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.all_attached() {
            self.picker.update(cx, |p, cx| p.focus(window, cx));
        }
    }

    /// Attach repository's last row: the folder panel, over this dialog. Cancelling it leaves
    /// everything here as it was.
    fn add_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.attach.as_ref().map(|a| a.name.clone()) else {
            return;
        };
        let form = cx.entity().downgrade();
        self.shark
            .update(cx, |s, cx| {
                s.attach_local_repo(name, Some(form), window, cx)
            })
            .ok();
    }

    /// Repositories the folder panel added arrive marked.
    // ponytail: appended, so they list below the attached rows until the dialog reopens; sorting
    // them in would shift the indices a running `list` or `refresh` still holds.
    pub(super) fn added(&mut self, repos: Vec<Repo>, window: &mut Window, cx: &mut Context<Self>) {
        let first = self.repos.len();
        for r in repos {
            self.repos.push(r);
            self.picked.push(true);
            self.own.push(None);
            self.renamed.push(None);
            self.branches.push(None);
            self.prs.push(None);
            self.chosen_pr.push(None);
            self.from.push(None);
            self.defaults.push(None);
            self.refreshed_at.push([None; 2]);
            self.loading.push([false; 2]);
            self.errors.push([None, None]);
        }
        self.list(first, cx);
        self.picker.update(cx, |p, cx| p.focus(window, cx));
        cx.notify();
    }

    /// Attach repository's Attach: queues the marked repos and `row`. A branch name collision is
    /// told here; the operation checks again, as branches can appear while it waits.
    fn submit_attach(
        &mut self,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.all_attached() {
            self.add_local(window, cx);
            return false;
        }
        let Some(name) = self.attach.as_ref().map(|a| a.name.clone()) else {
            return false;
        };
        let picks = self.targets(row);
        let fresh = || picks.iter().copied().filter(|i| self.own[*i].is_none());
        let unnamed = fresh().find(|i| self.new_branch(*i, cx).is_empty());
        let taken = fresh().find(|i| self.typed_exists(*i, cx));
        if let Some(i) = unnamed {
            notify_err(window, cx, "Name the branch first");
            self.pick_branch(i, window, cx);
        } else if let Some(i) = taken {
            let (repo, typed) = (&self.repos[i].name, self.new_branch(i, cx));
            let msg = format!("{repo}: branch `{typed}` already exists; pick another name");
            notify_err(window, cx, msg);
        } else if !picks.is_empty() {
            let picks = (picks.iter())
                .map(|i| crate::state::Attachment {
                    fresh: self.own[*i].is_none(),
                    ..crate::state::Attachment::fresh(
                        &self.repos[*i],
                        &(self.own[*i].clone()).unwrap_or_else(|| self.new_branch(*i, cx)),
                        self.from[*i].as_deref().unwrap_or_default(),
                    )
                })
                .collect();
            self.shark
                .update(cx, |s, cx| s.queue_attachment(name, picks, window, cx))
                .ok();
            return true;
        }
        false
    }

    /// Returns whether the form was valid (and the dialog may close).
    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.attach.is_some() {
            return self.submit_attach(self.source_repo(cx), window, cx);
        }
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
                s.create_task_on(branch, repos, prs, parents, insert, prompt, window, cx)
            })
            .ok();
        true
    }
}

impl Render for NewTaskForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, faint) = (theme.muted_foreground, faint(theme));
        let in_branch = self.branch.read(cx).focus_handle(cx).is_focused(window);
        let attaching = self.attach.is_some();
        let entity = cx.entity().downgrade();
        let items: Vec<CommandItem> = match self.mode {
            Mode::Repos if attaching => self.repo_items(cx),
            Mode::Branches(ix) | Mode::Bases(ix) if attaching => self.branch_items(ix, cx),
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
            Mode::Branches(ix) | Mode::Bases(ix) => {
                let list = self.branches[ix].clone().unwrap_or_default();
                let new = CommandItem::new().label("New branch (task name)");
                (matches!(self.mode, Mode::Branches(_))
                    .then_some(new)
                    .into_iter())
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
            Mode::Repos if attaching => (String::new(), "Search repositories…".to_string()),
            Mode::Repos => ("Repos".to_string(), "Filter repos…".to_string()),
            Mode::Branches(ix) => (
                format!("Branch for {}", self.repos[ix].name),
                "Filter branches…".to_string(),
            ),
            Mode::Bases(ix) => (
                format!("Base for {}", self.repos[ix].name),
                "Filter branches…".to_string(),
            ),
            Mode::Prs(ix) => (
                format!("Pull requests for {}", self.repos[ix].name),
                "Filter pull requests…".into(),
            ),
        };
        let picker = crate::ui::command(&self.picker, window)
            .items(items)
            .placeholder(placeholder)
            .bordered(!attaching)
            .max_h(px(if attaching { 380. } else { 240. }))
            .on_confirm(move |ix, window, cx| {
                entity.update(cx, |f, cx| f.choose(ix.row, window, cx)).ok();
            })
            // The summary row and the count follow the highlight.
            .when(attaching, |c| {
                let (moved, typed) = (cx.entity().downgrade(), cx.entity().downgrade());
                c.on_select(move |_, _, cx| {
                    moved.update(cx, |_, cx| cx.notify()).ok();
                })
                .on_query(move |_, _, cx| {
                    typed.update(cx, |_, cx| cx.notify()).ok();
                })
            });
        // Install the new rows before resolving their filtered selection.
        let picker = RenderOnce::render(picker, window, cx).into_any_element();
        if let Some(at) = self.pending_selection.take() {
            self.picker
                .update(cx, |p, cx| p.set_selected_index(at, window, cx));
        }
        let form = v_flex()
            .key_context("NewTask")
            .on_action(cx.listener(|this, _: &SwitchField, window, cx| {
                if this.attach.is_some() {
                    return this.attach_tab(window, cx);
                }
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
                // Its field takes typing.
                if this.attach.is_some() {
                    return cx.propagate();
                }
                if this.mode == Mode::Repos
                    && let Some(ix) = this.picker.read(cx).selected_index()
                {
                    this.toggle(ix.row, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PickBranch, window, cx| {
                if this.attach.is_none() {
                    if let Some(ix) = this.source_repo(cx) {
                        this.pick_branch(ix, window, cx);
                    }
                    return;
                }
                match (this.mode, this.change_repo(cx)) {
                    (Mode::Branches(ix), _) => this.pick_base(ix, window, cx),
                    (_, Some(ix)) => this.pick_branch(ix, window, cx),
                    _ => {}
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
                cx.listener(|this, _: &RefreshTaskSources, window, cx| {
                    this.refresh(true, window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &SubmitForm, window, cx| {
                if this.submit(window, cx) {
                    window.close_dialog(cx);
                }
            }))
            .on_action(cx.listener(|this, e: &Enter, window, cx| this.attach_enter(e, window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                match this.attach.is_some() && this.mode != Mode::Repos {
                    true => this.back(window, cx),
                    false => cx.propagate(),
                }
            }));
        if attaching {
            return form
                .child(self.attach_body(picker, in_branch, cx))
                .into_any_element();
        }
        form.gap_3()
            .child(Input::new(&self.branch))
            .child({
                // As `create_workspace` names it.
                let name = self.primary(cx).unwrap_or_default().replace('/', "-");
                let dir = tasks_dir().join(if name.is_empty() { "<name>" } else { &name });
                div()
                    .text_xs()
                    .text_color(faint)
                    .truncate()
                    .child(format!("Worktrees go in {}", super::menus::tilde(&dir)))
            })
            .children(self.insert.as_ref().map(|child| {
                let shark = self.shark.upgrade();
                let child = shark.map_or(child.clone(), |s| s.read(cx).task_title(child));
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
            .children(self.source_status(cx))
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
                        (Mode::Bases(_), _) => d,
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
            .into_any_element()
    }
}

/// A dialog row, as in the issue picker.
fn row() -> Div {
    h_flex().flex_1().min_w_0().h(px(ROW_H)).gap_2()
}

/// Attach repository's rows, keys and layout. They follow the issue picker: search on top, a flat
/// list, keys below.
impl NewTaskForm {
    fn repo_items(&self, cx: &mut Context<Self>) -> Vec<CommandItem> {
        let entity = cx.entity().downgrade();
        let repos = (0..self.repos.len()).map(|i| {
            let name = self.repos[i].name.clone();
            let attached = self.attached(i);
            let taken = (!attached && self.own[i].is_none() && self.typed_exists(i, cx))
                .then(|| (self.new_branch(i, cx), self.suggestion(i, cx)));
            // Only what the summary row wouldn't say of every repo.
            let own = self.own[i].is_some() || self.from[i].is_some() || self.renamed[i].is_some();
            let note = (own && taken.is_none()).then(|| self.plan(i, cx));
            let used = (self.attach.iter()).find_map(|a| a.used.get(&name).cloned());
            let marked = self.picked[i];
            let label = match (&taken, attached) {
                (Some((branch, _)), _) => format!("{name}, branch {branch} already exists"),
                (None, true) => format!("{name}, attached"),
                (None, false) => name.clone(),
            };
            let entity = entity.clone();
            CommandItem::new()
                .label(name.clone())
                .keywords([self.repos[i].path.to_string_lossy().to_string()])
                .disabled(attached)
                .child(move |_, cx| {
                    let theme = cx.theme();
                    let (muted, warning) = (theme.muted_foreground, theme.warning_foreground);
                    let (rename, mark) = (entity.clone(), entity.clone());
                    let (icon, color) = match taken {
                        Some(_) => (IconName::TriangleAlert, warning),
                        None => (IconName::FolderGit2, muted),
                    };
                    let meta = || div().flex_none().text_xs().text_color(muted);
                    row()
                        .id(("attach-row", i))
                        .test_support()
                        .aria_label(label.clone())
                        .child(Icon::new(icon).with_size(px(14.)).text_color(color))
                        .child(div().min_w_0().truncate().child(name.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(if taken.is_some() { warning } else { muted })
                                .when_some(taken.clone(), |d, (branch, _)| {
                                    d.child(format!("Branch {branch} already exists"))
                                })
                                .children(note.clone()),
                        )
                        .when_some(taken.clone(), |d, (_, name)| {
                            d.child(
                                Button::new(("attach-rename", i))
                                    .ghost()
                                    .xsmall()
                                    .flex_none()
                                    .label(format!("Use {name}"))
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        rename
                                            .update(cx, |f, cx| f.use_suggestion(i, window, cx))
                                            .ok();
                                    }),
                            )
                        })
                        .children(used.clone().map(|used| meta().child(used)))
                        .map(|d| match attached {
                            true => d.child(meta().child("Attached")),
                            false => d.child(
                                Checkbox::new(("repo-pick", i))
                                    .small()
                                    .checked(marked)
                                    .tooltip("Mark (cmd-enter)")
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        mark.update(cx, |f, cx| f.toggle(i, cx)).ok();
                                    }),
                            ),
                        })
                })
        });
        let add = CommandItem::new()
            .label("Add a local repository…")
            .child(|_, cx| {
                let muted = cx.theme().muted_foreground;
                row()
                    .id("attach-add-row")
                    .test_support()
                    .child(
                        Icon::new(IconName::FolderPlus)
                            .with_size(px(14.))
                            .text_color(muted),
                    )
                    .child("Add a local repository…")
            });
        repos.chain([add]).collect()
    }

    /// One line a branch: a base doesn't care where it's checked out, a checkout does.
    fn branch_items(&self, ix: usize, cx: &App) -> Vec<CommandItem> {
        let bases = matches!(self.mode, Mode::Bases(_));
        let new = CommandItem::new()
            .label("New branch")
            .child(|_, _| row().id("attach-new-branch").child("New branch"));
        let list = self.branches[ix].clone().unwrap_or_default();
        let muted = cx.theme().muted_foreground;
        (!bases)
            .then_some(new)
            .into_iter()
            .chain(list.into_iter().enumerate().map(|(i, b)| {
                let default = self.is_default(ix, &b.name);
                let used = (b.checked_out.as_ref())
                    .filter(|_| !bases)
                    .map(|at| format!("Checked out at {}", tilde(Path::new(at))));
                let name = b.name.clone();
                CommandItem::new().label(b.name).child(move |_, _| {
                    row()
                        .id(("source-branch", i))
                        .child(div().min_w_0().truncate().child(name.clone()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_right()
                                .text_xs()
                                .text_color(muted)
                                .children(used.clone()),
                        )
                        .when(default, |d| {
                            d.child(
                                div()
                                    .id("base-default")
                                    .flex_none()
                                    .text_xs()
                                    .text_color(muted)
                                    .child("Default")
                                    .test_support(),
                            )
                        })
                })
            }))
            .collect()
    }

    /// Tab: on the repositories, takes the suggested name; in Change, name field <-> branches.
    fn attach_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Repos => {
                if let Some(ix) = self.collision(cx) {
                    self.use_suggestion(ix, window, cx);
                }
            }
            Mode::Branches(ix) if self.names_own(ix, cx) => {
                if self.branch.read(cx).focus_handle(cx).is_focused(window) {
                    self.picker.update(cx, |p, cx| p.focus(window, cx))
                } else {
                    self.branch.update(cx, |i, cx| i.focus(window, cx))
                }
                cx.notify();
            }
            _ => {}
        }
    }

    /// Enter that a field passed on: cmd-enter marks the highlighted repository, and in the name
    /// field enter is done with it. The rest stays with the list and the dialog.
    fn attach_enter(&mut self, e: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        let in_branch = self.branch.read(cx).focus_handle(cx).is_focused(window);
        match (self.mode, e.secondary) {
            (Mode::Repos, true) if self.attach.is_some() => {
                if let Some(ix) = self.source_repo(cx) {
                    self.toggle(ix, cx);
                }
            }
            (Mode::Branches(_), false) if self.attach.is_some() && in_branch => {
                self.back(window, cx)
            }
            _ => cx.propagate(),
        }
    }

    /// What's happening to the listed branches or pull requests, if it's worth saying.
    fn source_status(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let (ix, kind) = self.mode.slot()?;
        let text = if self.loading[ix][kind] {
            "Refreshing…".to_string()
        } else if let Some(error) = &self.errors[ix][kind] {
            format!("Refresh failed: {error}. Showing cached results.")
        } else if kind == 1 && self.prs[ix].as_ref().is_some_and(|p| p.is_empty()) {
            "No open pull requests".to_string()
        } else {
            return None;
        };
        Some(
            div()
                .id("source-status")
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .aria_label(text.clone())
                .child(text)
                .test_support(),
        )
    }

    /// The Attach button, counting what it attaches, and the keys.
    fn attach_footer(&self, in_branch: bool, cx: &mut Context<Self>) -> Div {
        let count = self.targets(self.source_repo(cx)).len();
        let attach = (count > 0).then(|| {
            let label = match count {
                1 => "Attach 1 repository".to_string(),
                n => format!("Attach {n} repositories"),
            };
            Button::new("create")
                .primary()
                .xsmall()
                .label(label)
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.submit(window, cx) {
                        window.close_dialog(cx);
                    }
                }))
                .into_any_element()
        });
        let hints: &[_] = match (self.mode, in_branch) {
            (Mode::Repos, _) if self.collision(cx).is_some() => &[
                ("enter", "Attach"),
                ("cmd-enter", "Mark"),
                ("tab", "Use suggested name"),
                ("cmd-b", "Change"),
                ("escape", "Close"),
            ],
            (Mode::Repos, _) => &[
                ("up down", "Navigate"),
                ("enter", "Attach"),
                ("cmd-enter", "Mark"),
                ("cmd-b", "Change"),
                ("escape", "Close"),
            ],
            (Mode::Branches(_), true) => &[
                ("enter", "Done"),
                ("tab", "Branches"),
                ("cmd-b", "Base"),
                ("escape", "Back"),
            ],
            (Mode::Branches(ix), _) if self.names_own(ix, cx) => &[
                ("enter", "Check out"),
                ("tab", "Name"),
                ("cmd-b", "Base"),
                ("escape", "Back"),
            ],
            (Mode::Branches(_), _) => &[
                ("enter", "Check out"),
                ("cmd-b", "Base"),
                ("escape", "Back"),
            ],
            _ => &[
                ("enter", "Use as base"),
                ("cmd-b", "Branch"),
                ("escape", "Back"),
            ],
        };
        crate::ui::key_footer(hints, attach).text_color(cx.theme().muted_foreground)
    }

    /// The branch and base the highlighted repository gets, and the way to change them.
    fn summary(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let plan = self.plan(ix, cx);
        let follows = self.own[ix].is_none() && !self.names_own(ix, cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        h_flex()
            .id("attach-summary")
            .test_support()
            .aria_label(plan.clone())
            .h(px(36.))
            .px_3()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                Icon::new(IconName::GitBranch)
                    .with_size(px(14.))
                    .text_color(muted),
            )
            .child(div().min_w_0().truncate().child(plan))
            .when(follows, |d| {
                d.child(
                    div()
                        .id("attach-follows")
                        .test_support()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child("Follows the task's branch"),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("attach-change")
                    .ghost()
                    .xsmall()
                    .label("Change")
                    .tooltip("Change branch or base (cmd-b)")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_branch(ix, window, cx);
                    })),
            )
    }

    /// Change, inside the dialog: where the list goes back to, and the other list.
    fn drill_header(&self, heading: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let other = match self.mode {
            Mode::Bases(_) => "Branch",
            _ => "Base",
        };
        h_flex()
            .px_1()
            .py_1()
            .gap_2()
            .text_sm()
            .child(
                Button::new("source-back")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronLeft)
                    .label("Back")
                    .tooltip("Back (esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
            )
            .child(div().flex_1().min_w_0().truncate().child(heading))
            .child(
                Button::new("source-other")
                    .ghost()
                    .xsmall()
                    .label(other)
                    .tooltip(format!("{other} (cmd-b)"))
                    .on_click(cx.listener(|this, _, window, cx| match this.mode {
                        Mode::Branches(ix) => this.pick_base(ix, window, cx),
                        Mode::Bases(ix) => this.pick_branch(ix, window, cx),
                        _ => {}
                    })),
            )
            .child(
                Button::new("refresh-sources")
                    .ghost()
                    .xsmall()
                    .label("Refresh")
                    .tooltip("Refresh (cmd-r)")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(true, window, cx))),
            )
    }

    /// The new branch's name, or that it isn't one to change here.
    fn name_field(&self, ix: usize, cx: &App) -> Div {
        let field = div().px_3().py_1();
        if self.names_own(ix, cx) {
            return field.child(Input::new(&self.branch));
        }
        field.text_sm().child(self.new_branch(ix, cx)).child(
            div()
                .id("attach-follows")
                .test_support()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Follows the task's branch"),
        )
    }

    fn attach_body(
        &self,
        picker: AnyElement,
        in_branch: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if self.all_attached() {
            let add = Button::new("attach-add")
                .primary()
                .xsmall()
                .label("Add a local repository")
                .on_click(cx.listener(|this, _, window, cx| this.add_local(window, cx)));
            return v_flex()
                .id("attach-none")
                .test_support()
                .gap_1()
                .child(
                    v_flex()
                        .px_3()
                        .pt_2()
                        .gap_1()
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::MEDIUM)
                                .child("Every repository is attached"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("Add a local repository to attach another."),
                        ),
                )
                .child(
                    crate::ui::key_footer(
                        &[("enter", "Add"), ("escape", "Close")],
                        Some(add.into_any_element()),
                    )
                    .text_color(muted),
                )
                .into_any_element();
        }
        let footer = self.attach_footer(in_branch, cx);
        let body = v_flex();
        let (ix, heading) = match self.mode {
            Mode::Branches(ix) => (ix, format!("Branch for {}", self.repos[ix].name)),
            Mode::Bases(ix) => (ix, format!("Base for {}", self.repos[ix].name)),
            _ => {
                let summary = self.change_repo(cx).map(|ix| self.summary(ix, cx));
                return body
                    .child(picker)
                    .children(summary)
                    .child(footer)
                    .into_any_element();
            }
        };
        let name = matches!(self.mode, Mode::Branches(_)).then(|| self.name_field(ix, cx));
        body.child(self.drill_header(heading, cx))
            .children(name)
            .child(picker)
            .children(self.source_status(cx).map(|s| div().px_3().child(s)))
            .child(footer)
            .into_any_element()
    }
}
