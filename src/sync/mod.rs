//! The header's git button: keeps every repo of the open workspace in sync with git and GitHub,
//! and runs the next step toward a merged PR. `status` decides what that step is; this file
//! polls, renders and executes. The app only embeds the entity and forwards three actions.
mod status;
#[cfg(test)]
pub(crate) use status::parse_prs;
pub use status::{Kind, Pr, PrState, Repo, ago, github_url, short};

use crate::app::menus::{each_or_all, pr_entry};
use crate::app::{inter_features, notify_err, open_menu};
use crate::chat::Chat;
use crate::git::{cmd_bounded, par_map};
use crate::menu::{self, Entry};
use crate::pr;
use crate::state::Workspace;
use gpui_kit::assets::IconName;
use gpui_kit::base::async_util::{Receiver, Sender, unbounded};
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::command::CommandItem;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, Sizable as _, WindowExt, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use status::{
    Action, Local, among, diverged, fetch_pr, next_action, pr_head_moved, pr_settling, probe,
};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// `git status` per repo. ponytail: polling, not FSEvents; watch .git if huge repos make this slow.
const LOCAL_EVERY: Duration = Duration::from_secs(3);
const FETCH_EVERY: Duration = Duration::from_secs(180);
/// On window activation, fetch only if the last one is older than this.
const FETCH_ON_FOCUS: Duration = Duration::from_secs(60);
const PR_EVERY: Duration = Duration::from_secs(60);
/// While checks run or GitHub is still computing mergeability.
const PR_SETTLING: Duration = Duration::from_secs(10);
/// After gh failed for every repo (offline, logged out, rate limited).
const PR_BACKOFF: Duration = Duration::from_secs(300);
/// Checks that never report (a required check with no runner) mustn't keep polling fast forever.
const SETTLE_FOR: Duration = Duration::from_secs(600);
/// While a PR shows a head we don't have, someone may have pushed; fetch this often.
const FETCH_MOVED: Duration = Duration::from_secs(30);

pub enum SyncEvent {
    /// A user-requested Git operation completed successfully.
    Interacted,
    Reviewed(Vec<String>),
    MergeOrder(Vec<String>),
    DeleteRemote(bool),
    Archive,
    /// A parent PR merged: Shark moves these `(repo, old base, new base)`, then sends `prompt`.
    Restack {
        moves: Vec<(String, String, String)>,
        prompt: String,
    },
    /// The PR target changed, or a fetch may have moved its base ref.
    DiffBaseChanged,
    /// Create PRs succeeded.
    PrsCreated,
}

pub struct Branch {
    pub repo: String,
    pub name: String,
    pub base: String,
    pub url: Option<String>,
}

pub struct GitSync {
    ws: Workspace,
    /// Every open chat of the workspace; git writes wait for all of them.
    chats: Vec<Entity<Chat>>,
    /// The shown chat; judgment steps prompt it.
    chat: Option<Entity<Chat>>,
    repos: Vec<Repo>,
    /// `None` until the first probe lands.
    pub(crate) action: Option<Action>,
    /// Label of the step being run; one at a time.
    running: Option<String>,
    fetched: Option<Instant>,
    pr_checked: Option<Instant>,
    gh_error: Option<String>,
    /// From the last fetch (offline, ssh refused); cleared when one succeeds everywhere.
    fetch_error: Option<String>,
    /// Review IDs already sent to the agent; persisted in state.json.
    reviewed: Vec<String>,
    pub(crate) delete_remote: bool,
    /// Repo names in the order Merge runs them; persisted in state.json.
    merge_order: Vec<String>,
    /// gh failed for every repo on the last ask.
    gh_down: bool,
    settling_since: Option<Instant>,
    want_fetch: bool,
    want_pr: bool,
    kick: Sender<()>,
    kicked: Receiver<()>,
    /// Polling runs only while the workspace is shown; dropping the task stops it.
    poll: Option<Task<()>>,
    /// Open PRs per repo, for the stack around this workspace's branches; from the disk cache
    /// the New workspace dialog shares, refreshed with each fetch.
    pub stack_prs: std::collections::HashMap<String, Vec<crate::complete::Pr>>,
    /// The git menu, open under the chevron, whose bounds the last paint recorded.
    menu: Option<(Entity<menu::Menu>, Subscription)>,
    chevron: Rc<Cell<Bounds<Pixels>>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SyncEvent> for GitSync {}

/// Everything a background probe needs, detached from the entity.
#[derive(Clone)]
struct Job {
    /// `(worktree, base, branch)` per repo.
    repos: Vec<(PathBuf, String, String)>,
    fetch: bool,
    pr: bool,
    /// The last probe per repo, so unchanged refs skip the costly comparisons.
    prev: Vec<Local>,
    /// With `fetch`: `(repo, source)` whose open PRs to list, for the stack around the branch.
    stack: Vec<(String, PathBuf)>,
}

impl Job {
    /// Every repo of `ws`, asking GitHub, not fetching.
    fn prs(ws: &Workspace) -> Job {
        Job {
            repos: worktrees(ws),
            fetch: false,
            pr: true,
            prev: vec![],
            stack: vec![],
        }
    }
}

struct Probed {
    repos: Vec<(PathBuf, String, String)>,
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
    /// Per repo, a stacked one's parent PR; `None` when not asked.
    parents: Vec<Option<Result<Option<Pr>, String>>>,
    /// Per `job.stack` repo, its open PRs; `None` when gh failed.
    stack_prs: Vec<(String, Option<Vec<crate::complete::Pr>>)>,
    fetched: bool,
    fetch_error: Option<String>,
}

/// Repos run in parallel, so one hung remote costs one timeout, not one per repo.
fn run_job(job: &Job) -> Probed {
    let results = par_map(job.repos.iter().enumerate(), |(i, (wt, base, branch))| {
        // Errors (offline, no origin) show in the tooltip and menu, not a toast every 3 min.
        let fetch_err = job
            .fetch
            .then(|| cmd_bounded(wt, "git", &["fetch", "--prune", "--quiet", "origin"], 60).err())
            .flatten();
        let local = probe(wt, branch, base, job.prev.get(i));
        let head = local.as_ref().map(|l| l.head.as_str()).unwrap_or_default();
        let pr = job.pr.then(|| fetch_pr(wt, branch, head));
        let trunk = local.as_ref().map_or("", |l| l.trunk.as_str());
        let parent = (crate::stack::parent_of(base, trunk).filter(|_| job.pr && !trunk.is_empty()))
            .map(|p| status::fetch_parent(wt, p));
        (local, (pr, parent), fetch_err)
    });
    let mut locals = vec![];
    let mut prs = vec![];
    let mut parents = vec![];
    let mut errs = vec![];
    for ((local, (pr, parent), err), (wt, ..)) in results.into_iter().zip(&job.repos) {
        locals.push(local);
        prs.extend(pr);
        parents.push(parent);
        if let Some(e) = err {
            let name = wt.file_name().unwrap_or_default().to_string_lossy();
            errs.push(format!("{name}: {e}"));
        }
    }
    // ponytail: every repo's open PRs on each fetch (one gh call per repo per 3 min); only
    // stacked repos and ones with an open PR if that shows up in gh's rate limit.
    let stack_prs = match job.fetch {
        true => par_map(&job.stack, |(name, src)| {
            (
                name.clone(),
                crate::complete::fetch_repo_prs(name, src, true).ok(),
            )
        }),
        false => vec![],
    };
    Probed {
        repos: job.repos.clone(),
        locals,
        prs: job.pr.then_some(prs),
        parents,
        stack_prs,
        fetched: job.fetch,
        fetch_error: (!errs.is_empty()).then(|| errs.join("\n")),
    }
}

/// A slow check of a workspace that isn't on screen, so its sidebar row can say "needs you"
/// (red CI, a review, conflicts) without opening it. It never fetches: that would fight the
/// agent for refs, and what changes while nobody looks is GitHub's answer.
///
/// Blocking: one `git status` and, when `gh`, one `gh pr list` per repo, in parallel, each with
/// its timeout. Returns the label (it names the step when it is the user's to take) and each
/// repo's state. The local state is always fresh; a PR gh didn't give keeps `last`'s. The label
/// is `Err` when gh wasn't asked or failed for any repo (offline, logged out, not GitHub), so
/// the caller keeps the last label and may back off.
pub fn check_in_background(
    ws: &Workspace,
    last: &[Repo],
    gh: bool,
    reviewed: &[String],
) -> (Result<Option<String>, String>, Vec<Repo>) {
    let p = run_job(&Job {
        pr: gh,
        ..Job::prs(ws)
    });
    background_answer(ws, p.locals, p.prs, p.parents, last, reviewed)
}

fn background_answer(
    ws: &Workspace,
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
    parents: Vec<Option<Result<Option<Pr>, String>>>,
    last: &[Repo],
    reviewed: &[String],
) -> (Result<Option<String>, String>, Vec<Repo>) {
    let failed = match &prs {
        None => Some("GitHub not asked".to_string()),
        Some(prs) => prs.iter().find_map(|p| p.as_ref().err().cloned()),
    };
    let repos = repos_from(ws, locals, prs, parents, last);
    let label = match failed {
        Some(e) => Err(e),
        None => {
            let a = next_action(&repos, reviewed);
            Ok(needs_you(&a).then_some(a.label))
        }
    };
    (label, repos)
}

/// Something is wrong (conflicts, failing checks, a git error), a review waits, or it can merge.
fn needs_you(a: &Action) -> bool {
    a.attention || !a.reviews.is_empty() || a.kind == Kind::Merge
}

fn due(at: Option<Instant>, every: Duration) -> bool {
    at.is_none_or(|t| t.elapsed() >= every)
}

impl GitSync {
    pub(crate) fn restore_settings(&mut self, state: &crate::state::State) {
        self.reviewed = state
            .reviewed
            .get(&self.ws.name)
            .cloned()
            .unwrap_or_default();
        self.merge_order = pr::merge_order(
            &self.ws,
            state
                .merge_order
                .get(&self.ws.name)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        self.delete_remote = state.delete_remote_branches;
    }

    pub fn new(ws: Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (kick, kicked) = unbounded();
        let repos = ws.repos.iter().map(|r| Repo::fresh(r, &ws)).collect();
        let _subs = vec![cx.observe_window_activation(window, |s, window, _| {
            if window.is_window_active() {
                s.kick(due(s.fetched, FETCH_ON_FOCUS), true);
            }
        })];
        let stack_prs = (ws.repos.iter())
            .map(|r| {
                (
                    r.name.clone(),
                    crate::complete::cached_repo_prs(&r.name, &r.src, true),
                )
            })
            .collect();
        GitSync {
            stack_prs,
            reviewed: vec![],
            delete_remote: false,
            merge_order: pr::merge_order(&ws, &[]),
            ws,
            chats: vec![],
            chat: None,
            repos,
            action: None,
            running: None,
            fetched: None,
            pr_checked: None,
            gh_error: None,
            fetch_error: None,
            gh_down: false,
            settling_since: None,
            want_fetch: true,
            want_pr: true,
            kick,
            kicked,
            poll: None,
            menu: None,
            chevron: Default::default(),
            _subs,
        }
    }

    /// Only the shown workspace polls; switching back catches up right away.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        match (active, self.poll.is_some()) {
            (true, false) => {
                self.want_fetch |= due(self.fetched, FETCH_ON_FOCUS);
                self.want_pr = true;
                self.start(cx);
            }
            (false, true) => self.poll = None,
            _ => {}
        }
    }

