//! The header's git button: keeps every repo of the open workspace in sync with git and GitHub,
//! and runs the next step toward a merged PR. `status` decides what that step is; this file
//! polls, renders and executes. The app only embeds the entity and forwards three actions.
mod status;
pub use status::{Kind, Pr, PrState, Repo, ago, github_url};

use crate::app::menus::{each_or_all, pr_entry};
use crate::app::{inter_features, notify_err, open_menu};
use crate::menu::{self, Entry};
use crate::chat::Chat;
use crate::git::{cmd_bounded, par_map};
use crate::pr;
use crate::state::Workspace;
use gpui_kit::assets::IconName;
use gpui_kit::base::async_util::{Receiver, Sender, unbounded};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
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
    Archive,
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
}

impl Job {
    /// Every repo of `ws`, asking GitHub, not fetching.
    fn prs(ws: &Workspace) -> Job {
        Job {
            repos: worktrees(ws),
            fetch: false,
            pr: true,
            prev: vec![],
        }
    }
}

struct Probed {
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
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
        (local, pr, fetch_err)
    });
    let mut locals = vec![];
    let mut prs = vec![];
    let mut errs = vec![];
    for ((local, pr, err), (wt, ..)) in results.into_iter().zip(&job.repos) {
        locals.push(local);
        prs.extend(pr);
        if let Some(e) = err {
            let name = wt.file_name().unwrap_or_default().to_string_lossy();
            errs.push(format!("{name}: {e}"));
        }
    }
    Probed {
        locals,
        prs: job.pr.then_some(prs),
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
) -> (Result<Option<String>, String>, Vec<Repo>) {
    let p = run_job(&Job {
        pr: gh,
        ..Job::prs(ws)
    });
    background_answer(ws, p.locals, p.prs, last)
}

