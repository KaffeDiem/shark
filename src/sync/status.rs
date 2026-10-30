//! Pure git/GitHub state per repo and the step it calls for. No gpui here, so it is unit tested.
use crate::prs::PrStatus;
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
    /// This branch is an isolated checkout for reviewing somebody else's PR.
    pub review: bool,
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
    /// The repo's default branch, e.g. `origin/main`; a base other than it may be a stack.
    pub trunk: String,
    /// The parent commit this branch is built on (`git::parent_tip_key`); its commits after it
    /// are its own. Empty when the branch was never stacked.
    pub parent_tip: String,
    /// HEAD holds parent commits the base doesn't: the parent was rewritten or squash-merged,
    /// so what follows `parent_tip` must be rebased onto the base.
    pub restack: bool,
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
    /// This answer belongs to a review checkout; a later config change must not authorize writes.
    pub review_checkout: bool,
    pub number: u64,
    pub state: PrState,
    pub draft: bool,
    /// GitHub's `mergeStateStatus`: CLEAN, DIRTY, BEHIND, BLOCKED, UNSTABLE, HAS_HOOKS, DRAFT, UNKNOWN.
    pub merge_state: String,
    /// `reviewDecision`: APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or empty.
    pub review: String,
    pub checks: Checks,
    pub head: String,
    /// The branch it merges into, e.g. `main`.
    pub base: String,
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

    /// GitHub hasn't computed mergeability yet.
    pub fn merge_unknown(&self) -> bool {
        matches!(self.merge_state.as_str(), "UNKNOWN" | "")
    }

    /// The base has moved on since the head was cut.
    pub fn behind(&self) -> bool {
        self.merge_state == "BEHIND"
    }

    /// Lifecycle label of the PR (not its checks).
    pub fn status(&self) -> PrStatus {
        match self.state {
            PrState::Merged => PrStatus::Merged,
            PrState::Closed => PrStatus::Closed,
            PrState::Open if self.draft => PrStatus::Draft,
            PrState::Open => PrStatus::Open,
        }
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

/// Probes one worktree. `--no-optional-locks` keeps `git status` from taking
/// `index.lock`, which would make the agent's own git commands fail. `prev` is the last probe:
/// while HEAD and `origin/<branch>` haven't moved, its remote counts still hold.
/// Only an authoritative base may advance or clear the recorded parent boundary.
pub fn probe(
    wt: &Path,
    branch: &str,
    base: &str,
    prev: Option<&Local>,
    allow_parent_update: bool,
) -> Result<Local, String> {
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
    l.review = crate::git::review_pr_url(wt, branch).is_some();
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
    l.trunk = trunk_of(wt, prev);
    let remote = format!("refs/remotes/origin/{branch}");
    if let Some(oid) = crate::git::ref_oid(wt, &remote) {
        if let Some(p) =
            prev.filter(|p| p.remote.is_some() && (&p.head, &p.remote_head) == (&l.head, &oid))
        {
            (l.remote, l.rewritten, l.remote_head) = (p.remote, p.rewritten, oid);
        } else {
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
    }
    base_counts(wt, base, l).map(|l| parent_tip(wt, branch, base, l, allow_parent_update))
}

/// The trunk the last probe found, else the repo's default branch.
pub fn trunk_of(wt: &Path, prev: Option<&Local>) -> String {
    (prev.map(|p| p.trunk.clone()).filter(|t| !t.is_empty()))
        .unwrap_or_else(|| crate::git::default_base(wt))
}

/// Reads the parent tip, and moves it to the base's tip once HEAD has all of the base (the
/// branch was rebased or merged up): from then on, that is where its own commits start. The one
/// write a probe makes, and only when the tip moves; `config.lock` isn't the agent's index.
fn parent_tip(
    wt: &Path,
    branch: &str,
    base: &str,
    mut l: Local,
    allow_parent_update: bool,
) -> Local {
    let key = crate::git::parent_tip_key(branch);
    l.parent_tip = git(wt, &["config", &key]).unwrap_or_default();
    let stacked = crate::stack::parent_of(base, &l.trunk).is_some();
    if l.parent_tip.is_empty() && !stacked {
        return l;
    }
    // Only the real base: a fallback (a pruned parent) says nothing about the boundary.
    let Some(tip) = crate::git::ref_oid(wt, &format!("{base}^{{commit}}")) else {
        return l;
    };
    let ancestor = |a: &str, b: &str| {
        let out = run(wt, "git", &["merge-base", "--is-ancestor", "--", a, b]).ok()?;
        match out.status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    };
    let in_head = ancestor(&l.parent_tip, "HEAD");
    let in_base = ancestor(&l.parent_tip, &tip);
    l.restack = !l.parent_tip.is_empty() && in_head == Some(true) && in_base == Some(false);
    if l.base.1 == 0 {
        if !allow_parent_update {
            return l;
        }
        // An old trunk (or a parent rewritten backwards) can be in HEAD before inherited work is removed.
        if !l.parent_tip.is_empty() && in_base != Some(true) && in_head != Some(false) {
            return l;
        }
        if !stacked {
            // Restacked onto the trunk: nothing left to track.
            if git(
                wt,
                &["config", "--fixed-value", "--unset", &key, &l.parent_tip],
            )
            .is_ok()
            {
                l.parent_tip.clear();
            }
        } else if l.parent_tip != tip && git(wt, &["config", &key, &tip]).is_ok() {
            l.parent_tip = tip;
        }
    }
    l
}

fn base_counts(wt: &Path, base: &str, mut l: Local) -> Result<Local, String> {
    // A stacked branch's parent may be gone from origin (merged and deleted) or not pushed yet:
    // then count against the local parent, else the trunk. (0, 0) would read as "nothing of its
    // own", and archive deletes such branches. Still (0, 0) on an unborn branch.
    let parent = crate::stack::parent_of(base, &l.trunk).map(|p| format!("refs/heads/{p}"));
    let tries = std::iter::once(base.to_string())
        .chain(parent)
        .chain([l.trunk.clone()]);
    let mut error = format!("no base available for `{base}`");
    for b in tries.filter(|b| !b.is_empty()) {
        let range = format!("HEAD...{b}");
        match git(wt, &["rev-list", "--left-right", "--count", &range]) {
            Ok(c) => {
                l.base = counts(&c);
                return Ok(l);
            }
            Err(e) => error = e,
        }
    }
    match l.head.as_str() {
        "(initial)" => Ok(l),
        _ => Err(format!("Can't compare HEAD with base `{base}`: {error}")),
    }
}

const PR_FIELDS: &str = "url,number,state,isDraft,mergeStateStatus,reviewDecision,statusCheckRollup,headRefOid,baseRefName,isCrossRepository,updatedAt,reviews,author";

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
    // A re-run leaves the old run in the rollup; only the newest per workflow and name counts
    // (two workflows may both have a `build`).
    let key = |c: &Value| {
        (
            c["workflowName"].as_str().unwrap_or("").to_string(),
            name(c),
        )
    };
    let mut latest: Vec<&Value> = vec![];
    for c in items {
        match latest.iter().position(|l| key(l) == key(c)) {
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

/// `branch`'s PRs of any state in origin's repo (`--repo`, so a fork's upstream never answers),
/// as `gh pr list --json fields` gives them; same-named fork branches are left out.
pub fn branch_prs(wt: &Path, branch: &str, fields: &str, secs: u64) -> Result<Vec<Value>, String> {
    let origin = origin_repo(wt);
    let args = crate::pr::with_repo(
        vec![
            "pr", "list", "--head", branch, "--state", "all", "--json", fields, "--limit", "100",
        ],
        origin.as_deref(),
    );
    let json = cmd_bounded(wt, "gh", &args, secs)?;
    let list: Vec<Value> = serde_json::from_str(&json).map_err(|e| format!("gh: {e}"))?;
    Ok(own(&list).into_iter().cloned().collect())
}

/// Picks this branch's PR from `gh pr list --head … --state all`: the open one, else the newest.
/// Fork PRs whose branch happens to have the same name are ignored.
#[cfg(test)]
pub fn parse_prs(json: &str) -> Option<Pr> {
    let list: Vec<Value> = serde_json::from_str(json).ok()?;
    Some(parse_pr(pick_pr(&own(&list))?))
}

/// The PRs from this repo itself; a fork's PR on a same-named branch isn't about it.
pub fn own(list: &[Value]) -> Vec<&Value> {
    list.iter()
        .filter(|p| p["isCrossRepository"] != true)
        .collect()
}

/// The open PR, else the most recently updated; also how the Git tab picks, so both agree.
pub fn pick_pr<'a>(own: &[&'a Value]) -> Option<&'a Value> {
    own.iter()
        .copied()
        .find(|p| p["state"] == "OPEN")
        .or_else(|| {
            own.iter()
                .copied()
                .max_by_key(|p| p["updatedAt"].as_str().unwrap_or("").to_string())
        })
}

fn parse_pr(pick: &Value) -> Pr {
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
    Pr {
        url: s("url"),
        review_checkout: false,
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
        base: s("baseRefName"),
        change_requests: ids(&|r| r["state"] == "CHANGES_REQUESTED"),
        // The author's own replies to threads are COMMENTED reviews too; they ask for nothing.
        comments: ids(&|r| {
            r["state"] == "COMMENTED" && r["author"]["login"] != pick["author"]["login"]
        }),
        contains_head: String::new(),
    }
}

/// A stacked repo's parent PR, when it tells the parent's state: open, or merged or closed with
/// the head origin still has (or origin dropped the branch). A long-lived base like `develop`
/// with an old merged PR has moved on since; that PR isn't about it.
pub fn fetch_parent(wt: &Path, parent: &str) -> Result<Option<Pr>, String> {
    let pr = fetch_pr(wt, parent, "")?;
    let remote = format!("refs/remotes/origin/{parent}");
    let tip = crate::git::ref_oid(wt, &remote);
    Ok(pr.filter(|p| p.state == PrState::Open || tip.as_ref().is_none_or(|t| *t == p.head)))
}

/// Origin's GitHub URL, for `gh --repo`: gh may default to a fork's upstream, but branch
/// ownership belongs to origin.
pub fn origin_repo(wt: &Path) -> Option<String> {
    git(wt, &["config", "--get", "remote.origin.url"])
        .ok()
        .and_then(|url| github_url(&url))
}

/// `Ok(None)` means the branch has no PR. Unlike `gh pr view`, "none" isn't an error exit,
/// so it can't be confused with being offline or logged out.
/// `head` is the local HEAD as just probed.
pub fn fetch_pr(wt: &Path, branch: &str, head: &str) -> Result<Option<Pr>, String> {
    let review = crate::git::review_pr_url(wt, branch);
    let origin = origin_repo(wt);
    let own = if review.is_none() || origin.is_some() {
        let result = branch_prs(wt, branch, PR_FIELDS, 20)
            .map(|list| pick_pr(&list.iter().collect::<Vec<_>>()).map(parse_pr));
        match result {
            Ok(pr) => pr,
            // A private fork can be inaccessible while the original review is still readable.
            Err(_) if review.is_some() => None,
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    if let Some(url) = &review {
        if let Some(pr) = own
            .as_ref()
            .filter(|p| published_review(wt, branch, url, origin.as_deref(), p))
        {
            git(
                wt,
                &["check-ref-format", &format!("refs/heads/{}", pr.base)],
            )?;
            // Only remove the identity we read; another workspace may have changed it meanwhile.
            git(
                wt,
                &[
                    "config",
                    "--fixed-value",
                    "--unset",
                    &format!("branch.{branch}.shark-pr-url"),
                    url,
                ],
            )?;
        } else {
            let json = cmd_bounded(wt, "gh", &["pr", "view", url, "--json", PR_FIELDS], 20)?;
            let value = serde_json::from_str(&json).map_err(|e| format!("gh: {e}"))?;
            return Ok(Some(Pr {
                review_checkout: true,
                ..parse_pr(&value)
            }));
        }
    }
    let mut pr = own;
    if let Some(p) = pr
        .as_mut()
        .filter(|p| p.state == PrState::Merged && !head.is_empty() && p.head != head)
    {
        mark_contains_head(wt, p, head);
    }
    Ok(pr)
}

/// Whether `p`, an open PR of this repo, is the review checkout `branch`'s own published work
/// (`url` is the reviewed PR): its identity may be dropped.
fn published_review(wt: &Path, branch: &str, url: &str, origin: Option<&str>, p: &Pr) -> bool {
    p.state == PrState::Open
        && p.url != url
        && p.number != 0
        && !p.base.is_empty()
        && origin.is_some_and(|o| p.url == format!("{o}/pull/{}", p.number))
        && !p.head.is_empty()
        // Publishing establishes ownership even if GitHub has since advanced or rewritten it.
        && ((git(wt, &["config", "--get", &format!("branch.{branch}.remote")]).as_deref()
            == Ok("origin")
            && git(wt, &["config", "--get", &format!("branch.{branch}.merge")]).as_deref()
                == Ok(format!("refs/heads/{branch}").as_str()))
            || git(
                wt,
                &["merge-base", "--is-ancestor", "--", &p.head, &format!("refs/heads/{branch}")],
            )
            .is_ok())
}

/// Sets `p.contains_head` to `head` when the merged PR's head holds it.
fn mark_contains_head(wt: &Path, p: &mut Pr, head: &str) {
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

/// Everything known about one repo of a workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    pub name: String,
    /// Ref the branch was cut from, e.g. `origin/main`.
    pub base: String,
    /// The branch its worktree should be on.
    pub branch: String,
    pub local: Result<Local, String>,
    /// `None` until GitHub was asked; `Some(Err)` when gh failed (offline, logged out, not GitHub).
    pub pr: Option<Result<Option<Pr>, String>>,
    /// Stacked only: the parent branch's PR, as last answered (kept while gh fails).
    pub parent: Option<Pr>,
}

impl Repo {
    /// Known by name only: nothing probed yet.
    pub fn fresh(r: &crate::state::WsRepo, ws: &crate::state::Workspace) -> Repo {
        Repo {
            name: r.name.clone(),
            base: r.base.clone(),
            branch: ws.branch_of(r).into(),
            local: Ok(Local::default()),
            pr: None,
            parent: None,
        }
    }

    pub fn pr(&self) -> Option<&Pr> {
        self.pr.as_ref()?.as_ref().ok()?.as_ref()
    }

    pub fn is_review(&self) -> bool {
        self.local.as_ref().is_ok_and(|l| l.review) || self.pr().is_some_and(|p| p.review_checkout)
    }

    pub fn open_pr(&self) -> Option<&Pr> {
        self.pr().filter(|p| p.state == PrState::Open)
    }

    /// The branch it is stacked on: its base is `origin/<parent>` and not the trunk.
    pub fn stacked(&self) -> Option<&str> {
        let trunk = self.local.as_ref().map(|l| l.trunk.as_str()).ok()?;
        crate::stack::parent_of(&self.base, trunk).filter(|_| !trunk.is_empty())
    }

    /// Stacked on a parent whose PR is in `state`.
    pub fn parent_is(&self, state: PrState) -> Option<&Pr> {
        self.stacked()?;
        self.parent.as_ref().filter(|p| p.state == state)
    }

    /// Its parent merged and its own work hasn't landed: time to move onto the parent's base.
    pub fn parent_merged(&self) -> bool {
        self.parent_is(PrState::Merged).is_some() && !self.landed()
    }
}

/// One repo's part of the `restack` prompt: where its commits go and the parent commit they
/// start after (the recorded tip; else a merged parent's last commit; else the merge base).
pub fn restack_line(r: &Repo, onto: &str) -> String {
    let tip = (r.local.as_ref()).map_or("", |l| l.parent_tip.as_str());
    let after = match &r.parent {
        _ if !tip.is_empty() => format!("`{tip}`"),
        Some(p) if r.stacked().is_some() && p.state != PrState::Open && !p.head.is_empty() => {
            format!(
                "`{}` (the parent PR's last commit; `git fetch origin pull/{}/head` if it's missing)",
                p.head, p.number
            )
        }
        _ => format!("`$(git merge-base HEAD {})`", r.base),
    };
    format!("{}: onto `{onto}`, after {after}", r.name)
}

/// The `restack` prompt for `repos`, each onto `onto(repo)`.
pub fn restack_prompt(repos: &[&Repo], onto: impl Fn(&Repo) -> String) -> String {
    let fill = |names: &[String]| {
        (repos.iter().filter(|r| names.contains(&r.name)))
            .map(|r| restack_line(r, &onto(r)))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let pairs = repos.iter().map(|r| (&*r.name, &*r.branch));
    prompts::render_each("restack", pairs, fill)
}

/// `origin/main` → `main`, for labels.
pub fn short(base: &str) -> &str {
    base.strip_prefix("origin/").unwrap_or(base)
}

impl Repo {
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
            _ if l.review => parts.push("local PR review".into()),
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
                match (p.status(), &p.checks) {
                    (s @ (PrStatus::Merged | PrStatus::Closed), _) => s.label(),
                    (_, Checks::Failing(_)) => "checks failing",
                    (_, Checks::Pending) => "checks running",
                    (s, _) => s.label(),
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
    /// The parent PR merged: move the base to the parent's base, then restack with the agent.
    Restack,
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
    /// Direct writes must not race an agent; agent prompts go through the chat queue.
    pub fn needs_idle(&self) -> bool {
        !matches!(self, Kind::Agent(_) | Kind::OpenPr | Kind::None)
    }

    /// Opens PRs, as a new one or a draft.
    pub fn creates_pr(&self) -> bool {
        matches!(self, Kind::CreatePr | Kind::CreateDraftPr)
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

/// `r` is among `rs`; an empty scope means every repo.
pub fn in_scope(rs: &[String], r: &Repo) -> bool {
    rs.is_empty() || rs.contains(&r.name)
}

/// The single most useful next step for the whole workspace. Local problems come before
/// remote ones, and within the PR phase actionable steps beat passive waits, so a
/// "checks running" in one repo can't hide a "ready for review" in another. `handled`: review
/// IDs already sent to the agent.
pub fn next_action(repos: &[Repo], handled: &[String]) -> Action {
    if repos.iter().any(Repo::is_review) {
        let normal: Vec<_> = repos.iter().filter(|r| !r.is_review()).cloned().collect();
        if normal.is_empty() {
            return act(
                "Review PR",
                Kind::OpenPr,
                repos.iter().map(|r| r.name.clone()).collect(),
                false,
            );
        }
        let mut action = next_action(&normal, handled);
        if action.repos.is_empty() {
            action.repos = normal.iter().map(|r| r.name.clone()).collect();
        }
        return action;
    }
    let names = |f: &dyn Fn(&Repo, &Local) -> bool| -> Vec<String> {
        repos
            .iter()
            .filter(|r| r.local.as_ref().is_ok_and(|l| f(r, l)))
            .map(|r| r.name.clone())
            .collect()
    };
    // The `restack` prompt for `rs`, each onto its own base.
    let restack =
        |rs: &[String]| restack_prompt(&among(repos, rs).collect::<Vec<_>>(), |r| r.base.clone());
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
    // Prompt `key` for the repos in `rs`, once per branch; `fill` gives `{repos}`.
    let render = |key: &str, rs: &[String], fill: &dyn Fn(&[String]) -> String| {
        prompts::render_each(key, among(repos, rs).map(|r| (&*r.name, &*r.branch)), fill)
    };
    // An agent step whose prompt `key` names the repos and their branch.
    let ask = |verb: &str, key: &str, rs: Vec<String>, attention| {
        let prompt = render(key, &rs, &|rs| rs.join(", "));
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
            let rs = names(&|r, l| l.branch.as_deref() != Some(&r.branch));
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
        // A stack's parent changed under it; before any PR step, so no PR carries its commits.
        .or_else(|| {
            let rs = names(&|r, _| r.parent_merged());
            let onto = among(repos, &rs).find_map(|r| r.parent.as_ref());
            let verb = onto.map(|p| format!("Parent merged · rebase onto {}", p.base));
            step(&verb.unwrap_or_default(), Kind::Restack, rs, true)
        })
        .or_else(|| {
            let rs = names(&|r, _| r.parent_is(PrState::Closed).is_some());
            step("Parent PR closed", Kind::None, rs, true)
        })
        .or_else(|| {
            let rs = names(&|_, l| l.restack);
            let verb = among(repos, &rs)
                .next()
                .map(|r| format!("Restack onto {}", short(&r.base)));
            step(
                &verb.unwrap_or_default(),
                Kind::Agent(restack(&rs)),
                rs,
                false,
            )
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
            let prompt = render("pr_conflicts", &rs, &with_base);
            step("Resolve PR conflicts", Kind::Agent(prompt), rs, true)
        })
        .or_else(|| {
            let rs = current(&|p| matches!(p.checks, Checks::Failing(_)));
            let failing = |rs: &[String]| {
                among(repos, rs)
                    .filter_map(|r| match &r.open_pr()?.checks {
                        Checks::Failing(n) => Some(format!("{}: {}", r.name, n.join(", "))),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let prompt = render("checks", &rs, &failing);
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
            // GitHub's Update branch merges the parent in; a stack rebases instead.
            let behind = current(&|p| p.behind());
            let (stacked, rs): (Vec<String>, Vec<String>) = (behind.into_iter()).partition(|n| {
                among(repos, std::slice::from_ref(n)).any(|r| r.stacked().is_some())
            });
            let verb = among(repos, &stacked)
                .next()
                .map(|r| format!("Rebase onto {}", short(&r.base)));
            step(
                &verb.unwrap_or_default(),
                Kind::Agent(restack(&stacked)),
                stacked,
                false,
            )
            .or_else(|| step("Update branch", Kind::UpdateBranch, rs, false))
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
    // A wait opens the PRs it waits on in the browser; `rs` are those repos.
    let passive = |l: &str, rs: Vec<String>| act(l, Kind::OpenPr, rs, false);
    // GitHub computes mergeability lazily, and lags a push by a few seconds.
    let rs = prs(&|p, l| p.head != l.head || p.merge_unknown());
    if !rs.is_empty() {
        return passive("Checking PR…", rs);
    }
    let rs = prs(&|p, _| p.checks == Checks::Pending);
    if !rs.is_empty() {
        return passive("Checks running", rs);
    }
    let rs = prs(&|p, _| p.review == "CHANGES_REQUESTED");
    if !rs.is_empty() {
        return passive("Awaiting re-review", rs);
    }
    let blocked = prs(&|p, _| p.merge_state == "BLOCKED");
    if !blocked.is_empty() {
        let rs = prs(&|p, _| p.merge_state == "BLOCKED" && p.review == "REVIEW_REQUIRED");
        return match rs.is_empty() {
            true => passive("Merge blocked", blocked),
            false => passive("Awaiting review", rs),
        };
    }
    // A child merges after its parent; GitHub would otherwise merge it into the parent branch.
    let waiting = |r: &Repo| r.parent_is(PrState::Open).is_some();
    let mergeable = names(&|r, _| r.open_pr().is_some() && !waiting(r));
    if let Some(a) = step("Merge", Kind::Merge, mergeable, false) {
        return a;
    }
    if let Some(r) = repos.iter().find(|r| r.open_pr().is_some() && waiting(r)) {
        let p = r.parent.as_ref().map_or(0, |p| p.number);
        let rs = names(&|r, _| r.open_pr().is_some() && waiting(r));
        return passive(&format!("Waiting on parent PR #{p}"), rs);
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
        r.open_pr()
            .is_some_and(|p| p.checks == Checks::Pending || p.merge_unknown() || head_moved(r, p))
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
/// ForcePush, `gh` args for Merge (minus the merge method), Reopen, UpdateBranch and Ready.
/// `Ok(None)` when the repo has nothing to do. Menu steps skip the next-step check, so these guards are what keeps them from moving
/// the wrong commit.
pub fn step_args(kind: &Kind, r: &Repo) -> Result<Option<Vec<String>>, String> {
    if r.is_review() {
        return Err(format!("{} is a local PR review checkout", r.name));
    }
    let b = r.branch.as_str();
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
            for (state, reason) in [
                (PrState::Open, "must merge first"),
                (PrState::Merged, "has merged; restack onto its base first"),
                (PrState::Closed, "is closed; choose a new base first"),
            ] {
                if let Some(parent) = r.parent_is(state) {
                    return Err(format!("stacked on #{}, which {reason}", parent.number));
                }
            }
            if p.base != short(&r.base) {
                return Err(format!(
                    "PR targets `{}`, but this workspace targets `{}`; retarget the PR before merging",
                    p.base,
                    short(&r.base)
                ));
            }
            let l = local()?;
            // Merge exactly what this workspace has: clean, pushed, and GitHub's head matching.
            if l.changed > 0 || l.remote != Some((0, 0)) || p.head != l.head {
                return Err(
                    "local branch, origin and the PR don't match; push or pull first".into(),
                );
            }
            args(&["pr", "merge", &p.url, "--match-head-commit", &p.head])
        }
        Kind::Reopen => match r.pr().filter(|p| p.state == PrState::Closed) {
            Some(p) => args(&["pr", "reopen", &p.url]),
            None => Ok(None),
        },
        Kind::UpdateBranch => match r.open_pr().filter(|p| p.behind()) {
            Some(p) => args(&["pr", "update-branch", &p.url]),
            None => Ok(None),
        },
        Kind::Ready => match r.open_pr().filter(|p| p.draft) {
            Some(p) => args(&["pr", "ready", &p.url]),
            None => Ok(None),
        },
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
    fn only_direct_writes_need_idle_agents() {
        use super::Kind;
        for kind in [Kind::Agent("commit".into()), Kind::OpenPr, Kind::None] {
            assert!(!kind.needs_idle(), "{kind:?}");
        }
        for kind in [
            Kind::Pull,
            Kind::Push,
            Kind::ForcePush,
            Kind::CreatePr,
            Kind::CreateDraftPr,
            Kind::Restack,
            Kind::Reopen,
            Kind::UpdateBranch,
            Kind::Ready,
            Kind::Merge,
            Kind::Archive,
        ] {
            assert!(kind.needs_idle(), "{kind:?}");
        }
    }

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
    fn same_named_checks_of_two_workflows_do_not_hide_each_other() {
        let rollup: Value = serde_json::from_str(
            r#"[{"name":"build","workflowName":"ci","status":"COMPLETED","conclusion":"FAILURE","startedAt":"2026-01-01"},
                {"name":"build","workflowName":"release","status":"COMPLETED","conclusion":"SUCCESS","startedAt":"2026-01-02"}]"#,
        )
        .unwrap();
        assert_eq!(checks(&rollup), Checks::Failing(vec!["build".into()]));
        // The same workflow's re-run still replaces its older run.
        let rerun: Value = serde_json::from_str(
            r#"[{"name":"build","workflowName":"ci","status":"COMPLETED","conclusion":"FAILURE","startedAt":"2026-01-01"},
                {"name":"build","workflowName":"ci","status":"COMPLETED","conclusion":"SUCCESS","startedAt":"2026-01-02"}]"#,
        )
        .unwrap();
        assert_eq!(checks(&rerun), Checks::Passing);
    }

    #[test]
    fn button_and_git_tab_pick_the_same_pr() {
        // gh lists newest created first; the one updated last is not the first.
        let list: Vec<Value> = serde_json::from_str(
            r#"[{"url":"new","state":"CLOSED","updatedAt":"2026-01-01"},
                {"url":"old","state":"MERGED","updatedAt":"2026-03-01"}]"#,
        )
        .unwrap();
        let own: Vec<&Value> = list.iter().collect();
        assert_eq!(pick_pr(&own).unwrap()["url"], "old");
        assert_eq!(
            parse_prs(&serde_json::to_string(&list).unwrap())
                .unwrap()
                .url,
            "old"
        );
        let mut open = list.clone();
        open.push(serde_json::json!({"url":"open","state":"OPEN"}));
        let own: Vec<&Value> = open.iter().collect();
        assert_eq!(pick_pr(&own).unwrap()["url"], "open");
    }

    #[test]
    fn waits_open_only_the_prs_they_wait_on() {
        let waiting = |p: Pr| repo("web", clean(1), Some(p));
        let done = repo("api", clean(1), Some(pr("CLEAN", Checks::Passing)));
        let api_pending = repo("api", clean(1), Some(pr("CLEAN", Checks::Pending)));
        let a = next(&[api_pending, waiting(pr("CLEAN", Checks::Passing))]);
        assert_eq!(
            (a.label.as_str(), &a.kind),
            ("Checks running", &Kind::OpenPr)
        );
        assert_eq!(a.repos, ["api"]);
        let a = next(&[done.clone(), waiting(pr("BLOCKED", Checks::Passing))]);
        assert_eq!(
            (a.label.as_str(), a.repos),
            ("Merge blocked", vec!["web".to_string()])
        );
        let review = Pr {
            review: "REVIEW_REQUIRED".into(),
            ..pr("BLOCKED", Checks::Passing)
        };
        let a = next(&[done.clone(), waiting(review)]);
        assert_eq!(
            (a.label.as_str(), a.repos),
            ("Awaiting review", vec!["web".to_string()])
        );
        let changes = Pr {
            review: "CHANGES_REQUESTED".into(),
            change_requests: vec!["r1".into()],
            ..pr("CLEAN", Checks::Passing)
        };
        // The agent has seen r1; what is left is waiting for the reviewer.
        let a = next_action(&[done, waiting(changes)], &["r1".into()]);
        assert_eq!(
            (a.label.as_str(), a.repos),
            ("Awaiting re-review", vec!["web".to_string()])
        );
    }

    #[test]
    fn picks_own_open_pr() {
        let json = r#"[{"url":"u1","number":1,"state":"CLOSED","updatedAt":"2026-01-02"},
                       {"url":"u3","number":3,"state":"OPEN","isCrossRepository":true},
                       {"url":"u2","number":2,"state":"OPEN","updatedAt":"2026-01-01"}]"#;
        assert_eq!(parse_prs(json).unwrap().number, 2);
        assert_eq!(parse_prs("[]"), None);
        let fork = serde_json::json!({"url":"https://github.com/o/r/pull/3","number":3,"state":"OPEN","isCrossRepository":true});
        assert_eq!(
            parse_pr(&fork).number,
            3,
            "an explicitly selected PR retains fork identity"
        );
    }

    #[test]
    fn review_checkouts_never_offer_publishing() {
        let review = repo(
            "review",
            Local {
                review: true,
                changed: 1,
                ..clean(2)
            },
            Some(pr("CLEAN", Checks::Passing)),
        );
        let action = next_action(std::slice::from_ref(&review), &[]);
        assert_eq!(action.label, "Review PR");
        assert_eq!(action.kind, Kind::OpenPr);
        for kind in [
            Kind::Pull,
            Kind::Push,
            Kind::ForcePush,
            Kind::Merge,
            Kind::Reopen,
        ] {
            assert!(step_args(&kind, &review).is_err());
        }
        let normal = repo("work", clean(2), None);
        let action = next_action(&[review, normal], &[]);
        assert_eq!(action.kind, Kind::CreatePr);
        assert_eq!(action.repos, ["work"]);
    }

    fn repo(name: &str, l: Local, pr: Option<Pr>) -> Repo {
        Repo {
            name: name.into(),
            base: "origin/main".into(),
            branch: "feat".into(),
            local: Ok(l),
            pr: Some(Ok(pr)),
            parent: None,
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
            review_checkout: false,
            number: 1,
            state: PrState::Open,
            draft: false,
            merge_state: merge_state.into(),
            review: String::new(),
            checks,
            head: "h".into(),
            base: "main".into(),
            change_requests: vec![],
            comments: vec![],
            contains_head: String::new(),
        }
    }

    fn next(repos: &[Repo]) -> Action {
        next_action(repos, &[])
    }

    fn one(l: Local, p: Option<Pr>) -> Action {
        next(&[repo("a", l, p)])
    }

    /// Each repo is held to its own branch, and the prompt names that one.
    #[test]
    fn per_repo_branch() {
        let own = |l: Local| Repo {
            branch: "kd/x".into(),
            ..repo("b", l, None)
        };
        let on_x = Local {
            branch: Some("kd/x".into()),
            ..clean(1)
        };
        let a = next(&[repo("a", clean(1), None), own(on_x)]);
        assert!(!a.label.starts_with("Switch"), "{}", a.label);
        let elsewhere = Local {
            branch: Some("feat".into()),
            ..clean(1)
        };
        let a = next(&[repo("a", clean(1), None), own(elsewhere)]);
        assert_eq!(a.label, "Switch to branch · b");
        let Kind::Agent(prompt) = a.kind else {
            panic!("{:?}", a.kind)
        };
        assert!(
            prompt.contains("`kd/x`") && !prompt.contains("`feat`"),
            "{prompt}"
        );
        // Both off their branches: one instruction per branch, each naming its own.
        let off = || Local {
            branch: Some("other".into()),
            ..clean(1)
        };
        let a = next(&[repo("a", off(), None), own(off())]);
        let Kind::Agent(prompt) = a.kind else {
            panic!("{:?}", a.kind)
        };
        let (fa, kb) = prompt.split_once("\n\n").unwrap();
        assert!(
            fa.contains(": a.") && fa.contains("`feat`") && !fa.contains("kd/x"),
            "{fa}"
        );
        assert!(
            kb.contains(": b.") && kb.contains("`kd/x`") && !kb.contains("feat"),
            "{kb}"
        );
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
            branch: "feat".into(),
            local: Err("gone".into()),
            pr: None,
            parent: None,
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
        let ask = |p: Pr, handled: &[String]| next_action(&[repo("a", clean(1), Some(p))], handled);
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
        let ask = |h: &[String]| next_action(&[repo("a", clean(1), Some(p.clone()))], h);
        let a = ask(&[]);
        assert_eq!(
            (a.label.as_str(), a.reviews),
            ("Address comments", vec!["c1".to_string()])
        );
        assert_eq!(ask(&["c1".to_string()]).kind, Kind::Merge);
    }

    #[test]
    fn step_guards() {
        let args = |k: Kind, l: Local, p: Option<Pr>| step_args(&k, &repo("a", l, p));
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

    #[test]
    fn merge_refuses_a_pr_whose_base_has_not_been_retargeted() {
        let own = Pr {
            base: "old-parent".into(),
            ..pr("CLEAN", Checks::Passing)
        };
        let mut r = repo("api", clean(1), Some(own));
        let error = step_args(&Kind::Merge, &r).unwrap_err();
        assert!(
            error.contains("old-parent") && error.contains("main"),
            "{error}"
        );
        r.base = "origin/old-parent".into();
        assert!(step_args(&Kind::Merge, &r).unwrap().is_some());
    }

    /// The real thing: a linked worktree from `create_workspace` against a bare origin.
    /// `ios` stacked on `logic` (trunk `origin/main`), clean with one commit of its own.
    fn stacked(own: Option<Pr>, parent: Option<Pr>, l: impl FnOnce(&mut Local)) -> Repo {
        let mut local = Local {
            trunk: "origin/main".into(),
            ..clean(1)
        };
        l(&mut local);
        Repo {
            name: "ios".into(),
            base: "origin/logic".into(),
            parent,
            ..repo("ios", local, own)
        }
    }

    fn parent(state: PrState) -> Pr {
        Pr {
            number: 5,
            state,
            head: "p".into(),
            base: "main".into(),
            ..pr("CLEAN", Checks::Passing)
        }
    }

    /// The trunk, a review checkout and a branch off the trunk are never stacked, whatever a
    /// stale parent PR says.
    #[test]
    fn only_origin_bases_off_the_trunk_are_stacked() {
        let r = stacked(None, Some(parent(PrState::Merged)), |_| {});
        assert_eq!(r.stacked(), Some("logic"));
        assert!(r.parent_merged());
        for base in ["origin/main", "refs/shark/pull/7/base"] {
            let r = Repo {
                base: base.into(),
                ..r.clone()
            };
            assert_eq!(r.stacked(), None, "{base}");
            assert!(!r.parent_merged());
            assert_ne!(next(&[r]).kind, Kind::Restack);
        }
        let unknown = stacked(None, None, |l| l.trunk.clear());
        assert_eq!(unknown.stacked(), None, "trunk not probed yet");
    }

    /// A merged parent comes before any PR step, names the parent's base and only the stacked
    /// repo; uncommitted work still comes first, and a child that landed has nothing to move.
    #[test]
    fn parent_merged_restacks_first() {
        let merged = Some(parent(PrState::Merged));
        let a = next(&[stacked(None, merged.clone(), |_| {})]);
        assert_eq!(
            (a.label.as_str(), &a.kind, a.attention),
            ("Parent merged · rebase onto main", &Kind::Restack, true)
        );
        let mid = Pr {
            base: "core".into(),
            ..parent(PrState::Merged)
        };
        let a = next(&[stacked(None, Some(mid), |_| {})]);
        assert_eq!(
            a.label, "Parent merged · rebase onto core",
            "onto the grandparent"
        );
        let backend = repo("api", clean(1), Some(pr("CLEAN", Checks::Passing)));
        let a = next(&[
            backend.clone(),
            stacked(Some(pr("CLEAN", Checks::Passing)), merged.clone(), |_| {}),
        ]);
        assert_eq!(
            (a.label.as_str(), a.repos.clone()),
            (
                "Parent merged · rebase onto main · ios",
                vec!["ios".to_string()]
            )
        );
        let dirty = stacked(None, merged.clone(), |l| l.changed = 2);
        assert_eq!(next(&[dirty]).label, "Commit and push");
        let landed = Pr {
            state: PrState::Merged,
            ..pr("CLEAN", Checks::Passing)
        };
        let r = stacked(Some(landed), merged, |_| {});
        assert!(r.landed() && !r.parent_merged());
        assert_eq!(next(&[r]).kind, Kind::Archive);
    }

    #[test]
    fn parent_closed_needs_a_human() {
        let a = next(&[stacked(None, Some(parent(PrState::Closed)), |_| {})]);
        assert_eq!(
            (a.label.as_str(), &a.kind, a.attention),
            ("Parent PR closed", &Kind::None, true)
        );
    }

    /// A rewritten or squash-merged parent left its old commits in HEAD: restack what follows the
    /// recorded tip onto the base, before opening or pushing a PR.
    #[test]
    fn pending_restack_rebases_after_the_tip() {
        let r = stacked(None, Some(parent(PrState::Open)), |l| {
            l.restack = true;
            l.parent_tip = "t1".into();
        });
        let a = next(&[r]);
        assert_eq!(a.label, "Restack onto logic");
        let Kind::Agent(prompt) = &a.kind else {
            panic!("{a:?}")
        };
        assert!(
            prompt.contains("ios: onto `origin/logic`, after `t1`"),
            "{prompt}"
        );
        assert!(
            prompt.contains("git rebase --onto <onto> <after>") && prompt.contains("`feat`"),
            "{prompt}"
        );
        // After set_base moved it to the trunk the restack is still owed.
        let moved = Repo {
            base: "origin/main".into(),
            ..stacked(None, None, |l| {
                l.restack = true;
                l.parent_tip = "t1".into();
            })
        };
        assert_eq!(next(&[moved]).label, "Restack onto main");
    }

    #[test]
    fn restack_line_prefers_the_recorded_tip() {
        let tip = stacked(None, Some(parent(PrState::Merged)), |l| {
            l.parent_tip = "t1".into()
        });
        assert_eq!(
            restack_line(&tip, "origin/main"),
            "ios: onto `origin/main`, after `t1`"
        );
        let merged = stacked(None, Some(parent(PrState::Merged)), |_| {});
        assert_eq!(
            restack_line(&merged, "origin/main"),
            "ios: onto `origin/main`, after `p` (the parent PR's last commit; `git fetch origin pull/5/head` if it's missing)"
        );
        let open = stacked(None, Some(parent(PrState::Open)), |_| {});
        assert_eq!(
            restack_line(&open, "origin/logic"),
            "ios: onto `origin/logic`, after `$(git merge-base HEAD origin/logic)`"
        );
    }

    /// A child merges after its parent, from the button and from the menu alike; the other repo
    /// of the workspace merges meanwhile.
    #[test]
    fn child_waits_for_its_parent_to_merge() {
        let own = Some(pr("CLEAN", Checks::Passing));
        let ios = stacked(own.clone(), Some(parent(PrState::Open)), |_| {});
        let a = next(std::slice::from_ref(&ios));
        assert_eq!(
            (a.label.as_str(), &a.kind),
            ("Waiting on parent PR #5", &Kind::OpenPr)
        );
        for (state, reason) in [
            (PrState::Open, "must merge first"),
            (PrState::Merged, "restack onto its base first"),
            (PrState::Closed, "choose a new base first"),
        ] {
            let r = stacked(own.clone(), Some(parent(state)), |_| {});
            let e = step_args(&Kind::Merge, &r).unwrap_err();
            assert!(e.contains("stacked on #5") && e.contains(reason), "{e}");
        }
        let api = repo("api", clean(1), own.clone());
        let a = next(&[api, ios]);
        assert_eq!(
            (a.label.as_str(), a.repos),
            ("Merge · api", vec!["api".to_string()])
        );
        let free = stacked(own, Some(parent(PrState::Merged)), |l| l.base = (1, 0));
        assert!(
            step_args(
                &Kind::Merge,
                &Repo {
                    base: "origin/main".into(),
                    ..free
                }
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn parent_boundary_requires_verified_base_and_integrated_history() {
        use std::fs;
        let wt = std::env::temp_dir().join(format!("shark-parent-guard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&wt);
        fs::create_dir_all(&wt).unwrap();
        git(&wt, &["init", "-q", "-b", "feature"]).unwrap();
        let c = |args: &[&str]| {
            git(
                &wt,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        let commit = |message| {
            c(&["commit", "-qm", message, "--allow-empty"]);
            git(&wt, &["rev-parse", "HEAD"]).unwrap()
        };
        let main = commit("main");
        git(&wt, &["update-ref", "refs/remotes/origin/main", &main]).unwrap();
        git(
            &wt,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .unwrap();
        let parent = commit("parent");
        let child = commit("child");
        git(&wt, &["update-ref", "refs/remotes/origin/parent", &child]).unwrap();
        let key = crate::git::parent_tip_key("feature");
        git(&wt, &["config", &key, &parent]).unwrap();

        for base in ["origin/main", "origin/parent"] {
            let local = probe(&wt, "feature", base, None, false).unwrap();
            assert_eq!(
                local.parent_tip, parent,
                "unverified base {base} must not change the boundary"
            );
            assert_eq!(git(&wt, &["config", &key]).unwrap(), parent);
        }

        let tree = git(&wt, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let rewritten = c(&["commit-tree", &tree, "-p", &main, "-m", "rewritten parent"]);
        git(
            &wt,
            &["update-ref", "refs/remotes/origin/parent", &rewritten],
        )
        .unwrap();
        let local = probe(&wt, "feature", "origin/parent", None, false).unwrap();
        assert!(
            local.restack,
            "read-only probes still detect rewritten parents"
        );
        assert_eq!(local.parent_tip, parent);
        assert_eq!(git(&wt, &["config", &key]).unwrap(), parent);

        git(&wt, &["update-ref", "refs/remotes/origin/parent", &child]).unwrap();
        let local = probe(&wt, "feature", "origin/parent", None, true).unwrap();
        assert_eq!(
            local.parent_tip, child,
            "verified bases can advance the boundary"
        );
        assert_eq!(git(&wt, &["config", &key]).unwrap(), child);

        // A retarget can arrive before main's squash merge is fetched.
        git(&wt, &["config", &key, &parent]).unwrap();
        let local = probe(&wt, "feature", "origin/main", None, true).unwrap();
        assert_eq!(local.base.1, 0, "the stale trunk is already in HEAD");
        assert!(local.restack);
        assert_eq!(local.parent_tip, parent);
        assert_eq!(git(&wt, &["config", &key]).unwrap(), parent);
        let read_only = probe(&wt, "feature", "origin/main", Some(&local), false).unwrap();
        assert!(
            read_only.restack,
            "read-only polls must retain the same restack verdict"
        );
        assert_eq!(read_only.parent_tip, parent);
        assert_eq!(git(&wt, &["config", &key]).unwrap(), parent);
        let squash = c(&["commit-tree", &tree, "-p", &main, "-m", "squashed parent"]);
        git(&wt, &["update-ref", "refs/remotes/origin/main", &squash]).unwrap();
        let local = probe(&wt, "feature", "origin/main", None, true).unwrap();
        assert!(local.restack);
        assert_eq!(local.parent_tip, parent);
        assert_eq!(git(&wt, &["config", &key]).unwrap(), parent);

        c(&["rebase", "-q", "--onto", "origin/main", &parent]);
        let local = probe(&wt, "feature", "origin/main", None, true).unwrap();
        assert!(local.parent_tip.is_empty());
        assert!(git(&wt, &["config", &key]).is_err());

        let missing = "0123456789012345678901234567890123456789";
        git(&wt, &["config", &key, missing]).unwrap();
        let local = probe(&wt, "feature", "origin/main", None, true).unwrap();
        assert_eq!(
            local.parent_tip, missing,
            "missing history is not evidence of a rebase"
        );
        assert_eq!(git(&wt, &["config", &key]).unwrap(), missing);
        fs::remove_dir_all(wt).unwrap();
    }

    /// GitHub's Update branch would merge the parent in; a stack rebases onto it instead.
    #[test]
    fn behind_parent_rebases_instead_of_merging() {
        let ios = stacked(
            Some(pr("BEHIND", Checks::Passing)),
            Some(parent(PrState::Open)),
            |_| {},
        );
        let a = next(&[ios]);
        assert_eq!(a.label, "Rebase onto logic");
        assert!(
            matches!(&a.kind, Kind::Agent(p) if p.contains("onto `origin/logic`")),
            "{a:?}"
        );
        let plain = one(clean(1), Some(pr("BEHIND", Checks::Passing)));
        assert_eq!(plain.kind, Kind::UpdateBranch);
    }

    #[test]
    fn parses_base_ref() {
        let v = serde_json::json!({"url":"u","number":4,"state":"MERGED","headRefOid":"abc","baseRefName":"logic"});
        let p = parse_pr(&v);
        assert_eq!(
            (p.base.as_str(), p.head.as_str(), p.state),
            ("logic", "abc", PrState::Merged)
        );
    }

    /// Real git: the recorded parent tip follows the base while HEAD holds all of it, a missing
    /// base still counts the branch's own commits, and after a squash merge the restack command
    /// the prompt gives leaves exactly the child's own commit.
    #[test]
    fn parent_tip_tracks_and_restacks_after_a_squash() {
        use crate::git::{create_workspace_with_prs, git};
        use std::fs;
        let root = std::env::temp_dir().join(format!("shark-stack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (origin, src) = (root.join("origin.git"), root.join("src/ios"));
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
        let edit = |dir: &Path, file: &str, msg: &str| {
            fs::write(dir.join(file), format!("{msg}\n")).unwrap();
            c(dir, &["add", "."]);
            c(dir, &["commit", "-qm", msg]);
            git(dir, &["rev-parse", "HEAD"]).unwrap()
        };
        git(&src, &["init", "-q", "-b", "main"]).unwrap();
        edit(&src, "README.md", "init");
        git(
            &src,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )
        .unwrap();
        git(&src, &["push", "-q", "origin", "main"]).unwrap();
        git(&src, &["remote", "set-head", "origin", "main"]).unwrap();
        c(&src, &["checkout", "-qb", "logic"]);
        edit(&src, "a.txt", "a");
        let b = edit(&src, "b.txt", "b");
        git(&src, &["push", "-q", "origin", "logic"]).unwrap();
        c(&src, &["checkout", "-q", "main"]);
        let repo = crate::git::add_repo(&src, &[]).unwrap();
        let on = crate::git::on_branch(&[repo], "ui");
        let parents = [("ios".to_string(), "logic".to_string())];
        let ws =
            create_workspace_with_prs(&root.join("ws/ui"), "ui", "ui", &on, &[], &parents).unwrap();
        let wt = root.join("ws/ui/ios");
        assert_eq!(ws.repos[0].base, "origin/logic");
        let own = edit(&wt, "c.txt", "c");
        let p = |base: &str| probe(&wt, "ui", base, None, true).unwrap();
        let l = p("origin/logic");
        assert_eq!(
            (l.trunk.as_str(), l.parent_tip.as_str(), l.base, l.restack),
            ("origin/main", b.as_str(), (1, 0), false)
        );

        // The parent moves on: behind, nothing to restack, the tip stays until HEAD has it.
        c(&src, &["checkout", "-q", "logic"]);
        let d = edit(&src, "d.txt", "d");
        git(&src, &["push", "-q", "origin", "logic"]).unwrap();
        c(&src, &["checkout", "-q", "main"]);
        git(&wt, &["fetch", "-q", "origin"]).unwrap();
        let l = p("origin/logic");
        assert_eq!(
            (l.base, l.restack, l.parent_tip.as_str()),
            ((1, 1), false, b.as_str())
        );
        c(&wt, &["rebase", "-q", "origin/logic"]);
        let l = p("origin/logic");
        assert_eq!((l.base, l.parent_tip.as_str()), ((1, 0), d.as_str()));
        let key = crate::git::parent_tip_key("ui");
        assert_eq!(
            git(&src, &["config", &key]).unwrap(),
            d,
            "kept across restarts"
        );

        // Squash-merged and the branch deleted: the pruned base falls back to the local parent,
        // then the trunk, never (0, 0).
        c(&src, &["merge", "-q", "--squash", "logic"]);
        c(&src, &["commit", "-qm", "logic (#5)"]);
        git(&src, &["push", "-q", "origin", "main", ":logic"]).unwrap();
        git(&wt, &["fetch", "-q", "--prune", "origin"]).unwrap();
        let l = p("origin/logic");
        assert_eq!(
            (l.base.0, l.restack),
            (1, false),
            "against the local parent"
        );
        git(&src, &["branch", "-q", "-D", "logic"]).unwrap();
        assert!(p("origin/logic").base.0 > 0, "against the trunk");

        // Moved to the trunk, the restack is still owed; the prompt's command settles it.
        let l = p("origin/main");
        assert!(l.restack && l.parent_tip == d, "{l:?}");
        c(&wt, &["rebase", "-q", "--onto", "origin/main", &d]);
        let log = git(&wt, &["log", "--format=%s", "origin/main..HEAD"]).unwrap();
        assert_eq!(log, "c", "only the child's own commit");
        assert_ne!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), own);
        let l = p("origin/main");
        assert_eq!(
            (l.base, l.restack, l.parent_tip.as_str()),
            ((1, 0), false, "")
        );
        assert!(
            git(&src, &["config", &key]).is_err(),
            "nothing left to track"
        );
        // No comparison ref remains: do not let archive mistake unknown counts for no work.
        git(&src, &["update-ref", "-d", "refs/remotes/origin/main"]).unwrap();
        let error = probe(&wt, "ui", "origin/logic", None, true).unwrap_err();
        assert!(error.contains("Can't compare HEAD"), "{error}");

        // A genuinely unborn branch still has no commits of its own.
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        git(&empty, &["init", "-q", "-b", "main"]).unwrap();
        let unborn = probe(&empty, "main", "origin/main", None, true).unwrap();
        assert_eq!(unborn.head, "(initial)");
        assert_eq!(unborn.base, (0, 0));
        let _ = fs::remove_dir_all(&root);
    }

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
        let on = crate::git::on_branch(&[repo], "feat");
        let ws = create_workspace(&root.join("ws/feat"), "feat", "feat", &on).unwrap();
        let wt = root.join("ws/feat/api");
        let base = ws.repos[0].base.clone();
        let p = || probe(&wt, "feat", &base, None, true).unwrap();

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
            branch: "feat".into(),
            local: Ok(l),
            pr: Some(Ok(None)),
            parent: None,
        };
        assert_eq!(next_action(&[r(p())], &[]).kind, Kind::CreatePr);

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
            probe(&wt, "feat", &base, Some(&fake), true).unwrap().remote,
            Some((7, 7)),
            "unmoved refs reuse the last counts"
        );
        let force = step_args(&Kind::ForcePush, &r(l.clone())).unwrap().unwrap();
        assert_eq!(next_action(&[r(l)], &[]).kind, Kind::ForcePush);
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
        assert_eq!(next_action(&[r(l)], &[]).label, "Resolve conflicts");
        fs::remove_dir_all(&root).unwrap();
    }
}
