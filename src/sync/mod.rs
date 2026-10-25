//! The header's git button: keeps every repo of the open workspace in sync with git and GitHub,
//! and runs the next step toward a merged PR. `status` decides what that step is; this file
//! polls, renders and executes. The app only embeds the entity and forwards three actions.
pub mod background;
mod status;
pub use status::Kind;

use crate::app::{inter_features, open_menu};
use crate::chat::Chat;
use crate::git::cmd_bounded;
use crate::pr;
use crate::state::Workspace;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::command::CommandItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, Sizable as _, WindowExt, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use status::{
    Action, Local, Pr, Repo, ago, diverged, fetch_pr, next_action, pr_head_moved, pr_settling,
    probe,
};
use std::path::PathBuf;
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
}

pub struct GitSync {
    ws: Workspace,
    chat: Entity<Chat>,
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
    kick: smol::channel::Sender<()>,
    kicked: smol::channel::Receiver<()>,
    /// Polling runs only while the workspace is shown; dropping the task stops it.
    poll: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SyncEvent> for GitSync {}

/// Everything a background probe needs, detached from the entity.
#[derive(Clone)]
struct Job {
    branch: String,
    repos: Vec<(PathBuf, String)>,
    fetch: bool,
    pr: bool,
    /// The last probe per repo, so unchanged refs skip the costly comparisons.
    prev: Vec<Local>,
}

struct Probed {
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
    fetched: bool,
    fetch_error: Option<String>,
}

/// Repos run in parallel, so one hung remote costs one timeout, not one per repo.
fn run_job(job: &Job) -> Probed {
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = job
            .repos
            .iter()
            .enumerate()
            .map(|(i, (wt, base))| {
                s.spawn(move || {
                    // Errors (offline, no origin) show in the tooltip and menu, not a toast every 3 min.
                    let fetch_err = job
                        .fetch
                        .then(|| {
                            cmd_bounded(wt, "git", &["fetch", "--prune", "--quiet", "origin"], 60)
                                .err()
                        })
                        .flatten();
                    let local = probe(wt, &job.branch, base, job.prev.get(i));
                    let head = local.as_ref().map(|l| l.head.as_str()).unwrap_or_default();
                    let pr = job.pr.then(|| fetch_pr(wt, &job.branch, head));
                    (local, pr, fetch_err)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut locals = vec![];
    let mut prs = vec![];
    let mut errs = vec![];
    for ((local, pr, err), (wt, _)) in results.into_iter().zip(&job.repos) {
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

fn due(at: Option<Instant>, every: Duration) -> bool {
    at.is_none_or(|t| t.elapsed() >= every)
}

impl GitSync {
    pub fn new(
        ws: Workspace,
        chat: Entity<Chat>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (kick, kicked) = smol::channel::unbounded();
        let repos = ws.repos.iter().map(Repo::fresh).collect();
        let _subs = vec![
            cx.observe(&chat, |_, _, cx| cx.notify()), // working toggles enable/disable the button
            cx.subscribe(&chat, |s, _, ev, cx| {
                if let crate::chat::ChatEvent::TurnDone = ev {
                    s.kick(false, true, cx); // the agent may have committed, pushed or opened a PR
                }
            }),
            cx.observe_window_activation(window, |s, window, cx| {
                if window.is_window_active() {
                    s.kick(due(s.fetched, FETCH_ON_FOCUS), true, cx);
                }
            }),
        ];
        GitSync {
            reviewed: crate::state::reviewed(&ws.name),
            merge_order: pr::merge_order(&ws, &crate::state::merge_order(&ws.name)),
            ws,
            chat,
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

    /// A repo joined the workspace: track it too (known repos keep their state) and re-probe.
    pub fn set_workspace(&mut self, ws: Workspace, cx: &mut Context<Self>) {
        let mut old = std::mem::take(&mut self.repos);
        self.repos = ws
            .repos
            .iter()
            .map(|r| match old.iter().position(|o| o.name == r.name) {
                Some(i) => old.swap_remove(i),
                None => Repo::fresh(r),
            })
            .collect();
        self.merge_order = pr::merge_order(&ws, &self.merge_order);
        self.ws = ws;
        self.kick(true, true, cx);
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
                let tick = cx.background_executor().timer(LOCAL_EVERY);
                smol::future::or(tick, async {
                    let _ = kicked.recv().await;
                })
                .await;
                while kicked.try_recv().is_ok() {}
            }
        }));
    }

    fn kick(&mut self, fetch: bool, pr: bool, _: &mut Context<Self>) {
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
            branch: self.ws.branch.clone(),
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
                match pr {
                    // Keep the last good answer through a blip instead of flickering to "unknown".
                    Err(e) if matches!(r.pr, Some(Ok(_))) => self.gh_error = Some(e),
                    Err(e) => {
                        self.gh_error = Some(e.clone());
                        r.pr = Some(Err(e));
                    }
                    ok => r.pr = Some(ok),
                }
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
        let action = next_action(&self.repos, &self.ws.branch, &self.reviewed);
        if self.action.as_ref() != Some(&action) {
            self.action = Some(action);
            cx.notify();
        }
    }

    fn busy(&self, cx: &App) -> bool {
        self.chat.read(cx).working()
    }

    /// `(repo, branch on GitHub)` per repo; the URL is `None` until it is pushed to GitHub.
    pub fn branch_urls(&self) -> Vec<(String, Option<String>)> {
        self.repos
            .iter()
            .map(|r| (r.name.clone(), r.branch_url(&self.ws.branch)))
            .collect()
    }

    pub fn pr_urls(&self) -> Vec<String> {
        self.repos
            .iter()
            .filter_map(|r| Some(r.pr()?.url.clone()))
            .collect()
    }

    pub fn open_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let urls = self.pr_urls();
        if urls.is_empty() {
            return notify_err(window, cx, "No PRs yet: create them with space p r");
        }
        urls.iter().for_each(|u| cx.open_url(u));
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
        let a = Action {
            label: label.into(),
            kind,
            repos: vec![],
            attention: false,
            reviews: vec![],
        };
        self.run(a, false, window, cx)
    }

    pub fn fetch_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kick(true, true, cx);
        window.push_notification("Fetching…", cx);
    }

    /// `check`: the step came from the button, so re-probe first and refuse if the state moved
    /// on in the meantime (the snapshot can be seconds old, and the agent may have acted).
    fn run(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = &self.running {
            return notify_err(window, cx, format!("Still running: {r}"));
        }
        if a.kind.needs_idle() && self.busy(cx) {
            return notify_err(
                window,
                cx,
                "Agent is running; wait for it or stop it (ctrl-c)",
            );
        }
        match &a.kind {
            Kind::None if a.attention => {
                let detail: Vec<String> = self
                    .repos
                    .iter()
                    .filter(|r| a.repos.contains(&r.name))
                    .map(Repo::summary)
                    .collect();
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
                format!(
                    "Merge {} PRs?",
                    self.repos
                        .iter()
                        .filter(|r| r.open_pr().is_some())
                        .count()
                        .max(1)
                ),
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

    fn composer(&self, cx: &App) -> String {
        self.chat.read(cx).composer.read(cx).value().to_string()
    }

    fn exec(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.running = Some(a.label.clone());
        self.chat.update(cx, |c, cx| {
            c.hold = true;
            cx.notify(); // the composer footer says Enter queues now
        });
        cx.notify();
        let ws = self.ws.clone();
        let snapshot: Vec<_> = self.repos.iter().map(|r| r.pr.clone()).collect();
        let handled = self.reviewed.clone();
        let all = Job {
            branch: ws.branch.clone(),
            repos: worktrees(&self.ws),
            fetch: false,
            pr: true,
            prev: vec![],
        };
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
                        let now = next_action(&repos, &ws.branch, &handled);
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
                s.chat.update(cx, |c, cx| {
                    c.hold = false;
                    cx.notify();
                });
                match result {
                    Ok((a, Outcome::Ask(prompt))) => {
                        if !s.chat.update(cx, |c, cx| c.ask(&prompt, window, cx)) {
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
                        // The composer text became the PR title; clear it unless it was edited since.
                        if creates && !composer.trim().is_empty() {
                            s.chat.update(cx, |c, cx| {
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
                s.chat.update(cx, |c, cx| c.drain(cx));
                s.kick(true, true, cx); // the step may have moved the branch on GitHub
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            items,
            sel,
            move |s, row, window, cx| {
                let name = s.merge_order[row].clone();
                s.merge_order = pr::move_in_order(s.merge_order.clone(), &name, earlier);
                crate::state::set_merge_order(&s.ws.name, &s.merge_order);
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

    /// The button's tooltip: the step, each repo's state, how fresh it is, and what failed.
    fn tooltip(&self, cx: &App) -> String {
        let mut lines = vec!["Next git step (space g g)".to_string()];
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

    pub fn open_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut rows: Vec<(String, IconName, Row)> = vec![];
        if let Some(a) = self.action.clone().filter(|a| a.kind != Kind::None) {
            rows.push((format!("Next: {}", a.label), icon(&a), Row::Next));
        }
        let has_pr = self.repos.iter().any(|r| r.open_pr().is_some());
        rows.extend([
            ("Fetch now".into(), IconName::RefreshCw, Row::Fetch),
            (
                "Pull (fast-forward)".into(),
                IconName::ArrowDown,
                Row::Run(Kind::Pull),
            ),
            ("Push".into(), IconName::ArrowUp, Row::Run(Kind::Push)),
            (
                "Create / update linked PRs".into(),
                IconName::GitPullRequestCreate,
                Row::Run(Kind::CreatePr),
            ),
            (
                "Create draft PRs".into(),
                IconName::GitPullRequestDraft,
                Row::Run(Kind::CreateDraftPr),
            ),
        ]);
        let bases = self
            .ws
            .repos
            .iter()
            .map(|r| format!("{}: `{}`", r.name, r.base))
            .collect::<Vec<_>>()
            .join(", ");
        let base_prompt = crate::prompts::render("update_base", &bases, &self.ws.branch);
        rows.push((
            "Update from base (agent)".into(),
            IconName::GitMerge,
            Row::Run(Kind::Agent(base_prompt)),
        ));
        if has_pr {
            rows.push((
                "Merge PRs…".into(),
                IconName::GitMerge,
                Row::Run(Kind::Merge),
            ));
            rows.push((
                "Open PRs in browser".into(),
                IconName::ExternalLink,
                Row::Run(Kind::OpenPr),
            ));
        }
        if self.repos.len() > 1 {
            rows.push((
                format!("Merge order: {}…", self.merge_order.join(", ")),
                IconName::ListOrdered,
                Row::Order,
            ));
        }
        rows.push((
            format!(
                "Delete remote branches after merge: {}",
                if crate::state::delete_remote_branches() {
                    "on"
                } else {
                    "off"
                }
            ),
            IconName::GitBranch,
            Row::DeleteRemote,
        ));
        // Per-repo status lines; picking one opens its PR.
        for r in &self.repos {
            rows.push((
                r.summary(),
                IconName::FolderGit2,
                Row::Open(r.pr().map(|p| p.url.clone())),
            ));
        }
        if let Some(e) = &self.fetch_error {
            rows.push((
                format!("Fetch failed: {}", e.replace('\n', "; ")),
                IconName::TriangleAlert,
                Row::Open(None),
            ));
        }
        if let Some(e) = &self.gh_error {
            rows.push((
                format!("GitHub: {e}"),
                IconName::TriangleAlert,
                Row::Open(None),
            ));
        }
        let items = rows
            .iter()
            .map(|(l, i, _)| CommandItem::new().label(l.clone()).icon(*i))
            .collect();
        open_menu(
            cx.entity().downgrade(),
            items,
            0,
            move |s, row, window, cx| match &rows[row] {
                (_, _, Row::Next) => s.primary(window, cx),
                (_, _, Row::Fetch) => s.fetch_now(window, cx),
                (_, _, Row::Order) => s.move_in_merge_order(true, window, cx),
                (_, _, Row::DeleteRemote) => toggle_delete_remote(window, cx),
                (l, _, Row::Run(k)) => s.run_kind(k.clone(), l.trim_end_matches('…'), window, cx),
                (_, _, Row::Open(Some(url))) => cx.open_url(url),
                (_, _, Row::Open(None)) => {}
            },
            window,
            cx,
        );
    }
}

enum Row {
    Next,
    Fetch,
    Order,
    DeleteRemote,
    Run(Kind),
    Open(Option<String>),
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
    let b = ws.branch.as_str();
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
    let planned = |r: &Repo| status::step_args(&a.kind, b, r);
    match &a.kind {
        Kind::Agent(p) => Ok(Outcome::Ask(p.clone())),
        Kind::Pull => each(&|wt, r| {
            let Some(args) = planned(r)? else {
                return Ok(None);
            };
            cmd_bounded(wt, "git", &argv(&args), 120).map_err(|e| match diverged(&e) {
                true => format!("{e}\nThe branches diverged; use “Sync with remote”."),
                false => e,
            })?;
            Ok(Some("pulled".into()))
        }),
        Kind::Push => each(&|wt, r| {
            let Some(args) = planned(r)? else {
                return Ok(None);
            };
            cmd_bounded(wt, "git", &argv(&args), 120)
                .map_err(|e| format!("{e}\nIf the remote moved, pull first."))?;
            Ok(Some("pushed".into()))
        }),
        Kind::ForcePush => each(&|wt, r| {
            let Some(args) = planned(r)? else {
                return Ok(None);
            };
            cmd_bounded(wt, "git", &argv(&args), 120)?;
            Ok(Some("force-pushed".into()))
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
        Kind::Archive | Kind::OpenPr | Kind::None => Ok(Outcome::Done(String::new())),
    }
}

/// This repo's PR as just asked, or the last answer when gh wasn't asked or failed (a blip).
/// `(worktree, base)` per repo of `ws`.
fn worktrees(ws: &Workspace) -> Vec<(PathBuf, String)> {
    let dir = ws.dir();
    ws.repos
        .iter()
        .map(|r| (dir.join(&r.name), r.base.clone()))
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
            ..Repo::fresh(r)
        })
        .collect()
}

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

fn notify_err(window: &mut Window, cx: &mut App, e: impl Into<SharedString>) {
    window.push_notification(Notification::error(e), cx);
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
                Button::new("git-menu")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .tooltip("Git menu (space g m)")
                    .on_click(cx.listener(|s, _, window, cx| s.open_menu(window, cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Pr, fresh_or_last};

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
}