fn background_answer(
    ws: &Workspace,
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
    last: &[Repo],
) -> (Result<Option<String>, String>, Vec<Repo>) {
    let failed = match &prs {
        None => Some("GitHub not asked".to_string()),
        Some(prs) => prs.iter().find_map(|p| p.as_ref().err().cloned()),
    };
    let prs = (prs.into_iter().flatten().map(Some)).chain(std::iter::repeat(None));
    let mut repos = repos_from(ws, locals, prs);
    for r in &mut repos {
        let prev = (last.iter().find(|l| l.name == r.name)).and_then(|l| l.pr.clone());
        r.pr = fresh_or_last(r.pr.as_ref(), &prev);
    }
    let label = match failed {
        Some(e) => Err(e),
        None => {
            let a = next_action(&repos, &crate::state::reviewed(&ws.name));
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
    pub fn new(ws: Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (kick, kicked) = unbounded();
        let repos = ws.repos.iter().map(|r| Repo::fresh(r, &ws)).collect();
        let _subs = vec![cx.observe_window_activation(window, |s, window, _| {
            if window.is_window_active() {
                s.kick(due(s.fetched, FETCH_ON_FOCUS), true);
            }
        })];
        GitSync {
            reviewed: crate::state::reviewed(&ws.name),
            merge_order: pr::merge_order(&ws, &crate::state::merge_order(&ws.name)),
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
                Some(i) => Repo {
                    base: r.base.clone(),
                    ..old.swap_remove(i)
                },
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
        };
        self.want_fetch &= !job.fetch;
        self.want_pr = false;
        job
    }

    fn apply(&mut self, p: Probed, cx: &mut Context<Self>) {
        for (r, l) in self.repos.iter_mut().zip(p.locals) {
            r.local = l;
        }
        if let Some(prs) = p.prs {
            self.gh_error = None;
            self.gh_down = !prs.is_empty() && prs.iter().all(|p| p.is_err());
            for (r, pr) in self.repos.iter_mut().zip(prs) {
                if let Err(e) = &pr {
                    self.gh_error = Some(e.clone());
                }
                r.pr = fresh_or_last(Some(&pr), &r.pr);
            }
            self.pr_checked = Some(Instant::now());
        }
        if p.fetched {
            self.fetched = Some(Instant::now());
            if self.fetch_error != p.fetch_error {
                self.fetch_error = p.fetch_error;
                cx.notify();
            }
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
            cx.subscribe(&chat, |s, _, ev, _| {
                if let crate::chat::ChatEvent::TurnDone = ev {
                    s.kick(false, true); // the agent may have committed, pushed or opened a PR
                }
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

    /// `check`: the step came from the button, so re-probe first and refuse if the state moved
    /// on in the meantime (the snapshot can be seconds old, and the agent may have acted).
    fn run(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
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
                    match crate::state::delete_remote_branches() {
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
        let ws = self.ws.clone();
        let snapshot: Vec<_> = self.repos.iter().map(|r| r.pr.clone()).collect();
        let handled = self.reviewed.clone();
        let all = Job::prs(&ws);
        // Only steps that act on GitHub ask it again; the rest re-check git and decide with the
        // PR state already shown, so an agent prompt doesn't wait on a slow gh.
        let pr_kinds = !matches!(
            a.kind,
            Kind::Agent(_) | Kind::Pull | Kind::Push | Kind::ForcePush
        );
        let order = self.merge_order.clone();
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
                    let prs = fresh.prs.unwrap_or_default();
                    let repos = repos_from(
                        &ws,
                        fresh.locals,
                        snapshot
                            .iter()
                            .enumerate()
                            .map(|(i, last)| fresh_or_last(prs.get(i), last)),
                    );
                    if check {
                        let now = next_action(&repos, &handled);
                        if now.kind != a.kind || now.repos != a.repos {
                            return Err(format!(
                                "State changed; the next step is now “{}”",
                                now.label
                            ));
                        }
                    }
                    execute(&a, &ws, &repos, &order, &new_pr).map(|msg| (a, msg))
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.running = None;
                s.set_hold(false, cx);
                match result {
                    Ok((a, Outcome::Ask(prompt))) => {
                        let asked = (s.chat.clone())
                            .is_some_and(|c| c.update(cx, |c, cx| c.ask(&prompt, window, cx)));
                        if !asked {
                            notify_err(
                                window,
                                cx,
                                "Agent is running; wait for it or stop it (ctrl-c)",
                            );
                        } else if !a.reviews.is_empty() {
                            for id in a.reviews {
                                if !s.reviewed.contains(&id) {
                                    s.reviewed.push(id);
                                }
                            }
                            crate::state::set_reviewed(&s.ws.name, &s.reviewed);
                        }
                    }
                    Ok((a, Outcome::Done(msg))) => {
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
                // Messages queued during the step go now, unless the step's prompt went first.
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
        crate::state::set_merge_order(&self.ws.name, &self.merge_order);
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
                if a.kind.needs_idle() && self.busy(cx) {
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
        let repos = (moves.iter())
            .map(|(r, old, new)| format!("{r}: `{old}` → `{new}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let prompt = crate::prompts::render("rebase_base", &repos, &self.ws.branch);
        self.run_kind(Kind::Agent(prompt), "Rebase onto new base", window, cx)
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
                this.update(cx, |s, cx| s.run_kind_in(k.clone(), label, rs, w, cx)).ok();
            })
        };
        let rows = |f: &dyn Fn(&Repo) -> Option<String>| -> Vec<(String, String, String)> {
            (self.repos.iter())
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
                Some(menu::item(IconName::RefreshCw, "Fetch now", run(Self::fetch_now))),
                pull.map(|e| e.hint("Fast-forward")),
                kind(IconName::ArrowUp, Kind::Push, "Push", every.clone()),
                Some(
                    menu::item(
                        IconName::GitCompareArrows,
                        "Update from base",
                        run(Self::update_from_base),
                    )
                    .hint("Agent"),
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
                            this.update(cx, |s, cx| s.move_repo(&name, earlier, cx)).ok();
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
        let on = crate::state::delete_remote_branches();
        let delete = menu::item(
            IconName::GitBranch,
            "Delete remote branches after merge",
            toggle_delete_remote,
        );
        entries.push(if on { delete.checked(true) } else { delete.hint("Off") });
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

enum Outcome {
    Ask(String),
    Done(String),
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
) -> Result<Outcome, String> {
    let dir = ws.dir();
    let wanted = |r: &Repo| a.repos.is_empty() || a.repos.contains(&r.name);
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
    let delete_remote = a.kind == Kind::Merge && crate::state::delete_remote_branches();
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
            false => Ok(Outcome::Done(done.join("\n"))),
        }
    };
    let planned = |r: &Repo| status::step_args(&a.kind, r);
    match &a.kind {
        Kind::Agent(p) => Ok(Outcome::Ask(p.clone())),
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
            let prs = pr::create_linked_prs(ws, &dir, &landed, new_pr)?;
            Ok(Outcome::Done(
                prs.iter()
                    .map(|(r, u)| format!("{r}: {u}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
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
        Kind::Archive | Kind::OpenPr | Kind::None => unreachable!(),
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

/// The repos of `ws` with what was just probed for each, in order.
fn repos_from(
    ws: &Workspace,
    locals: Vec<Result<Local, String>>,
    prs: impl IntoIterator<Item = Option<Result<Option<Pr>, String>>>,
) -> Vec<Repo> {
    ws.repos
        .iter()
        .zip(locals)
        .zip(prs)
        .map(|((r, local), pr)| Repo {
            local,
            pr,
            ..Repo::fresh(r, ws)
        })
        .collect()
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

/// Flips the opt-in for deleting remote branches on merge (saved in state.json).
pub fn toggle_delete_remote(window: &mut Window, cx: &mut App) {
    let on = !crate::state::delete_remote_branches();
    crate::state::set_delete_remote_branches(on);
    let msg = match on {
        true => "Merging now deletes each PR's remote branch",
        false => "Merging now keeps the remote branches",
    };
    window.push_notification(msg, cx);
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
        h_flex()
            .gap_1()
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
                            .icon(icon)
                            .label(label)
                            .map(|b| match (passive, attention) {
                                (true, _) => b.ghost(),
                                (false, true) => b.outline(),
                                (false, false) => b.primary(),
                            })
                            .disabled(!clickable || blocked)
                            .on_click(cx.listener(|s, _, window, cx| s.primary(window, cx))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .child(
                        Button::new("git-menu")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronDown)
                            .tooltip("Git menu")
                            .on_click(cx.listener(|s, _, window, cx| s.open_menu(window, cx))),
                    )
                    .child({
                        let at = self.chevron.clone();
                        canvas(move |b, _, _| at.set(b), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                    }),
            )
            .children(self.menu.as_ref().map(|(m, _)| menu::layer(m.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Action, Job, Kind, Local, Pr, background_answer, fresh_or_last, needs_you, repos_from,
        run_job, status,
    };

    #[test]
    fn merge_confirm_counts_only_chosen_repos() {
        let repo = |name: &str, pr: Option<u64>| super::Repo {
            name: name.into(),
            base: "origin/main".into(),
            local: Ok(Local::default()),
            pr: Some(Ok(pr.map(|n| super::open_pr(name, n)))),
        };
        let repos = [repo("api", Some(1)), repo("web", Some(2)), repo("docs", None)];
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
            let (origin, src) = (root.join(format!("{name}.git")), root.join("src").join(name));
            std::fs::create_dir_all(&src).unwrap();
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            git(&src, &["init", "-q", "-b", "main"]).unwrap();
            git(&src, &[&c[..], &["commit", "-q", "--allow-empty", "-m", "i"]].concat()).unwrap();
            git(&src, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
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
        };
        let probed = |job: &Job| {
            let p = run_job(job);
            repos_from(&ws, p.locals, std::iter::repeat(None))
        };
        let rs = probed(&job);
        let on: Vec<_> = rs.iter().map(|r| r.local.as_ref().unwrap().branch.clone()).collect();
        assert_eq!(on, [Some("kd/x".into()), Some("feat".into())]);
        assert_eq!((rs[0].branch.as_str(), rs[1].branch.as_str()), ("kd/x", "feat"));
        let a = super::next_action(&rs, &[]);
        assert!(!a.label.starts_with("Switch"), "{}", a.label);

        // Push and pull go to the repo's own branch.
        let api = &dir.join("api");
        git(api, &[&c[..], &["commit", "-q", "--allow-empty", "-m", "w"]].concat()).unwrap();
        let rs = probed(&job);
        let push = status::step_args(&Kind::Push, &rs[0]).unwrap().unwrap();
        assert_eq!(push, ["push", "-u", "origin", "kd/x"]);
        assert!(status::step_args(&Kind::Push, &rs[1]).is_ok());

        // web wanders off its branch: only web is asked back, to `feat`.
        git(&dir.join("web"), &["switch", "-q", "-c", "other"]).unwrap();
        let rs = probed(&job);
        let a = super::next_action(&rs, &[]);
        assert_eq!(a.label, "Switch to branch · web");
        let Kind::Agent(prompt) = a.kind else { panic!() };
        assert!(prompt.contains("`feat`") && !prompt.contains("kd/x"), "{prompt}");
        assert!(status::step_args(&Kind::Push, &rs[1]).unwrap_err().contains("not on `feat`"));
        std::fs::remove_dir_all(&root).unwrap();
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
        let (label, repos) = background_answer(&ws, local(), Some(vec![err.clone()]), &[]);
        assert!(label.is_err(), "the caller keeps the last label");
        assert_eq!(repos[0].local.as_ref().map(|l| l.changed), Ok(3));
        assert_eq!(repos[0].pr, Some(err));
        let (label, repos) = background_answer(&ws, local(), None, &repos);
        assert!(label.is_err(), "gh not asked");
        assert!(matches!(repos[0].pr, Some(Err(_))), "the last PR stays");
    }
}
