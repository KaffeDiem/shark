//! The header's git button: keeps every repo of the open workspace in sync with git and GitHub,
//! and runs the next step toward a merged PR. `status` decides what that step is; this file
//! polls, renders and executes. The app only embeds the entity and forwards three actions.
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
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, Sizable as _, WindowExt, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use status::{Action, Local, Pr, Repo, fetch_pr, next_action, pr_head_moved, pr_settling, probe};
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
}

struct Probed {
    locals: Vec<Result<Local, String>>,
    prs: Option<Vec<Result<Option<Pr>, String>>>,
    fetched: bool,
}

/// Repos run in parallel, so one hung remote costs one timeout, not one per repo.
fn run_job(job: &Job) -> Probed {
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = job
            .repos
            .iter()
            .map(|(wt, base)| {
                s.spawn(move || {
                    if job.fetch {
                        // Errors (offline, no origin) show through gh and the menu, not a toast every 3 min.
                        let _ =
                            cmd_bounded(wt, "git", &["fetch", "--prune", "--quiet", "origin"], 60);
                    }
                    let local = probe(wt, &job.branch, base);
                    (local, job.pr.then(|| fetch_pr(wt, &job.branch)))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let (locals, prs): (Vec<_>, Vec<_>) = results.into_iter().unzip();
    Probed {
        locals,
        prs: job.pr.then(|| prs.into_iter().flatten().collect()),
        fetched: job.fetch,
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
        let repos = ws
            .repos
            .iter()
            .map(|r| Repo {
                name: r.name.clone(),
                base: r.base.clone(),
                local: Ok(Local::default()),
                pr: None,
            })
            .collect();
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
            ws,
            chat,
            repos,
            action: None,
            running: None,
            fetched: None,
            pr_checked: None,
            gh_error: None,
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
            repos: self.worktrees(&[]),
            // Fetching rewrites refs/remotes; don't fight the agent's own pull or push for the lock.
            fetch: fetch && !self.busy(cx),
            pr: self.want_pr || due(self.pr_checked, pr_every),
        };
        self.want_fetch &= !job.fetch;
        self.want_pr = false;
        job
    }

    /// `(worktree, base)` for the named repos, or all of them.
    fn worktrees(&self, names: &[String]) -> Vec<(PathBuf, String)> {
        let dir = self.ws.dir();
        self.ws
            .repos
            .iter()
            .filter(|r| names.is_empty() || names.contains(&r.name))
            .map(|r| (dir.join(&r.name), r.base.clone()))
            .collect()
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
        }
        let action = next_action(&self.repos, &self.ws.branch);
        if self.action.as_ref() != Some(&action) {
            self.action = Some(action);
            cx.notify();
        }
    }

    fn busy(&self, cx: &App) -> bool {
        self.chat.read(cx).working()
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
            _ => self.exec(a, check, window, cx),
        }
    }

    fn confirm(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body, ok) = match a.kind {
            Kind::Merge => (
                format!("Merge {} PRs?", self.pr_urls().len().max(1)),
                "Merges with each repo's allowed method, in order, and stops at the first failure. \
                 Merging can't be undone from here.",
                "Merge",
            ),
            _ => (
                "Force push?".to_string(),
                "Replaces the remote branch with your rewritten local history. Every remote commit \
                 has an equivalent local one, and the push fails if the remote moved since.",
                "Force push",
            ),
        };
        let sync = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |d, _, _| {
            let (sync, a) = (sync.clone(), a.clone());
            d.font_features(inter_features())
                .title(title.clone())
                .description(body)
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

    fn exec(&mut self, a: Action, check: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.running = Some(a.label.clone());
        self.chat.update(cx, |c, cx| {
            c.hold = true;
            cx.notify(); // the composer footer says Enter queues now
        });
        cx.notify();
        let ws = self.ws.clone();
        let snapshot: Vec<_> = self.repos.iter().map(|r| r.pr.clone()).collect();
        let all = Job {
            branch: ws.branch.clone(),
            repos: self.worktrees(&[]),
            fetch: false,
            pr: true,
        };
        let pr_kinds = !matches!(
            a.kind,
            Kind::Agent(_) | Kind::Pull | Kind::Push | Kind::ForcePush
        );
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Fresh state, never the snapshot: the decision and the act must agree.
                    let fresh = run_job(&Job {
                        pr: pr_kinds || check,
                        ..all
                    });
                    let prs = fresh.prs.unwrap_or_default();
                    let repos: Vec<Repo> = ws
                        .repos
                        .iter()
                        .zip(fresh.locals)
                        .enumerate()
                        .map(|(i, (r, local))| Repo {
                            name: r.name.clone(),
                            base: r.base.clone(),
                            local,
                            // A gh blip mid-check falls back to the last good answer.
                            pr: match (prs.get(i), &snapshot[i]) {
                                (Some(Err(_)), Some(Ok(p))) => Some(Ok(p.clone())),
                                (fresh, _) => fresh.cloned(),
                            },
                        })
                        .collect();
                    if check {
                        let now = next_action(&repos, &ws.branch);
                        if now.kind != a.kind || now.repos != a.repos {
                            return Err(format!(
                                "State changed; the next step is now “{}”",
                                now.label
                            ));
                        }
                    }
                    execute(&a, &ws, &repos).map(|msg| (a, msg))
                })
                .await;
            this.update_in(cx, |s, window, cx| {
                s.running = None;
                s.chat.update(cx, |c, cx| {
                    c.hold = false;
                    cx.notify();
                });
                match result {
                    Ok((_, Outcome::Ask(prompt))) => {
                        if !s.chat.update(cx, |c, cx| c.ask(&prompt, window, cx)) {
                            notify_err(
                                window,
                                cx,
                                "Agent is running; wait for it or stop it (ctrl-c)",
                            );
                        }
                    }
                    Ok((a, Outcome::Done(msg))) => window.push_notification(
                        Notification::success(msg).title(format!("{}: done", a.label)),
                        cx,
                    ),
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
        // Per-repo status lines; picking one opens its PR.
        for r in &self.repos {
            rows.push((
                r.summary(),
                IconName::FolderGit2,
                Row::Open(r.pr().map(|p| p.url.clone())),
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
fn execute(a: &Action, ws: &Workspace, repos: &[Repo]) -> Result<Outcome, String> {
    let b = ws.branch.as_str();
    let dir = ws.dir();
    let wanted = |r: &Repo| a.repos.is_empty() || a.repos.contains(&r.name);
    let each = |f: &Step| {
        let mut done = vec![];
        for r in repos.iter().filter(|r| wanted(r)) {
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
    let local = |r: &Repo| r.local.clone();
    // Menu steps skip the next-step check, so guard the branch they move here.
    let on_branch = |r: &Repo| {
        let l = local(r)?;
        match l.branch.as_deref() == Some(b) && l.op.is_none() {
            true => Ok(l),
            false => Err(format!("not on `{b}`, or a merge or rebase is in progress")),
        }
    };
    match &a.kind {
        Kind::Agent(p) => Ok(Outcome::Ask(p.clone())),
        Kind::Pull => each(&|wt, r| {
            let behind = on_branch(r)?.remote.is_some_and(|(_, n)| n > 0);
            if !behind {
                return Ok(None);
            }
            cmd_bounded(
                wt,
                "git",
                &["pull", "--ff-only", "--no-rebase", "origin", b],
                120,
            )
            .map_err(|e| format!("{e}\nIf the branches diverged, use “Sync with remote”."))?;
            Ok(Some("pulled".into()))
        }),
        Kind::Push => each(&|wt, r| {
            let l = on_branch(r)?;
            if l.remote.is_some_and(|(a, _)| a == 0) || (l.remote.is_none() && l.base.0 == 0) {
                return Ok(None);
            }
            cmd_bounded(wt, "git", &["push", "-u", "origin", b], 120)
                .map_err(|e| format!("{e}\nIf the remote moved, pull first."))?;
            Ok(Some("pushed".into()))
        }),
        Kind::ForcePush => each(&|wt, r| {
            if !on_branch(r)?.rewritten {
                return Ok(None);
            }
            // The commit whose patches were checked, not whatever a later fetch put there.
            let lease = format!("--force-with-lease={b}:{}", local(r)?.remote_head);
            cmd_bounded(wt, "git", &["push", &lease, "-u", "origin", b], 120)?;
            Ok(Some("force-pushed".into()))
        }),
        Kind::CreatePr => {
            let landed: Vec<String> = repos
                .iter()
                .filter(|r| r.landed())
                .map(|r| r.name.clone())
                .collect();
            let prs = pr::create_linked_prs(ws, &dir, &landed)?;
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
            let Some(p) = r.open_pr() else {
                return Ok(None);
            };
            let l = local(r)?;
            // Merge exactly what this workspace has: clean, pushed, and GitHub's head matching.
            if l.changed > 0 || l.remote != Some((0, 0)) || p.head != l.head {
                return Err(
                    "local branch, origin and the PR don't match; push or pull first".into(),
                );
            }
            let method = merge_method(wt)?;
            cmd_bounded(
                wt,
                "gh",
                &[
                    "pr",
                    "merge",
                    &p.url,
                    method,
                    "--match-head-commit",
                    &p.head,
                ],
                60,
            )
            .map_err(|e| match e.contains("timed out") {
                true => format!(
                    "{e}\nIt may still have merged; the button updates once GitHub answers."
                ),
                false => e,
            })?;
            Ok(Some(format!("merged #{}", p.number)))
        }),
        Kind::Archive | Kind::OpenPr | Kind::None => Ok(Outcome::Done(String::new())),
    }
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
                    .tooltip(match blocked {
                        true => "Waits for the agent to finish",
                        false => "Next git step (space g g)",
                    })
                    .on_click(cx.listener(|s, _, window, cx| s.primary(window, cx))),
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