    /// A repo joined the workspace or a base changed: track it too (known repos keep their
    /// state) and re-probe.
    pub fn set_workspace(&mut self, ws: Workspace, cx: &mut Context<Self>) {
        let mut old = std::mem::take(&mut self.repos);
        self.repos = ws
            .repos
            .iter()
            .map(|r| match old.iter().position(|o| o.name == r.name) {
                // A new base has another parent; its PR is asked for again.
                Some(i) if old[i].base != r.base => Repo {
                    base: r.base.clone(),
                    parent: None,
                    ..old.swap_remove(i)
                },
                Some(i) => old.swap_remove(i),
                None => Repo::fresh(r, &ws),
            })
            .collect();
        self.merge_order = pr::merge_order(&ws, &self.merge_order);
        self.ws = ws;
        self.kick(true, true);
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let kicked = self.kicked.clone();
        // One loop does fetch, probe and gh in order, so they never overlap or race each other.
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(job) = this.update(cx, |s, cx| s.job(cx)) else {
                    break;
                };
                if job.fetch || job.pr {
                    // Show local changes now; the network part can take a while.
                    let quick = Job {
                        fetch: false,
                        pr: false,
                        ..job.clone()
                    };
                    let probed = cx
                        .background_executor()
                        .spawn(async move { run_job(&quick) })
                        .await;
                    if this.update(cx, |s, cx| s.apply(probed, cx)).is_err() {
                        break;
                    }
                }
                let probed = cx
                    .background_executor()
                    .spawn(async move { run_job(&job) })
                    .await;
                if this.update(cx, |s, cx| s.apply(probed, cx)).is_err() {
                    break;
                }
                let _ = (kicked.recv())
                    .with_timeout(LOCAL_EVERY, cx.background_executor())
                    .await;
                while kicked.try_recv().is_ok() {}
            }
        }));
    }

    fn kick(&mut self, fetch: bool, pr: bool) {
        self.want_fetch |= fetch;
        self.want_pr |= pr;
        let _ = self.kick.try_send(());
    }

    fn job(&mut self, cx: &App) -> Job {
        let settling = pr_settling(&self.repos);
        self.settling_since = match settling {
            true => self.settling_since.or(Some(Instant::now())),
            false => None,
        };
        let fast = self
            .settling_since
            .is_some_and(|t| t.elapsed() < SETTLE_FOR);
        let pr_every = match (self.gh_down, fast) {
            (true, _) => PR_BACKOFF,
            (false, true) => PR_SETTLING,
            (false, false) => PR_EVERY,
        };
        let fetch = self.want_fetch
            || due(self.fetched, FETCH_EVERY)
            || (pr_head_moved(&self.repos) && due(self.fetched, FETCH_MOVED));
        let job = Job {
            repos: worktrees(&self.ws),
            // Fetching rewrites refs/remotes; don't fight a pull or push (the agent's or a step's)
            // for the lock.
            fetch: fetch && !self.busy(cx) && self.running.is_none(),
            pr: self.want_pr || due(self.pr_checked, pr_every),
            prev: self
                .repos
                .iter()
                .map(|r| r.local.clone().unwrap_or_default())
                .collect(),
            stack: (self.ws.repos.iter())
                .map(|r| (r.name.clone(), r.src.clone()))
                .collect(),
        };
        self.want_fetch &= !job.fetch;
        self.want_pr = false;
        job
    }

    fn apply(&mut self, p: Probed, cx: &mut Context<Self>) {
        // A base or repo changed while the worker ran; its answer belongs to the old workspace.
        if p.repos != worktrees(&self.ws) {
            self.kick(true, true);
            return;
        }
        for (name, prs) in p.stack_prs {
            if let Some(prs) = prs {
                self.stack_prs.insert(name, prs);
                cx.notify();
            }
        }
        if let Some(prs) = &p.prs {
            self.gh_down = !prs.is_empty() && prs.iter().all(|p| p.is_err());
            self.gh_error = prs.iter().find_map(|pr| pr.as_ref().err().cloned());
            self.pr_checked = Some(Instant::now());
        }
        if p.fetched {
            self.fetched = Some(Instant::now());
            if self.fetch_error != p.fetch_error {
                self.fetch_error = p.fetch_error;
                cx.notify();
            }
        }
        let bases: Vec<_> = self.repos.iter().map(repo_diff_base).collect();
        self.repos = repos_from(&self.ws, p.locals, p.prs, p.parents, &self.repos);
        if p.fetched || !bases.into_iter().eq(self.repos.iter().map(repo_diff_base)) {
            cx.emit(SyncEvent::DiffBaseChanged);
        }
        let action = next_action(&self.repos, &self.reviewed);
        if self.action.as_ref() != Some(&action) {
            self.action = Some(action);
            cx.notify();
        }
    }

    // ponytail: a closed chat's two subscriptions live until the workspace closes; they hold no
    // strong ref, so this only costs a few bytes per closed tab
    pub fn add_chat(&mut self, chat: Entity<Chat>, cx: &mut Context<Self>) {
        let hold = self.running.is_some();
        chat.update(cx, |c, _| c.hold = hold);
        self._subs.extend([
            cx.observe(&chat, |_, _, cx| cx.notify()), // working toggles enable/disable the button
            cx.subscribe(&chat, |s, _, ev, cx| match ev {
                crate::chat::ChatEvent::TurnDone => {
                    s.kick(false, true); // the agent may have committed, pushed or opened a PR
                }
                crate::chat::ChatEvent::GitReviewsSent(ids) if !ids.is_empty() => {
                    for id in ids {
                        if !s.reviewed.contains(id) {
                            s.reviewed.push(id.clone());
                        }
                    }
                    cx.emit(SyncEvent::Reviewed(s.reviewed.clone()));
                }
                _ => {}
            }),
        ]);
        self.chats.push(chat);
    }

    pub fn remove_chat(&mut self, chat: &Entity<Chat>, cx: &mut Context<Self>) {
        self.chats.retain(|c| c != chat);
        if self.chat.as_ref() == Some(chat) {
            self.chat = None;
        }
        cx.notify();
    }

    pub fn set_chat(&mut self, chat: Entity<Chat>, cx: &mut Context<Self>) {
        self.chat = Some(chat);
        cx.notify();
    }

    /// Why a step can't start now: one is running, or (`idle`) the agent is.
    pub fn blocked(&self, idle: bool, cx: &App) -> Option<String> {
        if let Some(r) = &self.running {
            return Some(format!("Still running: {r}"));
        }
        (idle && self.busy(cx)).then(|| "Agent is running; wait for it or stop it (ctrl-c)".into())
    }

    fn busy(&self, cx: &App) -> bool {
        self.chats.iter().any(|c| c.read(cx).working())
    }

    fn set_hold(&self, hold: bool, cx: &mut App) {
        for c in &self.chats {
            c.update(cx, |c, cx| {
                c.hold = hold;
                cx.notify(); // the composer footer says Enter queues now
            });
        }
    }

    /// Per repo: its checked-out branch, its base, and its own branch on GitHub (`None` until
    /// it is pushed there).
    pub fn branches(&self) -> Vec<Branch> {
        self.repos
            .iter()
            .map(|r| Branch {
                repo: r.name.clone(),
                name: (r.local.as_ref().ok())
                    .and_then(|l| l.branch.clone())
                    .unwrap_or_else(|| r.branch.clone()),
                base: r.base.clone(),
                url: r.branch_url(&r.branch),
            })
            .collect()
    }

    /// The open PR target for display, or None to keep the caller's current workspace base.
    pub fn diff_base(&self, repo: &str) -> Option<String> {
        self.repos
            .iter()
            .find(|r| r.name == repo)
            .and_then(repo_diff_base)
    }

    /// `repo`'s open PR, and whether its lines are on GitHub: the PR's head is our HEAD and
    /// nothing is uncommitted.
    pub fn pr_for(&self, repo: &str) -> Option<(Pr, bool)> {
        let r = self.repos.iter().find(|r| r.name == repo)?;
        let pr = r.open_pr()?.clone();
        let pushed = (r.local.as_ref()).is_ok_and(|l| l.head == pr.head && l.changed == 0);
        Some((pr, pushed))
    }

    /// Gives `repo` open PR `number`, whose head is ours when `pushed`.
    #[cfg(all(test, feature = "test-support"))]
    pub fn set_pr(&mut self, repo: &str, number: u64, pushed: bool) {
        let r = self.repos.iter_mut().find(|r| r.name == repo).unwrap();
        r.pr = Some(Ok(Some(open_pr(repo, number))));
        let head = if pushed { "h" } else { "local" };
        r.local = Ok(Local {
            head: head.into(),
            ..Default::default()
        });
    }

    /// Applies a fresh PR target through the polling path, including its diff refresh event.
    #[cfg(all(test, feature = "test-support"))]
    pub fn set_pr_base(&mut self, repo: &str, base: &str, cx: &mut Context<Self>) {
        let prs = self
            .repos
            .iter()
            .map(|r| {
                let mut pr = r.pr.clone().unwrap_or(Ok(None));
                if r.name == repo
                    && let Ok(Some(p)) = &mut pr
                {
                    p.base = base.into();
                }
                pr
            })
            .collect();
        self.apply(
            Probed {
                repos: worktrees(&self.ws),
                locals: self.repos.iter().map(|r| r.local.clone()).collect(),
                prs: Some(prs),
                parents: vec![],
                stack_prs: vec![],
                fetched: false,
                fetch_error: None,
            },
            cx,
        );
    }

    /// Gives stacked `repo` a merged parent PR `number` with last commit `head` into `base`.
    #[cfg(all(test, feature = "test-support"))]
    pub fn set_merged_parent(&mut self, repo: &str, number: u64, head: &str, base: &str) {
        let r = self.repos.iter_mut().find(|r| r.name == repo).unwrap();
        r.parent = Some(Pr {
            state: PrState::Merged,
            head: head.into(),
            base: base.into(),
            ..open_pr(repo, number)
        });
        self.action = Some(next_action(&self.repos, &self.reviewed));
    }

    /// `(repo, PR URL)` per repo with a PR.
    pub fn pr_urls(&self) -> Vec<(String, String)> {
        self.repos
            .iter()
            .filter_map(|r| Some((r.name.clone(), r.pr()?.url.clone())))
            .collect()
    }

    pub fn open_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let urls = self.pr_urls();
        if urls.is_empty() {
            return notify_err(window, cx, "No PRs yet: create them from the git menu");
        }
        urls.iter().for_each(|(_, u)| cx.open_url(u));
    }

    /// The button: run whatever the workspace needs next.
    pub fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.action.clone() {
            Some(a) => self.run(a, true, window, cx),
            None => notify_err(window, cx, "Still reading git state…"),
        }
    }

    /// Run `kind` on every repo it applies to, chosen from the menu or the palette.
    pub fn run_kind(
        &mut self,
        kind: Kind,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_kind_in(kind, label, vec![], window, cx)
    }

    /// [`Self::run_kind`] on just `repos` (empty: all).
    pub fn run_kind_in(
        &mut self,
        kind: Kind,
        label: &str,
        repos: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let a = Action {
            label: label.into(),
            kind,
            repos,
            attention: false,
            reviews: vec![],
        };
        self.run(a, false, window, cx)
    }

    pub fn fetch_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kick(true, true);
        window.push_notification("Fetching…", cx);
    }

    /// `check`: re-probe direct operations from the button; prompts retain the user's request.
    fn run(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Kind::Agent(prompt) = &a.kind {
            if let Err(e) = check_review_checkout(&self.repos, &a.repos) {
                return notify_err(window, cx, e);
            }
            let Some(chat) = &self.chat else {
                return notify_err(window, cx, "Open a chat to queue this step");
            };
            if !chat.update(cx, |c, cx| c.enqueue(prompt, a.reviews, cx)) {
                return notify_err(window, cx, "This workspace is being archived");
            }
            return;
        }
        if let Some(e) = self.blocked(a.kind.needs_idle(), cx) {
            return notify_err(window, cx, e);
        }
        match &a.kind {
            Kind::None if a.attention => {
                let detail: Vec<String> = among(&self.repos, &a.repos).map(Repo::summary).collect();
                notify_err(window, cx, detail.join("\n"))
            }
            Kind::None => {}
            Kind::OpenPr => self.open_prs(window, cx),
            Kind::Archive => cx.emit(SyncEvent::Archive),
            Kind::Restack => cx.emit(self.restack_event(&a.repos)),
            Kind::Merge | Kind::ForcePush => self.confirm(a, check, window, cx),
            // The composer turns public as the PR title; it may have been meant for the agent.
            Kind::CreatePr | Kind::CreateDraftPr if !self.composer(cx).trim().is_empty() => {
                self.confirm(a, check, window, cx)
            }
            _ => self.exec(a, check, window, cx),
        }
    }

    fn confirm(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body, ok) = match a.kind {
            Kind::Merge => (
                match open_prs_in(&self.repos, &a.repos) {
                    0 | 1 => "Merge the PR?".to_string(),
                    n => format!("Merge {n} PRs?"),
                },
                format!(
                    "Merges with each repo's allowed method in this order: {}, and stops at the \
                     first failure. {}Merging can't be undone from here.",
                    self.merge_order.join(", "),
                    match self.delete_remote {
                        true => "Then deletes each PR's remote branch. ",
                        false => "",
                    }
                ),
                "Merge",
            ),
            Kind::CreatePr | Kind::CreateDraftPr => {
                let new = pr::NewPr::new(&self.composer(cx), false);
                (
                    "Title the PRs from the composer?".to_string(),
                    format!(
                        "Title: {}{}\n\nThe composer is cleared after. To keep it for the agent, \
                         cancel and empty it: the PRs then take their title from the commits.",
                        new.title,
                        match new.body.is_empty() {
                            true => String::new(),
                            false => format!("\nBody: {}", new.body),
                        }
                    ),
                    "Create",
                )
            }
            _ => (
                "Force push?".to_string(),
                "Replaces the remote branch with your rewritten local history. Every remote commit \
                 has an equivalent local one, and the push fails if the remote moved since."
                    .to_string(),
                "Force push",
            ),
        };
        let sync = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |d, _, _| {
            let (sync, a) = (sync.clone(), a.clone());
            d.font_features(inter_features())
                .entrance(DialogEntrance::None)
                .title(title.clone())
                .description(body.clone())
                .ok_text(ok)
                .ok_variant(ButtonVariant::Primary)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    sync.update(cx, |s, cx| s.exec(a.clone(), check, window, cx))
                        .ok();
                    true
                })
        });
    }

    /// The shown chat's draft; empty while no chat is shown.
    fn composer(&self, cx: &App) -> String {
        (self.chat.as_ref())
            .map(|c| c.read(cx).composer.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn exec(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.running = Some(a.label.clone());
        self.set_hold(true, cx);
        cx.notify();
        let mut ws = self.ws.clone();
        let mut snapshot = self.repos.clone();
        if !check && matches!(a.kind, Kind::CreatePr | Kind::CreateDraftPr) && !a.repos.is_empty() {
            ws.repos.retain(|r| a.repos.contains(&r.name));
            snapshot.retain(|r| a.repos.contains(&r.name));
        }
        let handled = self.reviewed.clone();
        let all = Job::prs(&ws);
        // Pull and push only need fresh git state.
        let pr_kinds = !matches!(a.kind, Kind::Pull | Kind::Push | Kind::ForcePush);
        let order = self.merge_order.clone();
        let delete_remote = self.delete_remote;
        let creates = matches!(a.kind, Kind::CreatePr | Kind::CreateDraftPr);
        let composer = match creates {
            true => self.composer(cx),
            false => String::new(),
        };
        let new_pr = pr::NewPr::new(&composer, a.kind == Kind::CreateDraftPr);
        // The chat whose draft became the PR title, even if another tab is shown when it lands.
        let title_chat = self.chat.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Fresh state, never the snapshot: the decision and the act must agree.
                    let fresh = run_job(&Job {
                        pr: pr_kinds,
                        ..all
                    });
                    let repos = execution_repos(&ws, fresh, &snapshot, &a)?;
                    if check {
                        let now = next_action(&repos, &handled);
                        if now.kind != a.kind || now.repos != a.repos {
                            return Err(format!(
                                "State changed; the next step is now “{}”",
                                now.label
                            ));
                        }
                    }
                    execute(&a, &ws, &repos, &order, &new_pr, delete_remote, check)
                        .map(|msg| (a, msg))
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.running = None;
                s.set_hold(false, cx);
                match result {
                    Ok((a, msg)) => {
                        cx.emit(SyncEvent::Interacted);
                        if creates {
                            cx.emit(SyncEvent::PrsCreated);
                        }
                        // The composer text became the PR title; clear it unless it was edited since.
                        if let Some(chat) =
                            title_chat.filter(|_| creates && !composer.trim().is_empty())
                        {
                            chat.update(cx, |c, cx| {
                                c.composer.update(cx, |t, cx| {
                                    if t.value() == composer {
                                        t.set_value("", window, cx)
                                    }
                                })
                            });
                        }
                        window.push_notification(
                            Notification::success(msg).title(format!("{}: done", a.label)),
                            cx,
                        )
                    }
                    Err(e) => notify_err(window, cx, e),
                }
                // Messages queued during the step can now go out.
                for c in s.chats.clone() {
                    c.update(cx, |c, cx| c.drain(cx));
                }
                s.kick(true, true); // the step may have moved the branch on GitHub
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Moves `name` one place earlier or later in the merge order, and saves it.
    fn move_repo(&mut self, name: &str, earlier: bool, cx: &mut Context<Self>) {
        pr::move_in_order(&mut self.merge_order, name, earlier);
        cx.emit(SyncEvent::MergeOrder(self.merge_order.clone()));
        cx.notify();
    }

    /// Picks a repo and moves it one place earlier (or later) in the merge order. The menu
    /// reopens on the moved repo, so `enter` again keeps moving it; `esc` is done.
    pub fn move_in_merge_order(
        &mut self,
        earlier: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.repos.len() < 2 {
            return notify_err(
                window,
                cx,
                "Only one repo; there is no merge order to change",
            );
        }
        self.order_menu(earlier, 0, window, cx)
    }

    fn order_menu(
        &mut self,
        earlier: bool,
        sel: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let verb = if earlier { "earlier" } else { "later" };
        let items = self
            .merge_order
            .iter()
            .enumerate()
            .map(|(i, n)| {
                CommandItem::new()
                    .label(format!("{}. {n}: merge {verb}", i + 1))
                    .icon(IconName::FolderGit2)
            })
            .collect();
        open_menu(
            cx.entity().downgrade(),
            "Pick a repo to move…",
            items,
            sel,
            move |s, row, window, cx| {
                let name = s.merge_order[row].clone();
                s.move_repo(&name, earlier, cx);
                let at = s.merge_order.iter().position(|n| *n == name).unwrap_or(0);
                s.order_menu(earlier, at, window, cx);
            },
            window,
            cx,
        );
    }

    /// Repos whose branch can go on archive, with the head found merged: the work landed in a
    /// merged PR, or the branch never got commits of its own.
    pub fn merged_branches(&self) -> Vec<(String, String)> {
        self.repos
            .iter()
            .filter_map(|r| {
                let l = r.local.as_ref().ok()?;
                (r.landed() || (l.base.0 == 0 && l.changed == 0))
                    .then(|| (r.name.clone(), l.head.clone()))
            })
            .collect()
    }

    /// Each repo's state, once the first probe landed.
    pub fn repos(&self) -> Option<&[Repo]> {
        self.action.as_ref().map(|_| &self.repos[..])
    }

    /// The shown workspace's own answer to `check_in_background`, from its faster polling.
    pub fn needs_you(&self) -> Option<String> {
        self.action
            .as_ref()
            .filter(|a| needs_you(a))
            .map(|a| a.label.clone())
    }

    /// The button's tooltip: the step, each repo's state, how fresh it is, and what failed.
    fn tooltip(&self, cx: &App) -> String {
        let mut lines = vec!["Next git step".to_string()];
        match (&self.running, &self.action) {
            (Some(r), _) => lines.push(format!("Running: {r}")),
            (None, Some(a)) => {
                lines.push(a.label.clone());
                if matches!(a.kind, Kind::Agent(_)) {
                    lines.push("Adds a prompt to the current chat's queue".into());
                } else if a.kind.needs_idle() && self.busy(cx) {
                    lines.push("Waits for the agent to finish".into());
                }
            }
            (None, None) => lines.push("Reading git…".into()),
        }
        lines.extend(self.repos.iter().map(Repo::summary));
        let fresh: Vec<String> = [
            ("Fetched", self.fetched),
            ("GitHub checked", self.pr_checked),
        ]
        .into_iter()
        .filter_map(|(what, at)| Some(format!("{what} {}", ago(at?.elapsed()))))
        .collect();
        if !fresh.is_empty() {
            lines.push(fresh.join(" · "));
        }
        if let Some(e) = &self.fetch_error {
            lines.push(format!("Fetch failed: {e}"));
        }
        if let Some(e) = &self.gh_error {
            lines.push(format!("GitHub: {e} (showing last good state)"));
        }
        lines.join("\n")
    }

    /// Asks the agent to move the branch onto a new base; `moves` is `(repo, old, new)`.
    pub fn rebase_onto(
        &mut self,
        moves: &[(String, String, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fill = |rs: &[String]| {
            (moves.iter().filter(|(r, ..)| rs.contains(r)))
                .map(|(r, old, new)| format!("{r}: `{old}` → `{new}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let branch_of = |name: &str| {
            let r = self.repos.iter().find(|r| r.name == name);
            r.map_or(self.ws.branch.as_str(), |r| r.branch.as_str())
        };
        let repos = moves.iter().map(|(r, ..)| (r.as_str(), branch_of(r)));
        let prompt = crate::prompts::render_each("rebase_base", repos, fill);
        self.run_kind(Kind::Agent(prompt), "Rebase onto new base", window, cx)
    }

    /// The merged parents' repos onto their parents' bases, and the prompt that restacks them,
    /// written now: the new base drops the parent, and with it the parent's last commit.
    fn restack_event(&self, rs: &[String]) -> SyncEvent {
        let moving: Vec<&Repo> = among(&self.repos, rs)
            .filter(|r| r.parent_merged())
            .collect();
        let onto = |r: &Repo| format!("origin/{}", r.parent.as_ref().map_or("", |p| &p.base));
        let moves = (moving.iter())
            .map(|r| (r.name.clone(), r.base.clone(), onto(r)))
            .collect();
        let fill = |names: &[String]| {
            (moving.iter().filter(|r| names.contains(&r.name)))
                .map(|r| status::restack_line(r, &onto(r)))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let pairs = moving.iter().map(|r| (&*r.name, &*r.branch));
        let prompt = crate::prompts::render_each("restack", pairs, fill);
        SyncEvent::Restack { moves, prompt }
    }

    /// Asks the agent to rebase every stacked repo onto its parent's latest commits.
    pub fn restack_onto_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stacked: Vec<&Repo> = self
            .repos
            .iter()
            .filter(|r| r.stacked().is_some())
            .collect();
        if stacked.is_empty() {
            return notify_err(window, cx, "Not stacked: its base is the trunk");
        }
        let fill = |names: &[String]| {
            (stacked.iter().filter(|r| names.contains(&r.name)))
                .map(|r| status::restack_line(r, &r.base))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let pairs = stacked.iter().map(|r| (&*r.name, &*r.branch));
        let prompt = crate::prompts::render_each("restack", pairs, fill);
        self.run_kind(Kind::Agent(prompt), "Rebase onto parent", window, cx)
    }

    /// Available even before the agent has made any changes.
    pub fn commit_and_push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repos = self.repos.iter().map(|r| (&*r.name, &*r.branch));
        let prompt = crate::prompts::render_each("commit", repos, |rs| rs.join(", "));
        self.run_kind(Kind::Agent(prompt), "Commit and push", window, cx)
    }

    /// Asks the agent to bring every repo up to date with its base.
    pub fn update_from_base(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bases = |rs: &[String]| {
            (self.repos.iter().filter(|r| rs.contains(&r.name)))
                .map(|r| format!("{}: `{}`", r.name, r.base))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let repos = self.repos.iter().map(|r| (&*r.name, &*r.branch));
        let prompt = crate::prompts::render_each("update_base", repos, bases);
        self.run_kind(Kind::Agent(prompt), "Update from base", window, cx)
    }

    /// The git menu, anchored under the chevron: the next step, syncing, PRs, then the one
    /// setting. Repo states and errors stay in the button's tooltip.
    pub fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let run = |f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let this = this.clone();
            move |w: &mut Window, cx: &mut App| {
                this.update(cx, |s, cx| f(s, w, cx)).ok();
            }
        };
        // One repo acts directly; several make a submenu of repos plus "All".
        let kind = |icon, k: Kind, label: &'static str, rows| {
            let this = this.clone();
            each_or_all(icon, label, rows, move |rs, w, cx| {
                let rs = rs.to_vec();
                let label = label.trim_end_matches('…');
                this.update(cx, |s, cx| s.run_kind_in(k.clone(), label, rs, w, cx))
                    .ok();
            })
        };
        let rows = |f: &dyn Fn(&Repo) -> Option<String>| -> Vec<(String, String, String)> {
            (self.repos.iter())
                .filter(|r| !r.local.as_ref().is_ok_and(|l| l.review))
                .filter_map(|r| Some((r.name.clone(), f(r)?, r.name.clone())))
                .collect()
        };
        let every = rows(&|_| Some(String::new()));
        let mut entries = vec![];
        if let Some(a) = self.action.clone().filter(|a| a.kind != Kind::None) {
            entries.push(menu::item(icon(&a), a.label.clone(), run(Self::primary)).hint("Next"));
            entries.push(Entry::Sep);
        }
        let pull = kind(IconName::ArrowDown, Kind::Pull, "Pull", every.clone());
        entries.extend(
            [
                Some(menu::item(
                    IconName::RefreshCw,
                    "Fetch now",
                    run(Self::fetch_now),
                )),
                Some(
                    menu::item(
                        IconName::GitCommitHorizontal,
                        "Commit and push",
                        run(Self::commit_and_push),
                    )
                    .hint("Queue"),
                ),
                pull.map(|e| e.hint("Fast-forward")),
                kind(IconName::ArrowUp, Kind::Push, "Push", every.clone()),
                Some(
                    menu::item(
                        IconName::GitCompareArrows,
                        "Update from base",
                        run(Self::update_from_base),
                    )
                    .hint("Queue"),
                ),
                Some(Entry::Sep),
                kind(
                    IconName::GitPullRequestCreate,
                    Kind::CreatePr,
                    "Create or update PRs",
                    every.clone(),
                ),
                kind(
                    IconName::GitPullRequestDraft,
                    Kind::CreateDraftPr,
                    "Create draft PRs",
                    every,
                ),
                pr_entry(self),
                kind(
                    IconName::GitMerge,
                    Kind::Merge,
                    "Merge PRs…",
                    rows(&|r| Some(format!("#{}", r.open_pr()?.number))),
                ),
            ]
            .into_iter()
            .flatten(),
        );
        if self.repos.len() > 1 {
            // Each repo's possible moves, in merge order: the first can't go earlier.
            let n = self.merge_order.len();
            let mut moves = vec![];
            for (i, name) in self.merge_order.iter().enumerate() {
                if i > 0 {
                    moves.push(Entry::Sep);
                }
                for (earlier, ok, icon, verb) in [
                    (true, i > 0, IconName::ArrowUp, "earlier"),
                    (false, i + 1 < n, IconName::ArrowDown, "later"),
                ] {
                    let (this, name) = (this.clone(), name.clone());
                    moves.extend(ok.then(|| {
                        menu::item(icon, format!("Merge {name} {verb}"), move |_, cx| {
                            this.update(cx, |s, cx| s.move_repo(&name, earlier, cx))
                                .ok();
                        })
                    }));
                }
            }
            entries.push(
                menu::sub(IconName::ListOrdered, "Merge order", moves)
                    .hint(self.merge_order.join(" → ")),
            );
        }
        entries.push(Entry::Sep);
        // A setting: the check when it's on, "Off" when not, so it never reads as "delete now".
        let on = self.delete_remote;
        let owner = cx.entity().downgrade();
        let delete = menu::item(
            IconName::GitBranch,
            "Delete remote branches after merge",
            move |_, cx| {
                owner
                    .update(cx, |s, cx| {
                        cx.emit(SyncEvent::DeleteRemote(!s.delete_remote))
                    })
                    .ok();
            },
        );
        entries.push(if on {
            delete.checked(true)
        } else {
            delete.hint("Off")
        });
        let b = self.chevron.get();
        let at = point(b.right(), b.bottom() + px(4.));
        let m = menu::open_at(entries, at, Anchor::TopRight, window, cx);
        let sub = cx.subscribe(&m, |s, _, _: &DismissEvent, cx| {
            s.menu = None;
            cx.notify();
        });
        self.menu = Some((m, sub));
        cx.notify();
    }
}

fn repo_diff_base(repo: &Repo) -> Option<String> {
    // Review checkouts have private refs from the target repository, which may not be origin.
    repo.open_pr()
        .filter(|p| {
            !p.base.is_empty()
                && !repo.base.starts_with("refs/shark/pull/")
                && !repo.local.as_ref().is_ok_and(|l| l.review)
        })
        .map(|pr| format!("origin/{}", pr.base))
}

#[cfg(test)]
fn open_pr(repo: &str, number: u64) -> Pr {
    Pr {
        url: format!("https://github.com/o/{repo}/pull/{number}"),
        number,
        state: PrState::Open,
        draft: false,
        merge_state: "CLEAN".into(),
        review: String::new(),
        checks: status::Checks::Passing,
        head: "h".into(),
        base: "main".into(),
        change_requests: vec![],
        comments: vec![],
        contains_head: String::new(),
    }
}

/// Open PRs in the repos named in `rs` (empty: all), as [`execute`] targets them.
fn open_prs_in(repos: &[Repo], rs: &[String]) -> usize {
    (repos.iter())
        .filter(|r| (rs.is_empty() || rs.contains(&r.name)) && r.open_pr().is_some())
        .count()
}

fn check_review_checkout(repos: &[Repo], rs: &[String]) -> Result<(), String> {
    if let Some(r) = repos.iter().find(|r| {
        (rs.is_empty() || rs.contains(&r.name)) && r.local.as_ref().is_ok_and(|l| l.review)
    }) {
        return Err(format!(
            "{}: this is a local PR review checkout; use the original PR on GitHub to manage it",
            r.name
        ));
    }
    Ok(())
}

/// One repo's part of a step: `Ok(None)` when it has nothing to do.
type Step<'a> = dyn Fn(&std::path::Path, &Repo) -> Result<Option<String>, String> + 'a;

/// Runs a step on the target repos. Blocking; called on a background thread.
fn execute(
    a: &Action,
    ws: &Workspace,
    repos: &[Repo],
    order: &[String],
    new_pr: &pr::NewPr,
    delete_remote: bool,
    primary: bool,
) -> Result<String, String> {
    check_review_checkout(repos, &a.repos)?;
    let dir = ws.dir();
    // The next-step button also links existing PRs after a partial creation; explicit scopes stay narrow.
    let wanted = |r: &Repo| {
        a.repos.is_empty()
            || a.repos.contains(&r.name)
            || (primary
                && matches!(a.kind, Kind::CreatePr | Kind::CreateDraftPr)
                && !r.local.as_ref().is_ok_and(|l| l.review))
    };
    let mut targets: Vec<&Repo> = repos.iter().filter(|r| wanted(r)).collect();
    if a.kind == Kind::Merge {
        // A repo missing from the order goes last, not first (`None` sorts before `Some`).
        targets.sort_by_key(|r| {
            order
                .iter()
                .position(|n| *n == r.name)
                .unwrap_or(usize::MAX)
        });
    }
    let delete_remote = a.kind == Kind::Merge && delete_remote;
    let each = |f: &Step| {
        let mut done = vec![];
        for r in targets.iter().copied() {
            match f(&dir.join(&r.name), r) {
                Ok(Some(m)) => done.push(format!("{}: {m}", r.name)),
                Ok(None) => {}
                Err(e) => {
                    let prefix = match done.is_empty() {
                        true => String::new(),
                        false => format!("Done before the failure: {}\n", done.join(", ")),
                    };
                    return Err(format!("{prefix}{}: {e}", r.name));
                }
            }
        }
        match done.is_empty() {
            true => Err("Nothing to do".into()),
            false => Ok(done.join("\n")),
        }
    };
    let planned = |r: &Repo| status::step_args(&a.kind, r);
    match &a.kind {
        Kind::Pull | Kind::Push | Kind::ForcePush => each(&|wt, r| {
            let Some(args) = planned(r)? else {
                return Ok(None);
            };
            cmd_bounded(wt, "git", &argv(&args), 120).map_err(|e| match a.kind {
                Kind::Pull if diverged(&e) => {
                    format!("{e}\nThe branches diverged; use “Sync with remote”.")
                }
                Kind::Push => format!("{e}\nIf the remote moved, pull first."),
                _ => e,
            })?;
            Ok(Some(match a.kind {
                Kind::Pull => "pulled".into(),
                Kind::Push => "pushed".into(),
                _ => "force-pushed".into(),
            }))
        }),
        Kind::CreatePr | Kind::CreateDraftPr => {
            let landed: Vec<String> = repos
                .iter()
                .filter(|r| r.landed())
                .map(|r| r.name.clone())
                .collect();
            let mut selected = ws.clone();
            selected
                .repos
                .retain(|r| targets.iter().any(|target| target.name == r.name));
            let prs = pr::create_linked_prs(&selected, &dir, &landed, new_pr)?;
            Ok(prs
                .iter()
                .map(|(r, u)| format!("{r}: {u}"))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        Kind::Reopen => each(&|wt, r| {
            gh_on_pr(
                wt,
                r,
                &["pr", "reopen"],
                status::PrState::Closed,
                "reopened",
            )
        }),
        Kind::UpdateBranch => each(&|wt, r| {
            if r.open_pr().is_none_or(|p| p.merge_state != "BEHIND") {
                return Ok(None);
            }
            gh_on_pr(
                wt,
                r,
                &["pr", "update-branch"],
                status::PrState::Open,
                "updated from base",
            )
        }),
        Kind::Ready => each(&|wt, r| {
            if r.open_pr().is_none_or(|p| !p.draft) {
                return Ok(None);
            }
            gh_on_pr(
                wt,
                r,
                &["pr", "ready"],
                status::PrState::Open,
                "ready for review",
            )
        }),
        Kind::Merge => each(&|wt, r| {
            let Some(mut args) = planned(r)? else {
                return Ok(None);
            };
            args.push(merge_method(wt)?.into());
            // Run from the linked worktree, gh deletes only the remote branch: it skips the
            // local one that is checked out here (archive deletes that).
            if delete_remote {
                args.push("--delete-branch".into());
            }
            cmd_bounded(wt, "gh", &argv(&args), 60).map_err(|e| match e.contains("timed out") {
                true => format!(
                    "{e}\nIt may still have merged; the button updates once GitHub answers."
                ),
                false => e,
            })?;
            Ok(Some(format!(
                "merged #{}",
                r.open_pr().map_or(0, |p| p.number)
            )))
        }),
        // `run` handles these without a step.
        Kind::Agent(_) | Kind::Archive | Kind::Restack | Kind::OpenPr | Kind::None => {
            unreachable!()
        }
    }
}

/// `(worktree, base, branch)` per repo of `ws`.
fn worktrees(ws: &Workspace) -> Vec<(PathBuf, String, String)> {
    let dir = ws.dir();
    ws.repos
        .iter()
        .map(|r| (dir.join(&r.name), r.base.clone(), ws.branch_of(r).into()))
        .collect()
}

/// Assemble probe results identically for polling, background attention and execution.
fn repos_from(
    ws: &Workspace,
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
    parents: Vec<Option<Result<Option<Pr>, String>>>,
    last: &[Repo],
) -> Vec<Repo> {
    let prs = prs
        .into_iter()
        .flatten()
        .map(Some)
        .chain(std::iter::repeat(None));
    let parents = parents.into_iter().chain(std::iter::repeat(None));
    ws.repos
        .iter()
        .zip(locals)
        .zip(prs.zip(parents))
        .map(|((r, local), (pr, parent))| {
            let old = last
                .iter()
                .find(|old| old.name == r.name && old.branch == ws.branch_of(r));
            Repo {
                local,
                pr: fresh_or_last(pr.as_ref(), &old.and_then(|r| r.pr.clone())),
                parent: match parent {
                    Some(Ok(parent)) => parent,
                    _ => old
                        .filter(|old| old.base == r.base)
                        .and_then(|r| r.parent.clone()),
                },
                ..Repo::fresh(r, ws)
            }
        })
        .collect()
}

/// A merge needs a fresh parent answer; cached display state must never authorize it.
fn execution_repos(
    ws: &Workspace,
    fresh: Probed,
    last: &[Repo],
    action: &Action,
) -> Result<Vec<Repo>, String> {
    if action.kind == Kind::Merge {
        for (i, r) in ws.repos.iter().enumerate() {
            if !action.repos.is_empty() && !action.repos.contains(&r.name) {
                continue;
            }
            let own = fresh.prs.as_ref().and_then(|prs| prs.get(i));
            let parent = fresh.parents.get(i).and_then(|parent| parent.as_ref());
            for (what, result) in [("PR", own), ("parent PR", parent)] {
                if let Some(Err(e)) = result {
                    return Err(format!("{}: can't check the {what}: {e}", r.name));
                }
            }
        }
    }
    Ok(repos_from(ws, fresh.locals, fresh.prs, fresh.parents, last))
}

/// This repo's PR as just asked, or the last answer when gh wasn't asked or failed (a blip).
fn fresh_or_last(
    fresh: Option<&Result<Option<Pr>, String>>,
    last: &Option<Result<Option<Pr>, String>>,
) -> Option<Result<Option<Pr>, String>> {
    match (fresh, last) {
        (None, last) | (Some(Err(_)), last @ Some(Ok(_))) => last.clone(),
        (Some(fresh), _) => Some(fresh.clone()),
    }
}

fn argv(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn gh_on_pr(
    wt: &std::path::Path,
    r: &Repo,
    args: &[&str],
    state: status::PrState,
    done: &str,
) -> Result<Option<String>, String> {
    let Some(p) = r.pr().filter(|p| p.state == state) else {
        return Ok(None);
    };
    cmd_bounded(wt, "gh", &[args, &[p.url.as_str()]].concat(), 60)?;
    Ok(Some(format!("#{} {done}", p.number)))
}

/// The repo's allowed method, preferring squash. `gh pr merge` needs one when run headless.
fn merge_method(wt: &std::path::Path) -> Result<&'static str, String> {
    let json = cmd_bounded(
        wt,
        "gh",
        &[
            "repo",
            "view",
            "--json",
            "squashMergeAllowed,mergeCommitAllowed,rebaseMergeAllowed",
        ],
        30,
    )?;
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    [
        ("squashMergeAllowed", "--squash"),
        ("mergeCommitAllowed", "--merge"),
        ("rebaseMergeAllowed", "--rebase"),
    ]
    .into_iter()
    .find(|(k, _)| v[k] == true)
    .map(|(_, f)| f)
    .ok_or_else(|| "the repo allows no merge method".into())
}

fn icon(a: &Action) -> IconName {
    match a.kind {
        Kind::Agent(_) if a.attention => IconName::TriangleAlert,
        Kind::Agent(_) => IconName::GitCommitHorizontal,
        Kind::Pull => IconName::ArrowDown,
        Kind::Push | Kind::ForcePush => IconName::ArrowUp,
        Kind::CreatePr => IconName::GitPullRequestCreate,
        Kind::CreateDraftPr => IconName::GitPullRequestDraft,
        Kind::Reopen | Kind::Ready | Kind::OpenPr => IconName::GitPullRequest,
        Kind::UpdateBranch => IconName::RefreshCw,
        Kind::Merge => IconName::GitMerge,
        Kind::Archive => IconName::Archive,
        Kind::Restack => IconName::TriangleAlert,
        Kind::None if a.attention => IconName::TriangleAlert,
        Kind::None => IconName::Check,
    }
}

impl Render for GitSync {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy(cx);
        let this = cx.entity().downgrade();
        let warning = cx.theme().warning_foreground;
        let (label, a) = match (&self.running, &self.action) {
            (_, Some(a)) if matches!(a.kind, Kind::Agent(_)) => (a.label.clone(), Some(a.clone())),
            (Some(r), _) => (format!("{r}…"), None),
            (None, Some(a)) => (a.label.clone(), Some(a.clone())),
            (None, None) => ("Reading git…".into(), None),
        };
        let blocked = busy && a.as_ref().is_some_and(|a| a.kind.needs_idle());
        let clickable = a
            .as_ref()
            .is_some_and(|a| a.kind != Kind::None || a.attention);
        let passive = a
            .as_ref()
            .is_none_or(|a| matches!(a.kind, Kind::None | Kind::OpenPr));
        let attention = a.as_ref().is_some_and(|a| a.attention);
        let icon = match &a {
            Some(a) => Icon::new(icon(a)).when(attention, |i| i.text_color(warning)),
            None => Icon::new(IconName::RefreshCw), // static: only the agent gets a spinner
        };
        let variant = |b: Button| match (passive, attention) {
            (true, _) => b.ghost(),
            (false, true) => b.outline(),
            (false, false) => b.primary(),
        };
        let theme = cx.theme();
        let divider = match (passive, attention) {
            (false, false) => theme.primary_foreground.opacity(0.3),
            _ => theme.border,
        };
        // One split button: the chevron trails inside it, behind a hairline.
        let radius = theme.radius;
        let split = h_flex()
            .child(
                // A div, not Button::tooltip: that takes fixed text, and this is built on hover so
                // the "ago" times are current.
                div()
                    .id("git-next-tip")
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|s, _: &MouseDownEvent, w, cx| s.open_menu(w, cx)),
                    )
                    .tooltip(move |w, cx| {
                        let text = this.upgrade().map(|s| s.read(cx).tooltip(cx));
                        Tooltip::new(text.unwrap_or_default()).build(w, cx)
                    })
                    .child(
                        Button::new("git-next")
                            .small()
                            .h(px(28.))
                            .px(px(10.))
                            .icon(icon)
                            .label(label)
                            .rounded(ButtonRounded::None)
                            .rounded_l(radius)
                            .map(variant)
                            .disabled(!clickable || blocked)
                            .on_click(cx.listener(|s, _, window, cx| s.primary(window, cx))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .child(
                        Button::new("git-menu")
                            .small()
                            .h(px(28.))
                            .px(px(6.))
                            .icon(IconName::ChevronDown)
                            .rounded(ButtonRounded::None)
                            .rounded_r(radius)
                            .map(variant)
                            .tooltip("Git menu")
                            .on_click(cx.listener(|s, _, window, cx| s.open_menu(window, cx))),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(6.))
                            .bottom(px(6.))
                            .w(px(1.))
                            .bg(divider),
                    )
                    .child({
                        let at = self.chevron.clone();
                        canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                    }),
            );
        div()
            .child(split)
            .children(self.menu.as_ref().map(|(m, _)| menu::layer(m.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, Job, Kind, Local, Pr, background_answer, fresh_or_last, needs_you, repos_from,
        run_job, status,
    };

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn git_prompts_queue_while_busy(cx: &mut gpui_kit::TestAppContext) {
        use super::*;
        let _lock = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(gpui_kit::init);
        let ws = crate::state::Workspace {
            name: "git-queue-test".into(),
            branch: "feature".into(),
            repos: vec![crate::state::WsRepo {
                name: "api".into(),
                src: "/nonexistent".into(),
                base: "origin/main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        let (window, sync) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| GitSync::new(ws.clone(), w, cx))
                })
            })
            .unwrap();
        cx.update_window(window, |_, w, cx| {
            let chat =
                cx.new(|cx| Chat::new(ws.clone(), crate::state::ChatMeta::default(), false, w, cx));
            chat.update(cx, |c, cx| {
                c.busy = true;
                c.composer
                    .update(cx, |t, cx| t.set_value("keep my draft", w, cx));
            });
            sync.update(cx, |s, cx| {
                s.add_chat(chat.clone(), cx);
                s.set_chat(chat.clone(), cx);
                s.repos[0].local = Ok(Local {
                    branch: Some("feature".into()),
                    ..Default::default()
                });
                // Clean repos can be scheduled before the agent has written anything.
                s.commit_and_push(w, cx);
                assert_eq!(chat.read(cx).queued(), 1);
                assert!(s.running.is_none(), "no probe or git operation starts");
                s.repos[0].local.as_mut().unwrap().changed = 1;
                s.action = Some(next_action(&s.repos, &[]));
                s.running = Some("Pull".into());
                s.set_hold(true, cx);
                s.primary(w, cx);
                s.update_from_base(w, cx);
                assert_eq!(chat.read(cx).queued(), 3);
                assert_eq!(s.running.as_deref(), Some("Pull"));
                assert_eq!(
                    chat.read(cx).composer.read(cx).value().as_ref(),
                    "keep my draft"
                );
                // Direct operations still refuse to race either git or an agent.
                s.run_kind(Kind::Push, "Push", w, cx);
                assert_eq!(s.running.as_deref(), Some("Pull"));
                s.running = None;
                s.set_hold(false, cx);
                s.run_kind(Kind::Pull, "Pull", w, cx);
                assert!(s.running.is_none());
                s.repos[0].local.as_mut().unwrap().review = true;
                s.commit_and_push(w, cx);
                assert_eq!(chat.read(cx).queued(), 3, "review checkouts stay protected");
                assert!(chat.read(cx).sent_texts().is_empty());
            });
        })
        .unwrap();
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn primary_pr_retry_links_existing_prs_but_explicit_scope_stays_narrow() {
        use super::*;
        use crate::git::git;
        use std::{fs, os::unix::fs::PermissionsExt};
        let _lock = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let name = format!("pr-retry-{}", std::process::id());
        let root = std::env::temp_dir().join(&name);
        let mut ws = Workspace {
            name,
            branch: "feature".into(),
            ..Default::default()
        };
        let dir = ws.dir();
        fs::create_dir_all(&root).unwrap();
        for name in ["api", "web"] {
            let wt = dir.join(name);
            let origin = root.join(format!("{name}.git"));
            fs::create_dir_all(&wt).unwrap();
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            git(&wt, &["init", "-q", "-b", "main"]).unwrap();
            let commit = |message| {
                git(
                    &wt,
                    &[
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "commit",
                        "--allow-empty",
                        "-qm",
                        message,
                    ],
                )
                .unwrap();
            };
            commit("initial");
            git(&wt, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
            git(&wt, &["push", "-q", "origin", "main"]).unwrap();
            git(&wt, &["checkout", "-qb", "feature"]).unwrap();
            commit("work");
            ws.repos.push(crate::state::WsRepo {
                name: name.into(),
                src: wt,
                base: "origin/main".into(),
                branch: None,
            });
        }
        let previous = std::env::var_os("PATH").unwrap();
        struct Restore(std::ffi::OsString);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe { std::env::set_var("PATH", &self.0) };
            }
        }
        let _restore = Restore(previous.clone());
        let gh = root.join("gh");
        fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
case "$PWD" in */'{name}'/*) ;; *) PATH='{previous}' exec gh "$@" ;; esac
repo=$(basename "$PWD")
echo "$repo $1 $2" >> '{root}/calls'
case "$1 $2" in
"pr list") [ ! -f '{root}/'$repo.open ] || echo https://gh/$repo/1 ;;
"pr create")
  if [ "$repo" = web ] && [ -f '{root}/fail' ]; then echo 'gh: boom' >&2; exit 1; fi
  touch '{root}/'$repo.open
  echo https://gh/$repo/1 ;;
"pr view") echo Body ;;
"pr edit") echo "$5" > '{root}/'$repo.body ;;
*) exit 1 ;;
esac
"#,
                name = ws.name,
                root = root.display(),
                previous = previous.to_string_lossy()
            ),
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        unsafe {
            std::env::set_var(
                "PATH",
                format!("{}:{}", root.display(), previous.to_string_lossy()),
            )
        };
        let snapshot = || -> Vec<Repo> {
            ws.repos
                .iter()
                .map(|r| {
                    let local =
                        status::probe(&dir.join(&r.name), "feature", &r.base, None).unwrap();
                    let pr = root.join(format!("{}.open", r.name)).exists().then(|| Pr {
                        head: local.head.clone(),
                        ..open_pr(&r.name, 1)
                    });
                    Repo {
                        local: Ok(local),
                        pr: Some(Ok(pr)),
                        ..Repo::fresh(r, &ws)
                    }
                })
                .collect()
        };
        let repos = snapshot();
        let action = next_action(&repos, &[]);
        assert_eq!(action.kind, Kind::CreatePr);
        assert_eq!(action.repos, ["api", "web"]);
        fs::write(root.join("fail"), "").unwrap();
        let new_pr = pr::NewPr::new("", false);
        let error = execute(&action, &ws, &repos, &[], &new_pr, false, true).unwrap_err();
        assert!(error.contains("boom"), "{error}");
        assert!(root.join("api.open").exists() && !root.join("web.open").exists());
        assert!(!root.join("api.body").exists());
        fs::remove_file(root.join("fail")).unwrap();
        let mut repos = snapshot();
        // A mixed workspace must not publish its local PR review checkout.
        ws.repos.push(crate::state::WsRepo {
            name: "review".into(),
            src: Default::default(),
            base: "origin/main".into(),
            branch: None,
        });
        repos.push(Repo {
            local: Ok(Local {
                review: true,
                ..Default::default()
            }),
            ..Repo::fresh(ws.repos.last().unwrap(), &ws)
        });
        let retry = next_action(&repos, &[]);
        assert_eq!(retry.kind, Kind::CreatePr);
        assert_eq!(retry.repos, ["web"]);
        execute(&retry, &ws, &repos, &[], &new_pr, false, true).unwrap();
        for (repo, other) in [("api", "web"), ("web", "api")] {
            assert!(
                fs::read_to_string(root.join(format!("{repo}.body")))
                    .unwrap()
                    .contains(&format!("- {other}: https://gh/{other}/1"))
            );
        }
        let web_body = fs::read(root.join("web.body")).unwrap();
        let web_head = git(&root.join("web.git"), &["rev-parse", "feature"]).unwrap();
        fs::write(dir.join("web/dirty"), "leave untouched").unwrap();
        fs::write(root.join("calls"), "").unwrap();
        let explicit = Action {
            repos: vec!["api".into()],
            ..retry
        };
        execute(&explicit, &ws, &repos, &[], &new_pr, false, false).unwrap();
        let calls = fs::read_to_string(root.join("calls")).unwrap();
        assert!(!calls.contains("web") && !calls.contains("review") && !calls.contains("pr edit"));
        assert_eq!(fs::read(root.join("web.body")).unwrap(), web_body);
        assert_eq!(
            git(&root.join("web.git"), &["rev-parse", "feature"]).unwrap(),
            web_head
        );
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn selected_pr_action_does_not_check_an_unselected_checkout() {
        use super::*;
        use std::fs;
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
        let ws = Workspace {
            name: format!("scope-test-{}", std::process::id()),
            branch: "main".into(),
            repos: ["api", "web"]
                .map(|n| crate::state::WsRepo {
                    name: n.into(),
                    src: Default::default(),
                    base: "HEAD".into(),
                    branch: None,
                })
                .into(),
            ..Default::default()
        };
        let dir = ws.dir();
        let api = dir.join("api");
        fs::create_dir_all(&api).unwrap();
        fs::create_dir(dir.join("web")).unwrap(); // deliberately unavailable repo
        crate::git::git(&api, &["init", "-q", "-b", "main"]).unwrap();
        crate::git::git(
            &api,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        )
        .unwrap();
        let repos: Vec<_> = ws.repos.iter().map(|r| Repo::fresh(r, &ws)).collect();
        let mut action = Action {
            kind: Kind::CreateDraftPr,
            label: "Create".into(),
            repos: vec!["api".into()],
            attention: false,
            reviews: vec![],
        };
        let error = execute(
            &action,
            &ws,
            &repos,
            &[],
            &pr::NewPr::new("", true),
            false,
            false,
        )
        .unwrap_err();
        assert_eq!(error, "no repo has commits ahead of its base");
        action.repos.clear();
        assert!(
            execute(
                &action,
                &ws,
                &repos,
                &[],
                &pr::NewPr::new("", true),
                false,
                false
            )
            .unwrap_err()
            .contains("web")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn diff_base_follows_only_open_prs_and_keeps_review_refs() {
        use super::{PrState, repo_diff_base};
        let (ws, p) = stacked_probe();
        let mut repos = repos_from(&ws, p.locals, p.prs, p.parents, &[]);
        let r = &mut repos[0];
        r.base = "origin/main".into();
        assert_eq!(repo_diff_base(r).as_deref(), Some("origin/parent"));
        for state in [PrState::Closed, PrState::Merged] {
            r.pr = Some(Ok(Some(Pr {
                state,
                ..super::open_pr("api", 2)
            })));
            assert_eq!(repo_diff_base(r), None);
        }
        let mut pr = super::open_pr("api", 2);
        pr.base.clear();
        r.pr = Some(Ok(Some(pr.clone())));
        assert_eq!(repo_diff_base(r), None);
        pr.base = "parent".into();
        r.pr = Some(Ok(Some(pr)));
        r.base = "refs/shark/pull/2/base".into();
        assert_eq!(repo_diff_base(r), None);
        r.base = "release".into();
        assert_eq!(
            repo_diff_base(r).as_deref(),
            Some("origin/parent"),
            "origin may be added after workspace creation"
        );
        r.base = "origin/main".into();
        r.local.as_mut().unwrap().review = true;
        assert_eq!(
            repo_diff_base(r),
            None,
            "review identities keep their saved base"
        );
        r.local.as_mut().unwrap().review = false;
        for pr in [None, Some(Ok(None)), Some(Err("offline".into()))] {
            r.pr = pr;
            assert_eq!(repo_diff_base(r), None);
        }
        let (_, p) = stacked_probe();
        let last = repos_from(&ws, p.locals, p.prs, p.parents, &[]);
        let (_, p) = stacked_probe();
        let retained = repos_from(
            &ws,
            p.locals,
            Some(vec![Err("offline".into())]),
            vec![],
            &last,
        );
        assert_eq!(
            repo_diff_base(&retained[0]).as_deref(),
            Some("origin/parent")
        );
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn changed_pr_target_emits_diff_refresh_without_head_change(cx: &mut gpui_kit::TestAppContext) {
        use super::*;
        let _lock = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(gpui_kit::init);
        let (mut ws, _) = stacked_probe();
        ws.repos[0].base = "origin/main".into();
        ws.repos.push(crate::state::WsRepo {
            name: "web".into(),
            ..ws.repos[0].clone()
        });
        let (_, sync) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| GitSync::new(ws.clone(), w, cx))
                })
            })
            .unwrap();
        let events = Rc::new(Cell::new(0));
        let count = events.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&sync, move |_, event: &SyncEvent, _| {
                if matches!(event, SyncEvent::DiffBaseChanged) {
                    count.set(count.get() + 1);
                }
            })
        });
        cx.update(|cx| {
            sync.update(cx, |s, cx| {
                assert_eq!(
                    s.diff_base("api"),
                    None,
                    "no PR must not override current workspace state"
                );
                s.set_pr("api", 2, true);
                s.set_pr("web", 3, true);
                s.set_pr_base("api", "parent", cx);
                assert_eq!(s.diff_base("api").as_deref(), Some("origin/parent"));
                assert_eq!(s.diff_base("web").as_deref(), Some("origin/main"));
                assert_eq!(
                    s.ws.repos[0].base, "origin/main",
                    "workspace intent is unchanged"
                );
                assert_eq!(s.repos[0].local.as_ref().unwrap().head, "h");
            })
        });
        assert_eq!(events.get(), 1);
        cx.update(|cx| sync.update(cx, |s, cx| s.set_pr_base("api", "parent", cx)));
        assert_eq!(events.get(), 1, "unchanged answers do not refresh the diff");
        cx.update(|cx| sync.update(cx, |s, cx| s.set_pr_base("web", "release", cx)));
        assert_eq!(events.get(), 2);
        cx.update(|cx| sync.update(cx, |s, cx| s.set_pr_base("api", "", cx)));
        assert_eq!(
            events.get(),
            3,
            "falling back to the workspace base also refreshes"
        );
        cx.update(|cx| {
            sync.update(cx, |s, cx| {
                s.apply(
                    Probed {
                        repos: worktrees(&s.ws),
                        locals: s.repos.iter().map(|r| r.local.clone()).collect(),
                        prs: None,
                        parents: vec![],
                        stack_prs: vec![],
                        fetched: true,
                        fetch_error: Some("one remote failed".into()),
                    },
                    cx,
                );
            })
        });
        assert_eq!(
            events.get(),
            4,
            "even a partial fetch can add or move a base ref"
        );
    }

    #[test]
    fn review_checkout_refuses_git_lifecycle_actions() {
        let repo = super::Repo {
            name: "api".into(),
            base: "origin/main".into(),
            branch: "pr/7".into(),
            local: Ok(Local {
                review: true,
                ..Default::default()
            }),
            pr: Some(Ok(Some(super::open_pr("api", 7)))),
            parent: None,
        };
        for kind in [
            Kind::Push,
            Kind::Pull,
            Kind::Merge,
            Kind::CreatePr,
            Kind::Ready,
            Kind::Agent("commit and push".into()),
        ] {
            let action = Action {
                kind,
                label: String::new(),
                repos: vec![],
                attention: false,
                reviews: vec![],
            };
            let result = super::execute(
                &action,
                &Default::default(),
                std::slice::from_ref(&repo),
                &[],
                &crate::pr::NewPr::new("", false),
                false,
                false,
            );
            assert!(matches!(result, Err(e) if e.contains("local PR review checkout")));
        }
    }

    #[test]
    fn merge_confirm_counts_only_chosen_repos() {
        let repo = |name: &str, pr: Option<u64>| super::Repo {
            name: name.into(),
            base: "origin/main".into(),
            branch: "feat".into(),
            local: Ok(Local::default()),
            pr: Some(Ok(pr.map(|n| super::open_pr(name, n)))),
            parent: None,
        };
        let repos = [
            repo("api", Some(1)),
            repo("web", Some(2)),
            repo("docs", None),
        ];
        assert_eq!(super::open_prs_in(&repos, &[]), 2, "empty is all");
        assert_eq!(super::open_prs_in(&repos, &["web".into()]), 1);
        assert_eq!(super::open_prs_in(&repos, &["docs".into()]), 0);
    }

    #[test]
    fn what_needs_you() {
        let a = |kind, attention, reviews: &[&str]| Action {
            label: String::new(),
            kind,
            repos: vec![],
            attention,
            reviews: reviews.iter().map(|s| s.to_string()).collect(),
        };
        assert!(needs_you(&a(Kind::Merge, false, &[])));
        assert!(needs_you(&a(Kind::Agent("fix".into()), true, &[])));
        assert!(needs_you(&a(Kind::Agent("review".into()), false, &["r1"])));
        assert!(!needs_you(&a(Kind::Agent("commit".into()), false, &[])));
        assert!(!needs_you(&a(Kind::OpenPr, false, &[])), "a wait");
    }

    #[test]
    fn unasked_or_failed_gh_keeps_the_last_pr() {
        let last = Some(Ok(None));
        assert_eq!(fresh_or_last(None, &last), last, "agent steps don't ask gh");
        let err = Err("offline".to_string());
        assert_eq!(fresh_or_last(Some(&err), &last), last, "a blip");
        assert_eq!(fresh_or_last(Some(&err), &None), Some(err.clone()));
        let pr: Result<Option<Pr>, String> = Ok(None);
        assert_eq!(fresh_or_last(Some(&pr), &Some(err)), Some(pr));
    }

    /// Real worktrees on different branches: each is probed on its own branch, and the
    /// steps and prompts follow it.
    #[test]
    fn probes_each_repo_on_its_branch() {
        use crate::git::{add_repo, create_workspace, git};
        let root = std::env::temp_dir().join(format!("shark-sync-prb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let c = ["-c", "user.name=t", "-c", "user.email=t@t"];
        let mut repos = vec![];
        for name in ["api", "web"] {
            let (origin, src) = (
                root.join(format!("{name}.git")),
                root.join("src").join(name),
            );
            std::fs::create_dir_all(&src).unwrap();
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            git(&src, &["init", "-q", "-b", "main"]).unwrap();
            git(
                &src,
                &[&c[..], &["commit", "-q", "--allow-empty", "-m", "i"]].concat(),
            )
            .unwrap();
            git(
                &src,
                &["remote", "add", "origin", &origin.to_string_lossy()],
            )
            .unwrap();
            git(&src, &["push", "-q", "origin", "main", "main:kd/x"]).unwrap();
            repos.push((add_repo(&src, &[]).unwrap(), "feat".to_string()));
        }
        repos[0].1 = "kd/x".into();
        let dir = root.join("ws/feat");
        let ws = create_workspace(&dir, "feat", "feat", &repos).unwrap();
        let job = Job {
            repos: (ws.repos.iter())
                .map(|r| (dir.join(&r.name), r.base.clone(), ws.branch_of(r).into()))
                .collect(),
            fetch: false,
            pr: false,
            prev: vec![],
            stack: vec![],
        };
        let probed = |job: &Job| {
            let p = run_job(job);
            repos_from(&ws, p.locals, p.prs, p.parents, &[])
        };
        let rs = probed(&job);
        let on: Vec<_> = rs
            .iter()
            .map(|r| r.local.as_ref().unwrap().branch.clone())
            .collect();
        assert_eq!(on, [Some("kd/x".into()), Some("feat".into())]);
        assert_eq!(
            (rs[0].branch.as_str(), rs[1].branch.as_str()),
            ("kd/x", "feat")
        );
        let a = super::next_action(&rs, &[]);
        assert!(!a.label.starts_with("Switch"), "{}", a.label);

        // Push and pull go to the repo's own branch.
        let api = &dir.join("api");
        git(
            api,
            &[&c[..], &["commit", "-q", "--allow-empty", "-m", "w"]].concat(),
        )
        .unwrap();
        let rs = probed(&job);
        let push = status::step_args(&Kind::Push, &rs[0]).unwrap().unwrap();
        assert_eq!(push, ["push", "-u", "origin", "kd/x"]);
        assert!(status::step_args(&Kind::Push, &rs[1]).is_ok());

        // web wanders off its branch: only web is asked back, to `feat`.
        git(&dir.join("web"), &["switch", "-q", "-c", "other"]).unwrap();
        let rs = probed(&job);
        let a = super::next_action(&rs, &[]);
        assert_eq!(a.label, "Switch to branch · web");
        let Kind::Agent(prompt) = a.kind else {
            panic!()
        };
        assert!(
            prompt.contains("`feat`") && !prompt.contains("kd/x"),
            "{prompt}"
        );
        assert!(
            status::step_args(&Kind::Push, &rs[1])
                .unwrap_err()
                .contains("not on `feat`")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn stacked_probe() -> (crate::state::Workspace, super::Probed) {
        let ws = crate::state::Workspace {
            name: "stack".into(),
            branch: "child".into(),
            repos: vec![crate::state::WsRepo {
                name: "api".into(),
                src: "/unused".into(),
                base: "origin/parent".into(),
                branch: None,
            }],
            ..Default::default()
        };
        let mut own = super::open_pr("api", 2);
        own.base = "parent".into();
        let p = super::Probed {
            repos: super::worktrees(&ws),
            locals: vec![Ok(Local {
                branch: Some("child".into()),
                head: "h".into(),
                remote: Some((0, 0)),
                origin: Some("git@github.com:o/api.git".into()),
                base: (1, 0),
                trunk: "origin/main".into(),
                ..Default::default()
            })],
            prs: Some(vec![Ok(Some(own))]),
            parents: vec![Some(Ok(Some(super::open_pr("api", 1))))],
            stack_prs: vec![],
            fetched: false,
            fetch_error: None,
        };
        (ws, p)
    }

    #[test]
    fn execution_keeps_parent_merge_guards_and_fails_closed() {
        let action = Action {
            label: "Merge".into(),
            kind: Kind::Merge,
            repos: vec![],
            attention: false,
            reviews: vec![],
        };
        let (ws, p) = stacked_probe();
        let repos = super::execution_repos(&ws, p, &[], &action).unwrap();
        assert!(
            status::step_args(&Kind::Merge, &repos[0])
                .unwrap_err()
                .contains("stacked on #1")
        );
        assert_eq!(
            super::next_action(&repos, &[]).label,
            "Waiting on parent PR #1"
        );

        let (_, mut p) = stacked_probe();
        p.parents = vec![Some(Err("offline".into()))];
        assert!(
            super::execution_repos(&ws, p, &repos, &action)
                .unwrap_err()
                .contains("offline")
        );

        let (_, mut p) = stacked_probe();
        p.prs = Some(vec![Err("PR lookup failed".into())]);
        assert!(
            super::execution_repos(&ws, p, &repos, &action)
                .unwrap_err()
                .contains("PR lookup failed")
        );

        // A successful answer that the parent has no PR clears the cached parent.
        let (_, mut p) = stacked_probe();
        p.parents = vec![Some(Ok(None))];
        let repos = super::execution_repos(&ws, p, &repos, &action).unwrap();
        assert_eq!(repos[0].parent, None);
        assert!(
            status::step_args(&Kind::Merge, &repos[0])
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn parent_display_cache_is_scoped_to_its_base() {
        let (mut ws, p) = stacked_probe();
        let repos = repos_from(&ws, p.locals, p.prs, p.parents, &[]);
        let (_, mut p) = stacked_probe();
        p.parents = vec![Some(Err("offline".into()))];
        let retained = repos_from(&ws, p.locals, p.prs, p.parents, &repos);
        assert_eq!(retained[0].parent, repos[0].parent);
        ws.repos[0].base = "origin/other".into();
        let (_, p) = stacked_probe();
        let changed = repos_from(&ws, p.locals, p.prs, vec![None], &repos);
        assert_eq!(changed[0].parent, None);
    }

    #[cfg(feature = "test-support")]
    #[gpui_kit::test]
    fn poll_discards_results_after_a_base_change(cx: &mut gpui_kit::TestAppContext) {
        use super::*;
        let _lock = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cx.update(gpui_kit::init);
        let (ws, p) = stacked_probe();
        let (window, sync) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                    cx.new(|cx| GitSync::new(ws.clone(), w, cx))
                })
            })
            .unwrap();
        cx.update_window(window, |_, _, cx| {
            sync.update(cx, |s, cx| {
                let mut changed = ws.clone();
                changed.repos[0].base = "origin/new-parent".into();
                s.set_workspace(changed, cx);
                s.apply(p, cx);
                assert_eq!(s.repos[0].parent, None);
                assert!(s.repos[0].local.as_ref().unwrap().head.is_empty());
                assert!(s.want_pr && s.want_fetch);
            });
        })
        .unwrap();
    }

    #[test]
    fn failed_gh_keeps_the_local_state() {
        let ws = crate::state::Workspace {
            name: "ws".into(),
            repos: vec![crate::state::WsRepo {
                name: "api".into(),
                src: "/x".into(),
                base: "origin/main".into(),
                branch: None,
            }],
            ..Default::default()
        };
        let local = || {
            vec![Ok(Local {
                changed: 3,
                ..Default::default()
            })]
        };
        let err = Err("logged out".to_string());
        let (label, repos) =
            background_answer(&ws, local(), Some(vec![err.clone()]), vec![], &[], &[]);
        assert!(label.is_err(), "the caller keeps the last label");
        assert_eq!(repos[0].local.as_ref().map(|l| l.changed), Ok(3));
        assert_eq!(repos[0].pr, Some(err));
        let (label, repos) = background_answer(&ws, local(), None, vec![], &repos, &[]);
        assert!(label.is_err(), "gh not asked");
        assert!(matches!(repos[0].pr, Some(Err(_))), "the last PR stays");
    }
}
