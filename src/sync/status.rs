//! Pure git/GitHub state per repo and the step it calls for. No gpui here, so it is unit tested.
use crate::git::{cmd_bounded, git, run};
use crate::prompts;
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Local {
    /// `None` when HEAD is detached (also mid-rebase).
    pub branch: Option<String>,
    pub head: String,
    /// `origin`'s URL; `None` if the repo has no origin remote.
    pub origin: Option<String>,
    /// `(ahead, behind)` vs `origin/<branch>`; `None` if it isn't on the remote. Not `@{u}`:
    /// a fresh worktree may track its base, and a deleted remote branch keeps a stale upstream.
    pub remote: Option<(u32, u32)>,
    /// `origin/<branch>` as probed; a force push leases exactly this commit.
    pub remote_head: String,
    /// Diverged only because local history was rewritten: every remote-only commit has a
    /// patch-equivalent local one and the local tip is newer, so a lease-guarded force push
    /// loses nothing.
    pub rewritten: bool,
    /// `(ahead, behind)` vs the workspace base, e.g. `origin/main`.
    pub base: (u32, u32),
    pub changed: u32,
    pub conflicts: u32,
    pub op: Option<Op>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Checks {
    Pending,
    /// Also when the PR has no checks.
    Passing,
    Failing(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pr {
    pub url: String,
    pub number: u64,
    pub state: PrState,
    pub draft: bool,
    /// GitHub's `mergeStateStatus`: CLEAN, DIRTY, BEHIND, BLOCKED, UNSTABLE, HAS_HOOKS, DRAFT, UNKNOWN.
    pub merge_state: String,
    /// `reviewDecision`: APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or empty.
    pub review: String,
    pub checks: Checks,
    pub head: String,
    /// IDs of the reviews that requested changes.
    pub change_requests: Vec<String>,
    /// IDs of comment-only reviews (inline comments come as one) by anyone but the PR's author.
    pub comments: Vec<String>,
    /// Merged PRs only: the local HEAD, when it was found to be an ancestor of `head`. A web
    /// merge can land a newer head than ours (Update branch, a teammate's push).
    pub contains_head: String,
}

impl Pr {
    /// `head` is the PR's head, or a merged PR contains it.
    pub fn has(&self, head: &str) -> bool {
        self.head == head || (!head.is_empty() && self.contains_head == head)
    }
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub fn parse_status(out: &[u8]) -> Local {
    let mut l = Local::default();
    let mut fields = out.split(|b| *b == 0).map(String::from_utf8_lossy);
    while let Some(f) = fields.next() {
        if let Some(h) = f.strip_prefix("# branch.head ") {
            l.branch = (h != "(detached)").then(|| h.to_string());
        } else if let Some(o) = f.strip_prefix("# branch.oid ") {
            l.head = o.to_string(); // `(initial)` on an unborn branch
        } else if f.starts_with("u ") {
            l.conflicts += 1;
            l.changed += 1;
        } else if f.starts_with("2 ") {
            fields.next(); // a rename's original path is its own NUL field
            l.changed += 1;
        } else if f.starts_with("1 ") || f.starts_with("? ") {
            l.changed += 1;
        }
    }
    l
}

fn counts(s: &str) -> (u32, u32) {
    let mut it = s.split_whitespace().map(|n| n.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Probes one worktree. Read-only: `--no-optional-locks` keeps `git status` from taking
/// `index.lock`, which would make the agent's own git commands fail. `prev` is the last probe:
/// while HEAD and `origin/<branch>` haven't moved, its remote counts still hold.
pub fn probe(wt: &Path, branch: &str, base: &str, prev: Option<&Local>) -> Result<Local, String> {
    if !wt.exists() {
        return Err(format!("{} is missing", wt.display()));
    }
    let out = run(
        wt,
        "git",
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
        ],
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let mut l = parse_status(&out.stdout);
    // Per-worktree state files; --git-path resolves them inside .git/worktrees/<name>/.
    const OPS: [(&str, Op); 6] = [
        ("rebase-merge", Op::Rebase),
        ("rebase-apply", Op::Rebase),
        ("MERGE_HEAD", Op::Merge),
        ("CHERRY_PICK_HEAD", Op::CherryPick),
        ("REVERT_HEAD", Op::Revert),
        ("BISECT_LOG", Op::Bisect),
    ];
    let mut args = vec!["rev-parse", "--path-format=absolute"];
    OPS.iter().for_each(|(f, _)| args.extend(["--git-path", f]));
    let paths = git(wt, &args)?;
    l.op = paths
        .lines()
        .zip(OPS)
        .find(|(p, _)| Path::new(p).exists())
        .map(|(_, (_, op))| op);
    l.origin = git(wt, &["remote", "get-url", "origin"]).ok();
    let remote = format!("refs/remotes/origin/{branch}");
    if let Ok(oid) = git(wt, &["rev-parse", "--verify", "--quiet", &remote]) {
        if let Some(p) =
            prev.filter(|p| p.remote.is_some() && (&p.head, &p.remote_head) == (&l.head, &oid))
        {
            (l.remote, l.rewritten, l.remote_head) = (p.remote, p.rewritten, oid);
            return base_counts(wt, base, l);
        }
        l.remote_head = oid;
        let range = format!("HEAD...{remote}");
        let ab = counts(&git(wt, &["rev-list", "--left-right", "--count", &range])?);
        if ab.0 > 0 && ab.1 > 0 {
            let theirs = git(
                wt,
                &[
                    "rev-list",
                    "--count",
                    "--cherry-pick",
                    "--right-only",
                    &range,
                ],
            )?;
            // Patch equality is symmetric; only the side with the newer tip did the rewriting.
            let time = |r: &str| git(wt, &["log", "-1", "--format=%ct", r]).unwrap_or_default();
            let newer = time("HEAD").parse::<u64>().unwrap_or(0)
                > time(&remote).parse().unwrap_or(u64::MAX);
            l.rewritten = theirs == "0" && newer;
        }
        l.remote = Some(ab);
    }
    base_counts(wt, base, l)
}

fn base_counts(wt: &Path, base: &str, mut l: Local) -> Result<Local, String> {
    // Fails on an unborn branch or a base that no longer resolves; (0, 0) is the honest answer then.
    if let Ok(c) = git(
        wt,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{base}"),
        ],
    ) {
        l.base = counts(&c);
    }
    Ok(l)
}

const PR_FIELDS: &str = "url,number,state,isDraft,mergeStateStatus,reviewDecision,statusCheckRollup,headRefOid,isCrossRepository,updatedAt,reviews,author";

fn checks(rollup: &Value) -> Checks {
    let items = rollup.as_array().into_iter().flatten();
    let name = |c: &Value| {
        c["name"]
            .as_str()
            .or(c["context"].as_str())
            .unwrap_or("check")
            .to_string()
    };
    let started = |c: &Value| {
        c["startedAt"]
            .as_str()
            .or(c["createdAt"].as_str())
            .unwrap_or("")
            .to_string()
    };
    // A re-run leaves the old run in the rollup; only the newest per name counts.
    let mut latest: Vec<&Value> = vec![];
    for c in items {
        match latest.iter().position(|l| name(l) == name(c)) {
            Some(i) if started(latest[i]) <= started(c) => latest[i] = c,
            Some(_) => {}
            None => latest.push(c),
        }
    }
    let mut pending = false;
    let mut failed = vec![];
    for c in latest {
        // CheckRun has status + conclusion; StatusContext has state.
        let verdict = c["conclusion"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or(c["state"].as_str())
            .unwrap_or_default();
        match verdict {
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
            | "STARTUP_FAILURE" => failed.push(name(c)),
            "PENDING" | "EXPECTED" => pending = true,
            _ if c["status"].as_str().is_some_and(|s| s != "COMPLETED") => pending = true,
            _ => {}
        }
    }
    match (failed.is_empty(), pending) {
        (false, _) => Checks::Failing(failed),
        (true, true) => Checks::Pending,
        (true, false) => Checks::Passing,
    }
}

/// Picks this branch's PR from `gh pr list --head … --state all`: the open one, else the newest.
/// Fork PRs whose branch happens to have the same name are ignored.
pub fn parse_prs(json: &str) -> Option<Pr> {
    let list: Vec<Value> = serde_json::from_str(json).ok()?;
    let own: Vec<&Value> = list
        .iter()
        .filter(|p| p["isCrossRepository"] != true)
        .collect();
    let pick = own.iter().find(|p| p["state"] == "OPEN").or_else(|| {
        own.iter()
            .max_by_key(|p| p["updatedAt"].as_str().unwrap_or("").to_string())
    })?;
    let s = |k: &str| pick[k].as_str().unwrap_or_default().to_string();
    let ids = |keep: &dyn Fn(&Value) -> bool| -> Vec<String> {
        pick["reviews"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| keep(r))
            .filter_map(|r| Some(r["id"].as_str()?.to_string()))
            .collect()
    };
    Some(Pr {
        url: s("url"),
        number: pick["number"].as_u64().unwrap_or(0),
        state: match pick["state"].as_str() {
            Some("MERGED") => PrState::Merged,
            Some("CLOSED") => PrState::Closed,
            _ => PrState::Open,
        },
        draft: pick["isDraft"].as_bool().unwrap_or(false),
        merge_state: s("mergeStateStatus"),
        review: s("reviewDecision"),
        checks: checks(&pick["statusCheckRollup"]),
        head: s("headRefOid"),
        change_requests: ids(&|r| r["state"] == "CHANGES_REQUESTED"),
        // The author's own replies to threads are COMMENTED reviews too; they ask for nothing.
        comments: ids(&|r| {
            r["state"] == "COMMENTED" && r["author"]["login"] != pick["author"]["login"]
        }),
        contains_head: String::new(),
    })
}

/// `Ok(None)` means the branch has no PR. Unlike `gh pr view`, "none" isn't an error exit,
/// so it can't be confused with being offline or logged out.
/// `head` is the local HEAD as just probed.
pub fn fetch_pr(wt: &Path, branch: &str, head: &str) -> Result<Option<Pr>, String> {
    let json = cmd_bounded(
        wt,
        "gh",
        &[
            "pr", "list", "--head", branch, "--state", "all", "--json", PR_FIELDS,
        ],
        20,
    )?;
    let mut pr = parse_prs(&json);
    if let Some(p) = pr
        .as_mut()
        .filter(|p| p.state == PrState::Merged && !head.is_empty() && p.head != head)
    {
        // A head pushed on GitHub (Update branch) and auto-deleted right after the merge
        // may never have reached a fetch here; the PR ref keeps it.
        let have = format!("{}^{{commit}}", p.head);
        if git(wt, &["cat-file", "-e", &have]).is_err() {
            let pull = format!("refs/pull/{}/head", p.number);
            let _ = cmd_bounded(wt, "git", &["fetch", "--quiet", "origin", &pull], 20);
        }
        if git(wt, &["merge-base", "--is-ancestor", head, &p.head]).is_ok() {
            p.contains_head = head.to_string();
        }
    }
    Ok(pr)
}

/// Everything known about one repo of a workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    pub name: String,
    /// Ref the branch was cut from, e.g. `origin/main`.
    pub base: String,
    pub local: Result<Local, String>,
    /// `None` until GitHub was asked; `Some(Err)` when gh failed (offline, logged out, not GitHub).
    pub pr: Option<Result<Option<Pr>, String>>,
}

impl Repo {
    /// Known by name only: nothing probed yet.
    pub fn fresh(r: &crate::state::WsRepo) -> Repo {
        Repo {
            name: r.name.clone(),
            base: r.base.clone(),
            local: Ok(Local::default()),
            pr: None,
        }
    }

    pub fn pr(&self) -> Option<&Pr> {
        self.pr.as_ref()?.as_ref().ok()?.as_ref()
    }

    pub fn open_pr(&self) -> Option<&Pr> {
        self.pr().filter(|p| p.state == PrState::Open)
    }

    /// `branch` on GitHub, e.g. `https://github.com/o/r/tree/feat`; `None` unless origin is
    /// GitHub and the branch is pushed (an unpushed branch would 404).
    pub fn branch_url(&self, branch: &str) -> Option<String> {
        let l = self.local.as_ref().ok()?;
        l.remote?;
        Some(format!(
            "{}/tree/{branch}",
            github_url(l.origin.as_deref()?)?
        ))
    }

    /// Its work is exactly what a merged PR landed.
    pub fn landed(&self) -> bool {
        let head = self
            .local
            .as_ref()
            .map(|l| l.head.as_str())
            .unwrap_or_default();
        self.pr()
            .is_some_and(|p| p.state == PrState::Merged && p.has(head))
    }

    /// One line for the menu, e.g. `api: ↑2 · 3 changed · PR #12 checks failing`.
    pub fn summary(&self) -> String {
        let l = match &self.local {
            Err(e) => return format!("{}: {e}", self.name),
            Ok(l) => l,
        };
        let mut parts = vec![];
        if let Some(op) = l.op {
            parts.push(format!("{op:?} in progress").to_lowercase());
        }
        if l.changed > 0 {
            parts.push(format!("{} changed", l.changed));
        }
        match l.remote {
            None if l.base.0 > 0 => parts.push(format!("{} commits not pushed", l.base.0)),
            Some((a, b)) if a + b > 0 => parts.push(format!("{a} to push, {b} to pull")),
            _ => {}
        }
        if l.base.1 > 0 {
            parts.push(format!("{} behind {}", l.base.1, self.base));
        }
        match &self.pr {
            Some(Ok(Some(p))) => parts.push(format!(
                "PR #{} {}",
                p.number,
                match (&p.state, &p.checks) {
                    (PrState::Merged, _) => "merged",
                    (PrState::Closed, _) => "closed",
                    (_, Checks::Failing(_)) => "checks failing",
                    (_, Checks::Pending) => "checks running",
                    _ if p.draft => "draft",
                    _ => "open",
                }
            )),
            Some(Err(_)) => parts.push("GitHub unavailable".into()),
            _ => {}
        }
        if parts.is_empty() {
            parts.push("clean".into());
        }
        format!("{}: {}", self.name, parts.join(" · "))
    }
}

/// How a step is carried out.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Sent to the agent as a prompt.
    Agent(String),
    Pull,
    Push,
    ForcePush,
    CreatePr,
    /// `CreatePr` with new PRs opened as drafts; only from the menu or palette.
    CreateDraftPr,
    Reopen,
    UpdateBranch,
    Ready,
    Merge,
    Archive,
    OpenPr,
    /// Nothing to run; the label is a status.
    None,
}

impl Kind {
    /// Steps that change the worktree or the remote; they must not race a running agent.
    pub fn needs_idle(&self) -> bool {
        !matches!(self, Kind::OpenPr | Kind::None)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub label: String,
    pub kind: Kind,
    /// Repos the step applies to; empty means every repo.
    pub repos: Vec<String>,
    /// Something is wrong and needs the agent or a human.
    pub attention: bool,
    /// Review IDs this step sends to the agent; remembered so it isn't asked twice.
    pub reviews: Vec<String>,
}

fn act(label: impl Into<String>, kind: Kind, repos: Vec<String>, attention: bool) -> Action {
    Action {
        label: label.into(),
        kind,
        repos,
        attention,
        reviews: vec![],
    }
}

/// The repos named in `rs`, in workspace order.
pub fn among<'a>(repos: &'a [Repo], rs: &'a [String]) -> impl Iterator<Item = &'a Repo> {
    repos.iter().filter(|r| rs.contains(&r.name))
}

/// The single most useful next step for the whole workspace. Local problems come before
/// remote ones, and within the PR phase actionable steps beat passive waits, so a
/// "checks running" in one repo can't hide a "ready for review" in another. `handled`: review
/// IDs already sent to the agent.
pub fn next_action(repos: &[Repo], branch: &str, handled: &[String]) -> Action {
    let names = |f: &dyn Fn(&Repo, &Local) -> bool| -> Vec<String> {
        repos
            .iter()
            .filter(|r| r.local.as_ref().is_ok_and(|l| f(r, l)))
            .map(|r| r.name.clone())
            .collect()
    };
    let label = |verb: &str, rs: &[String]| match repos.len() {
        1 => verb.to_string(),
        _ => format!("{verb} · {}", rs.join(", ")),
    };
    let with_base = |rs: &[String]| {
        among(repos, rs)
            .map(|r| format!("{} (base `{}`)", r.name, r.base))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // The step for `rs`, unless that is empty.
    let step = |verb: &str, kind: Kind, rs: Vec<String>, attention| {
        (!rs.is_empty()).then(|| act(label(verb, &rs), kind, rs, attention))
    };
    // An agent step whose prompt `key` names the repos.
    let ask = |verb: &str, key: &str, rs: Vec<String>, attention| {
        let prompt = prompts::render(key, &rs.join(", "), branch);
        step(verb, Kind::Agent(prompt), rs, attention)
    };
    let with_reviews =
        |verb: &str, key: &str, rs: Vec<String>, ids: &dyn Fn(&Pr) -> Vec<String>| {
            let reviews = among(repos, &rs)
                .filter_map(Repo::open_pr)
                .flat_map(ids)
                .collect();
            Some(Action {
                reviews,
                ..ask(verb, key, rs, false)?
            })
        };

    let broken: Vec<String> = repos
        .iter()
        .filter(|r| r.local.is_err())
        .map(|r| r.name.clone())
        .collect();
    // Without a GitHub answer "no PR" is a guess; those repos sit out the PR steps, so they
    // can't get a duplicate PR, nor block the other repos' steps.
    let unknown = names(&|r, l| l.base.0 > 0 && !matches!(r.pr, Some(Ok(_))));
    let known = |r: &Repo| !unknown.contains(&r.name);
    // A merged PR holding HEAD means "landed"; any newer commit needs a new PR.
    let at_head =
        |r: &Repo, l: &Local, s: PrState| r.pr().is_some_and(|p| p.state == s && p.has(&l.head));
    let unopened = |r: &Repo, l: &Local| known(r) && l.base.0 > 0 && r.open_pr().is_none();
    let prs = |f: &dyn Fn(&Pr, &Local) -> bool| names(&|r, l| r.open_pr().is_some_and(|p| f(p, l)));
    // GitHub lags a push by a few seconds; until a PR shows our head, its verdicts are about
    // the old one. Such a PR waits on its own instead of holding up the others.
    let current = |f: &dyn Fn(&Pr) -> bool| prs(&|p, l| p.head == l.head && f(p));
    // GitHub keeps CHANGES_REQUESTED until the reviewer looks again, so only reviews the agent
    // hasn't seen ask for work. Without IDs there's nothing to remember; ask every time.
    let unhandled = |p: &Pr| {
        p.change_requests.is_empty() || p.change_requests.iter().any(|id| !handled.contains(id))
    };
    let new_comments = |p: &Pr| p.comments.iter().any(|id| !handled.contains(id));

    let actionable = step("Git error", Kind::None, broken, true)
        .or_else(|| {
            let rs = names(&|_, l| l.op == Some(Op::Bisect));
            step("Bisect in progress", Kind::None, rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.conflicts > 0);
            ask("Resolve conflicts", "conflicts", rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.op.is_some());
            ask("Finish operation", "finish", rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.branch.is_none());
            ask("Fix detached HEAD", "detached", rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.branch.as_deref() != Some(branch));
            ask("Switch to branch", "wrong_branch", rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.changed > 0);
            ask("Commit and push", "commit", rs, false)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.origin.is_none() && l.base.0 > 0);
            step("No origin remote", Kind::None, rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.rewritten);
            step("Force push", Kind::ForcePush, rs, false)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.remote.is_some_and(|(a, b)| a > 0 && b > 0));
            ask("Sync with remote", "diverged", rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.remote.is_some_and(|(_, b)| b > 0));
            step("Pull", Kind::Pull, rs, false)
        })
        .or_else(|| {
            let rs = names(&|r, l| unopened(r, l) && at_head(r, l, PrState::Closed));
            step("Reopen PR", Kind::Reopen, rs, false)
        })
        .or_else(|| {
            let rs = names(&|r, l| unopened(r, l) && !at_head(r, l, PrState::Merged));
            step("Create PR", Kind::CreatePr, rs, false)
        })
        .or_else(|| {
            let rs = names(&|r, l| r.open_pr().is_some() && l.remote.is_none_or(|(a, _)| a > 0));
            step("Push", Kind::Push, rs, false)
        })
        .or_else(|| {
            let rs = current(&|p| p.merge_state == "DIRTY");
            let prompt = prompts::render("pr_conflicts", &with_base(&rs), branch);
            step("Resolve PR conflicts", Kind::Agent(prompt), rs, true)
        })
        .or_else(|| {
            let rs = current(&|p| matches!(p.checks, Checks::Failing(_)));
            let failing: Vec<String> = among(repos, &rs)
                .filter_map(|r| match &r.open_pr()?.checks {
                    Checks::Failing(n) => Some(format!("{}: {}", r.name, n.join(", "))),
                    _ => None,
                })
                .collect();
            let prompt = prompts::render("checks", &failing.join("; "), branch);
            step("Fix checks", Kind::Agent(prompt), rs, true)
        })
        .or_else(|| {
            let rs = current(&|p| p.review == "CHANGES_REQUESTED" && unhandled(p));
            // Its prompt reads every comment, so the comment-only reviews are handled too.
            with_reviews("Address review", "review", rs, &|p| {
                [p.change_requests.clone(), p.comments.clone()].concat()
            })
        })
        .or_else(|| {
            let rs = current(&new_comments);
            with_reviews("Address comments", "comments", rs, &|p| p.comments.clone())
        })
        .or_else(|| {
            let rs = current(&|p| p.merge_state == "BEHIND");
            step("Update branch", Kind::UpdateBranch, rs, false)
        })
        .or_else(|| step("Ready for review", Kind::Ready, prs(&|p, _| p.draft), false));
    if let Some(a) = actionable {
        return a;
    }
    if !unknown.is_empty() {
        let failed = among(repos, &unknown).any(|r| matches!(r.pr, Some(Err(_))));
        return match failed {
            true => act(
                label("GitHub unavailable", &unknown),
                Kind::None,
                unknown,
                true,
            ),
            false => act("Checking PR…", Kind::None, vec![], false),
        };
    }
    let passive = |l: &str| act(l, Kind::OpenPr, vec![], false);
    // GitHub computes mergeability lazily, and lags a push by a few seconds.
    if !prs(&|p, l| p.head != l.head || matches!(p.merge_state.as_str(), "UNKNOWN" | "")).is_empty()
    {
        return passive("Checking PR…");
    }
    if !prs(&|p, _| p.checks == Checks::Pending).is_empty() {
        return passive("Checks running");
    }
    if !prs(&|p, _| p.review == "CHANGES_REQUESTED").is_empty() {
        return passive("Awaiting re-review");
    }
    if !prs(&|p, _| p.merge_state == "BLOCKED").is_empty() {
        let review =
            !prs(&|p, _| p.merge_state == "BLOCKED" && p.review == "REVIEW_REQUIRED").is_empty();
        return passive(if review {
            "Awaiting review"
        } else {
            "Merge blocked"
        });
    }
    if let Some(a) = step("Merge", Kind::Merge, prs(&|_, _| true), false) {
        return a;
    }
    if repos
        .iter()
        .any(|r| r.pr().is_some_and(|p| p.state == PrState::Merged))
    {
        return act("Merged · archive", Kind::Archive, vec![], false);
    }
    act("No changes", Kind::None, vec![], false)
}

/// Whether to ask GitHub again soon: its answer is about to change.
pub fn pr_settling(repos: &[Repo]) -> bool {
    repos.iter().any(|r| {
        r.open_pr().is_some_and(|p| {
            p.checks == Checks::Pending
                || matches!(p.merge_state.as_str(), "UNKNOWN" | "")
                || head_moved(r, p)
        })
    })
}

/// Pushed, yet the PR shows another head: GitHub lags, or someone else pushed (then fetch).
fn head_moved(r: &Repo, p: &Pr) -> bool {
    r.local
        .as_ref()
        .is_ok_and(|l| l.remote == Some((0, 0)) && l.head != p.head)
}

pub fn pr_head_moved(repos: &[Repo]) -> bool {
    repos
        .iter()
        .any(|r| r.open_pr().is_some_and(|p| head_moved(r, p)))
}

/// The command one repo's part of a mechanical step runs: `git` args for Pull, Push and
/// ForcePush, `gh` args for Merge (minus the merge method). `Ok(None)` when the repo has nothing
/// to do. Menu steps skip the next-step check, so these guards are what keeps them from moving
/// the wrong commit.
pub fn step_args(kind: &Kind, b: &str, r: &Repo) -> Result<Option<Vec<String>>, String> {
    let local = || r.local.clone();
    let on_branch = || {
        let l = local()?;
        match l.branch.as_deref() == Some(b) && l.op.is_none() {
            true => Ok(l),
            false => Err(format!("not on `{b}`, or a merge or rebase is in progress")),
        }
    };
    let args = |a: &[&str]| Ok(Some(a.iter().map(|s| s.to_string()).collect()));
    match kind {
        Kind::Pull => match on_branch()?.remote.is_some_and(|(_, n)| n > 0) {
            true => args(&["pull", "--ff-only", "--no-rebase", "origin", b]),
            false => Ok(None),
        },
        Kind::Push => {
            let l = on_branch()?;
            if l.remote.is_some_and(|(a, _)| a == 0) || (l.remote.is_none() && l.base.0 == 0) {
                return Ok(None);
            }
            args(&["push", "-u", "origin", b])
        }
        Kind::ForcePush => {
            let l = on_branch()?;
            if !l.rewritten {
                return Ok(None);
            }
            // The commit whose patches were checked, not whatever a later fetch put there.
            let lease = format!("--force-with-lease={b}:{}", l.remote_head);
            args(&["push", &lease, "-u", "origin", b])
        }
        Kind::Merge => {
            let Some(p) = r.open_pr() else {
                return Ok(None);
            };
            let l = local()?;
            // Merge exactly what this workspace has: clean, pushed, and GitHub's head matching.
            if l.changed > 0 || l.remote != Some((0, 0)) || p.head != l.head {
                return Err(
                    "local branch, origin and the PR don't match; push or pull first".into(),
                );
            }
            args(&["pr", "merge", &p.url, "--match-head-commit", &p.head])
        }
        _ => Ok(None),
    }
}

/// The web URL of a GitHub remote, from its https, scp-style or ssh form.
pub fn github_url(remote: &str) -> Option<String> {
    let path = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ]
    .iter()
    .find_map(|p| remote.strip_prefix(p))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    Some(format!("https://github.com/{path}"))
}

/// `8s ago`, `2m ago`, `3h ago`, `2d ago`.
pub fn ago(d: std::time::Duration) -> String {
    match d.as_secs() {
        s @ 0..60 => format!("{s}s ago"),
        s @ 60..3600 => format!("{}m ago", s / 60),
        s @ 3600..86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

/// A failed `pull --ff-only` because local and remote both have new commits.
pub fn diverged(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("not possible to fast-forward") || e.contains("diverg")
}

#[cfg(test)]
mod tests {
    #[test]
    fn ago_and_diverged() {
        use std::time::Duration;
        assert_eq!(super::ago(Duration::from_secs(8)), "8s ago");
        assert_eq!(super::ago(Duration::from_secs(150)), "2m ago");
        assert_eq!(super::ago(Duration::from_secs(7300)), "2h ago");
        assert_eq!(super::ago(Duration::from_secs(200_000)), "2d ago");
        assert!(super::diverged(
            "git pull: fatal: Not possible to fast-forward, aborting."
        ));
        assert!(super::diverged("hint: You have divergent branches"));
        assert!(!super::diverged(
            "git pull: fatal: unable to access 'https://x': timeout"
        ));
    }

    #[test]
    fn github_urls() {
        for r in [
            "https://github.com/o/r.git",
            "https://github.com/o/r",
            "git@github.com:o/r.git",
            "ssh://git@github.com/o/r.git",
        ] {
            assert_eq!(
                super::github_url(r).as_deref(),
                Some("https://github.com/o/r"),
                "{r}"
            );
        }
        assert_eq!(super::github_url("git@gitlab.com:o/r.git"), None);
    }

    use super::*;

    #[test]
    fn parses_porcelain_v2() {
        let out = b"# branch.oid abc\0# branch.head feat\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 a b src/x.rs\0\
2 R. N... 100644 100644 100644 a b R100 new name\0old name\0\
u UU N... 100644 100644 100644 100644 a b c conflict.rs\0? untracked file\0";
        let l = parse_status(out);
        assert_eq!(l.branch.as_deref(), Some("feat"));
        assert_eq!(l.head, "abc");
        assert_eq!(l.remote, None, "@{{u}} is ignored");
        assert_eq!((l.changed, l.conflicts), (4, 1));
        let d = parse_status(b"# branch.oid abc\0# branch.head (detached)\0");
        assert_eq!(d.branch, None);
    }

    #[test]
    fn rollup_summary() {
        let v: Value = serde_json::from_str(
            r#"[{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"},
                {"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":""},
                {"__typename":"StatusContext","context":"ci/x","state":"SUCCESS"}]"#,
        )
        .unwrap();
        assert_eq!(checks(&v), Checks::Failing(vec!["test".into()]));
        let pending: Value = serde_json::from_str(
            r#"[{"name":"lint","status":"QUEUED","conclusion":""},{"context":"ci","state":"SUCCESS"}]"#,
        )
        .unwrap();
        assert_eq!(checks(&pending), Checks::Pending);
        let rerun: Value = serde_json::from_str(
            r#"[{"name":"t","status":"COMPLETED","conclusion":"SUCCESS","startedAt":"2026-01-02"},
                {"name":"t","status":"COMPLETED","conclusion":"FAILURE","startedAt":"2026-01-01"},
                {"name":"s","status":"COMPLETED","conclusion":"SKIPPED"}]"#,
        )
        .unwrap();
        assert_eq!(checks(&rerun), Checks::Passing, "a passing re-run wins");
        assert_eq!(checks(&serde_json::json!([])), Checks::Passing);
    }

    #[test]
    fn picks_own_open_pr() {
        let json = r#"[{"url":"u1","number":1,"state":"CLOSED","updatedAt":"2026-01-02"},
                       {"url":"u3","number":3,"state":"OPEN","isCrossRepository":true},
                       {"url":"u2","number":2,"state":"OPEN","updatedAt":"2026-01-01"}]"#;
        assert_eq!(parse_prs(json).unwrap().number, 2);
        assert_eq!(parse_prs("[]"), None);
    }

    fn repo(name: &str, l: Local, pr: Option<Pr>) -> Repo {
        Repo {
            name: name.into(),
            base: "origin/main".into(),
            local: Ok(l),
            pr: Some(Ok(pr)),
        }
    }

    /// Clean, pushed, `ahead` commits on top of the base.
    fn clean(ahead: u32) -> Local {
        Local {
            branch: Some("feat".into()),
            head: "h".into(),
            origin: Some("git@github.com:o/r.git".into()),
            remote: (ahead > 0).then_some((0, 0)),
            base: (ahead, 0),
            ..Default::default()
        }
    }

    fn pr(merge_state: &str, checks: Checks) -> Pr {
        Pr {
            url: "u".into(),
            number: 1,
            state: PrState::Open,
            draft: false,
            merge_state: merge_state.into(),
            review: String::new(),
            checks,
            head: "h".into(),
            change_requests: vec![],
            comments: vec![],
            contains_head: String::new(),
        }
    }

    fn next(repos: &[Repo]) -> Action {
        next_action(repos, "feat", &[])
    }

    fn one(l: Local, p: Option<Pr>) -> Action {
        next(&[repo("a", l, p)])
    }

    #[test]
    fn local_steps() {
        assert_eq!(one(clean(0), None).label, "No changes");
        let dirty = Local {
            changed: 2,
            ..clean(1)
        };
        let a = next(&[repo("api", dirty, None), repo("web", clean(1), None)]);
        assert_eq!(a.label, "Commit and push · api");
        assert!(matches!(a.kind, Kind::Agent(ref p) if p.contains("api") && !p.contains("web")));
        assert_eq!(
            one(
                Local {
                    remote: None,
                    ..clean(1)
                },
                None
            )
            .kind,
            Kind::CreatePr
        );
        assert_eq!(
            one(
                Local {
                    remote: Some((0, 2)),
                    ..clean(1)
                },
                None
            )
            .kind,
            Kind::Pull
        );
        assert!(matches!(
            one(
                Local {
                    remote: Some((1, 2)),
                    ..clean(1)
                },
                None
            )
            .kind,
            Kind::Agent(_)
        ));
        let rewritten = Local {
            remote: Some((3, 2)),
            rewritten: true,
            ..clean(3)
        };
        assert_eq!(one(rewritten, None).kind, Kind::ForcePush);
        let rebasing = Local {
            op: Some(Op::Rebase),
            branch: None,
            ..clean(1)
        };
        assert_eq!(
            one(rebasing, None).label,
            "Finish operation",
            "mid-rebase isn't 'detached'"
        );
        let conflicted = Local {
            op: Some(Op::Merge),
            conflicts: 1,
            changed: 1,
            ..clean(1)
        };
        assert_eq!(one(conflicted, None).label, "Resolve conflicts");
        assert_eq!(
            one(
                Local {
                    branch: None,
                    ..clean(1)
                },
                None
            )
            .label,
            "Fix detached HEAD"
        );
        assert_eq!(
            one(
                Local {
                    branch: Some("main".into()),
                    ..clean(1)
                },
                None
            )
            .label,
            "Switch to branch"
        );
        assert!(
            one(
                Local {
                    origin: None,
                    remote: None,
                    ..clean(1)
                },
                None
            )
            .attention
        );
        let broken = Repo {
            name: "a".into(),
            base: "b".into(),
            local: Err("gone".into()),
            pr: None,
        };
        assert!(next(&[broken]).attention);
        let offline = Repo {
            pr: Some(Err("offline".into())),
            ..repo("a", clean(1), None)
        };
        assert_eq!(
            next(&[offline]).label,
            "GitHub unavailable",
            "not 'Create PR'"
        );
    }

    #[test]
    fn pr_steps() {
        let unpushed = Local {
            remote: Some((1, 0)),
            ..clean(2)
        };
        assert_eq!(
            one(unpushed, Some(pr("CLEAN", Checks::Passing))).kind,
            Kind::Push
        );
        let failing = pr("UNSTABLE", Checks::Failing(vec!["test".into()]));
        assert_eq!(one(clean(1), Some(failing)).label, "Fix checks");
        assert_eq!(
            one(clean(1), Some(pr("BEHIND", Checks::Passing))).kind,
            Kind::UpdateBranch
        );
        assert_eq!(
            one(clean(1), Some(pr("BLOCKED", Checks::Pending))).label,
            "Checks running"
        );
        assert_eq!(
            one(clean(1), Some(pr("UNKNOWN", Checks::Passing))).label,
            "Checking PR…"
        );
        let lagging = Pr {
            head: "old".into(),
            ..pr("CLEAN", Checks::Passing)
        };
        assert_eq!(
            one(clean(1), Some(lagging)).label,
            "Checking PR…",
            "GitHub hasn't seen the push yet"
        );
        let review = Pr {
            review: "REVIEW_REQUIRED".into(),
            ..pr("BLOCKED", Checks::Passing)
        };
        assert_eq!(one(clean(1), Some(review)).label, "Awaiting review");
        assert_eq!(
            one(clean(1), Some(pr("BLOCKED", Checks::Passing))).label,
            "Merge blocked"
        );
        assert_eq!(
            one(clean(1), Some(pr("CLEAN", Checks::Passing))).kind,
            Kind::Merge
        );
        let draft = Pr {
            draft: true,
            ..pr("DRAFT", Checks::Passing)
        };
        assert_eq!(one(clean(1), Some(draft)).kind, Kind::Ready);

        // Actionable beats passive across repos.
        let a = next(&[
            repo("api", clean(1), Some(pr("BLOCKED", Checks::Pending))),
            repo(
                "web",
                clean(1),
                Some(Pr {
                    draft: true,
                    ..pr("DRAFT", Checks::Passing)
                }),
            ),
        ]);
        assert_eq!(a.label, "Ready for review · web");

        // Merged at HEAD → archive; new commits after the merge → a new PR; closed at HEAD → reopen.
        let merged = Pr {
            state: PrState::Merged,
            ..pr("", Checks::Passing)
        };
        assert_eq!(one(clean(1), Some(merged.clone())).kind, Kind::Archive);
        assert_eq!(
            one(
                Local {
                    head: "h2".into(),
                    ..clean(2)
                },
                Some(merged)
            )
            .kind,
            Kind::CreatePr
        );
        let closed = Pr {
            state: PrState::Closed,
            ..pr("", Checks::Passing)
        };
        assert_eq!(one(clean(1), Some(closed)).kind, Kind::Reopen);
    }

    #[test]
    fn merged_at_a_newer_head_landed() {
        let gone = Local {
            remote: None, // auto-deleted after the merge
            ..clean(1)
        };
        let merged = Pr {
            state: PrState::Merged,
            head: "newer".into(),
            ..pr("", Checks::Passing)
        };
        assert_eq!(one(gone.clone(), Some(merged.clone())).kind, Kind::CreatePr);
        let contains = Pr {
            contains_head: "h".into(),
            ..merged
        };
        assert!(repo("a", gone.clone(), Some(contains.clone())).landed());
        assert_eq!(one(gone, Some(contains)).kind, Kind::Archive);
    }

    #[test]
    fn stale_or_unknown_repos_wait_alone() {
        let lagging = Pr {
            head: "old".into(),
            ..pr("DIRTY", Checks::Failing(vec!["test".into()]))
        };
        assert_eq!(one(clean(1), Some(lagging.clone())).label, "Checking PR…");
        let draft = Pr {
            draft: true,
            ..pr("DRAFT", Checks::Passing)
        };
        let web = || repo("web", clean(1), Some(draft.clone()));
        assert_eq!(
            next(&[repo("api", clean(1), Some(lagging)), web()]).label,
            "Ready for review · web"
        );
        let offline = Repo {
            pr: Some(Err("not GitHub".into())),
            ..repo("api", clean(1), None)
        };
        assert_eq!(
            next(&[offline.clone(), web()]).label,
            "Ready for review · web"
        );
        assert_eq!(
            next(&[offline.clone(), repo("web", clean(1), None)]).label,
            "Create PR · web"
        );
        let a = next(&[
            offline,
            repo("web", clean(1), Some(pr("CLEAN", Checks::Passing))),
        ]);
        assert_eq!(a.label, "GitHub unavailable · api", "not Merge");
    }

    #[test]
    fn a_handled_review_awaits_re_review() {
        let reviewed = |ids: &[&str]| Pr {
            review: "CHANGES_REQUESTED".into(),
            change_requests: ids.iter().map(|s| s.to_string()).collect(),
            ..pr("BLOCKED", Checks::Passing)
        };
        let ask = |p: Pr, handled: &[String]| {
            next_action(&[repo("a", clean(1), Some(p))], "feat", handled)
        };
        let a = ask(reviewed(&["r1"]), &[]);
        assert_eq!(
            (a.label.as_str(), a.reviews),
            ("Address review", vec!["r1".to_string()])
        );
        let handled = vec!["r1".to_string()];
        assert_eq!(ask(reviewed(&["r1"]), &handled).label, "Awaiting re-review");
        assert_eq!(
            ask(reviewed(&["r1", "r2"]), &handled).label,
            "Address review"
        );
        let json = r#"[{"state":"OPEN","reviews":[{"id":"r1","state":"CHANGES_REQUESTED"},{"id":"r2","state":"COMMENTED"}]}]"#;
        assert_eq!(parse_prs(json).unwrap().change_requests, ["r1"]);
    }

    #[test]
    fn comment_only_reviews_ask_once() {
        let json = r#"[{"state":"OPEN","author":{"login":"me"},"reviews":[
            {"id":"c1","state":"COMMENTED","author":{"login":"rev"}},
            {"id":"c2","state":"COMMENTED","author":{"login":"me"}}]}]"#;
        assert_eq!(
            parse_prs(json).unwrap().comments,
            ["c1"],
            "own replies skipped"
        );
        let p = Pr {
            comments: vec!["c1".into()],
            ..pr("CLEAN", Checks::Passing)
        };
        let ask = |h: &[String]| next_action(&[repo("a", clean(1), Some(p.clone()))], "feat", h);
        let a = ask(&[]);
        assert_eq!(
            (a.label.as_str(), a.reviews),
            ("Address comments", vec!["c1".to_string()])
        );
        assert_eq!(ask(&["c1".to_string()]).kind, Kind::Merge);
    }

    #[test]
    fn step_guards() {
        let args = |k: Kind, l: Local, p: Option<Pr>| step_args(&k, "feat", &repo("a", l, p));
        let behind = Local {
            remote: Some((0, 2)),
            ..clean(1)
        };
        assert_eq!(
            args(Kind::Pull, behind.clone(), None).unwrap().unwrap(),
            ["pull", "--ff-only", "--no-rebase", "origin", "feat"]
        );
        assert_eq!(args(Kind::Pull, clean(1), None), Ok(None));
        let other = Local {
            branch: Some("main".into()),
            ..behind
        };
        assert!(args(Kind::Pull, other.clone(), None).is_err());
        assert!(args(Kind::Push, other, None).is_err());
        let rebasing = Local {
            op: Some(Op::Rebase),
            remote: Some((1, 0)),
            ..clean(1)
        };
        assert!(args(Kind::Push, rebasing, None).is_err());
        assert_eq!(
            args(Kind::Push, clean(1), None),
            Ok(None),
            "nothing to push"
        );
        assert_eq!(
            args(Kind::ForcePush, clean(1), None),
            Ok(None),
            "not rewritten"
        );
        let rewritten = Local {
            remote: Some((1, 1)),
            remote_head: "probed".into(),
            rewritten: true,
            ..clean(1)
        };
        assert!(
            args(Kind::ForcePush, rewritten, None).unwrap().unwrap()[1]
                == "--force-with-lease=feat:probed"
        );
        let open = || Some(pr("CLEAN", Checks::Passing));
        assert_eq!(
            args(Kind::Merge, clean(1), open()).unwrap().unwrap(),
            ["pr", "merge", "u", "--match-head-commit", "h"]
        );
        let moved = Local {
            head: "h2".into(),
            ..clean(1)
        };
        assert!(
            args(Kind::Merge, moved, open()).is_err(),
            "PR head isn't ours"
        );
        let dirty = Local {
            changed: 1,
            ..clean(1)
        };
        assert!(args(Kind::Merge, dirty, open()).is_err());
        assert_eq!(args(Kind::Merge, clean(1), None), Ok(None));
    }

    /// The real thing: a linked worktree from `create_workspace` against a bare origin.
    #[test]
    fn probes_a_linked_worktree() {
        use crate::git::{create_workspace, git};
        use std::fs;
        let root = std::env::temp_dir().join(format!("shark-sync-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (origin, src) = (root.join("origin.git"), root.join("src/api"));
        fs::create_dir_all(&src).unwrap();
        git(
            &root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &origin.to_string_lossy(),
            ],
        )
        .unwrap();
        let c = |dir: &Path, args: &[&str]| {
            git(
                dir,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        git(&src, &["init", "-q", "-b", "main"]).unwrap();
        fs::write(src.join("README.md"), "hi\n").unwrap();
        c(&src, &["add", "."]);
        c(&src, &["commit", "-qm", "init"]);
        git(
            &src,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )
        .unwrap();
        git(&src, &["push", "-q", "-u", "origin", "main"]).unwrap();
        git(&src, &["remote", "set-head", "origin", "main"]).unwrap();
        let repo = crate::git::add_repo(&src, &[]).unwrap();
        let ws = create_workspace(&root.join("ws/feat"), "feat", "feat", &[repo]).unwrap();
        let wt = root.join("ws/feat/api");
        let base = ws.repos[0].base.clone();
        let p = || probe(&wt, "feat", &base, None).unwrap();

        let l = p();
        assert_eq!(
            (l.branch.as_deref(), l.remote, l.base, l.origin.is_some()),
            (Some("feat"), None, (0, 0), true)
        );
        assert!(
            git(&wt, &["rev-parse", "@{u}"]).is_err(),
            "new branch must not track its base"
        );

        fs::write(wt.join("README.md"), "feat\n").unwrap();
        assert_eq!(p().changed, 1);
        c(&wt, &["commit", "-qam", "feat"]);
        let r = |l: Local| Repo {
            name: "api".into(),
            base: base.clone(),
            local: Ok(l),
            pr: Some(Ok(None)),
        };
        assert_eq!(next_action(&[r(p())], "feat", &[]).kind, Kind::CreatePr);

        git(&wt, &["push", "-q", "-u", "origin", "feat"]).unwrap();
        assert_eq!(p().remote, Some((0, 0)));
        // The rewrite must be the newer side; a tie could equally be the remote's rewrite.
        let status = std::process::Command::new("git")
            .current_dir(&wt)
            .env("GIT_COMMITTER_DATE", "2099-01-01T00:00:00")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--amend",
                "-m",
                "reworded",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let l = p();
        assert!(l.remote == Some((1, 1)) && l.rewritten, "{l:?}");
        let fake = Local {
            remote: Some((7, 7)),
            ..l.clone()
        };
        assert_eq!(
            probe(&wt, "feat", &base, Some(&fake)).unwrap().remote,
            Some((7, 7)),
            "unmoved refs reuse the last counts"
        );
        let force = step_args(&Kind::ForcePush, "feat", &r(l.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(next_action(&[r(l)], "feat", &[]).kind, Kind::ForcePush);
        // Someone pushes after the probe: the lease on the probed commit must refuse.
        git(&src, &["push", "-qf", "origin", "main:feat"]).unwrap();
        let force: Vec<&str> = force.iter().map(String::as_str).collect();
        let e = git(&wt, &force).unwrap_err();
        assert!(e.contains("stale info"), "{e}");

        // A conflicting merge; its MERGE_HEAD lives in .git/worktrees/<name>/, not the worktree.
        c(&src, &["checkout", "-qb", "other"]);
        fs::write(src.join("README.md"), "other\n").unwrap();
        c(&src, &["commit", "-qam", "other"]);
        assert!(
            git(
                &wt,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "merge",
                    "other"
                ]
            )
            .is_err()
        );
        let l = p();
        assert_eq!((l.op, l.conflicts), (Some(Op::Merge), 1));
        assert_eq!(next_action(&[r(l)], "feat", &[]).label, "Resolve conflicts");
        fs::remove_dir_all(&root).unwrap();
    }
}
