//! The gpui-free half of the git button: probing the repos in the background and running a step
//! on them. `GitSync` (mod.rs) polls, renders and calls into this.
use super::status::{
    self, Action, Kind, Local, Pr, PrState, Repo, diverged, fetch_pr, in_scope, next_action, probe,
};
use crate::git::cmd_bounded;
use crate::pr;
use crate::proc::par_map;
use crate::state::Workspace;
use std::path::PathBuf;

/// Everything a background probe needs, detached from the entity.
#[derive(Clone)]
pub(super) struct Job {
    /// `(worktree, base, branch)` per repo.
    pub(super) repos: Vec<(PathBuf, String, String)>,
    pub(super) fetch: bool,
    pub(super) pr: bool,
    /// gh is down and its backoff hasn't run out: neither `pr` nor `fetch` asks GitHub.
    pub(super) backoff: bool,
    /// The last answer per repo, including its PR target between GitHub polls.
    pub(super) prev: Vec<Repo>,
    /// With `fetch`: `(repo, source)` whose open PRs to list, for the stack around the branch.
    pub(super) stack: Vec<(String, PathBuf)>,
}

impl Job {
    /// Every repo of `ws`, asking GitHub, not fetching.
    pub(super) fn prs(ws: &Workspace) -> Job {
        Job {
            repos: worktrees(ws),
            fetch: false,
            pr: true,
            backoff: false,
            prev: vec![],
            stack: vec![],
        }
    }
}

pub(super) struct Probed {
    pub(super) repos: Vec<(PathBuf, String, String)>,
    pub(super) locals: Vec<Result<Local, String>>,
    pub(super) prs: Option<Vec<Result<Option<Pr>, String>>>,
    /// Per repo, a stacked one's parent PR; `None` when not asked.
    pub(super) parents: Vec<Option<Result<Option<Pr>, String>>>,
    /// Per `job.stack` repo, its open PRs; `None` when gh failed.
    pub(super) stack_prs: Vec<(String, Option<Vec<crate::prs::Pr>>)>,
    pub(super) fetched: bool,
    pub(super) fetch_error: Option<String>,
}

/// Repos run in parallel, so one hung remote costs one timeout, not one per repo.
pub(super) fn run_job(job: &Job) -> Probed {
    let ask_pr = (job.pr || job.fetch) && !job.backoff;
    let results = par_map(job.repos.iter(), |(wt, saved_base, branch)| {
        let previous = job.prev.iter().find(|r| {
            wt.file_name().is_some_and(|name| name == r.name.as_str()) && r.branch == *branch
        });
        let prev_local = previous.and_then(|r| r.local.as_ref().ok());
        // A preliminary probe must not rewrite the saved stack boundary against an old base.
        let head = crate::git::git(wt, &["rev-parse", "--verify", "HEAD"]).unwrap_or_default();
        let mut fetch_err = job
            .fetch
            .then(|| cmd_bounded(wt, "git", &["fetch", "--prune", "--quiet", "origin"], 60).err())
            .flatten();
        let mut pr = ask_pr.then(|| fetch_pr(wt, branch, &head));
        let review_url = crate::git::review_pr_url(wt, branch);
        if review_url.is_some()
            && let Some(Ok(Some(pr))) = &mut pr
        {
            pr.review_checkout = true;
        }
        let previous = previous.filter(|r| {
            r.local
                .as_ref()
                .is_ok_and(|l| l.review == review_url.is_some())
        });
        let selected = fresh_or_last(pr.as_ref(), &previous.and_then(|r| r.pr.clone()));
        let authoritative = matches!(&pr, Some(Ok(_)));
        let known_base = matches!(&selected, Some(Ok(_)));
        let selected = selected
            .as_ref()
            .and_then(|p| p.as_ref().ok())
            .and_then(|p| p.as_ref());
        let review = review_url.is_some() || selected.is_some_and(|p| p.review_checkout);
        let own_base = selected
            .filter(|p| {
                !review
                    && (p.state == PrState::Open || saved_base.starts_with("refs/shark/pull/"))
                    && !p.base.is_empty()
            })
            .map(|p| p.base.as_str());
        let base = own_base
            .map(|b| format!("origin/{b}"))
            .unwrap_or_else(|| saved_base.clone());
        if job.fetch {
            let refresh = if let Some(base) = own_base {
                let source = format!("refs/heads/{base}");
                let target = format!("+{source}:refs/remotes/origin/{base}");
                let head = format!("refs/heads/{branch}");
                let remote_head = format!("+{head}:refs/remotes/origin/{branch}");
                let fetch_head =
                    selected.is_some_and(|p| p.state == PrState::Open) && branch != base;
                crate::git::git(wt, &["check-ref-format", &source]).and_then(|_| {
                    if fetch_head {
                        crate::git::git(wt, &["check-ref-format", &head])?;
                    }
                    let mut args = vec!["fetch", "--quiet", "--no-tags", "origin", &target];
                    if fetch_head {
                        args.push(&remote_head);
                    }
                    cmd_bounded(wt, "git", &args, 30).map(|_| ())
                })
            } else if let Some(Ok(Some(p))) = &pr {
                if p.review_checkout
                    && p.state == PrState::Open
                    && review_url.as_ref() == Some(&p.url)
                {
                    crate::git::refresh_review_base(
                        wt,
                        &crate::git::PullRequest {
                            number: p.number,
                            url: p.url.clone(),
                            base: p.base.clone(),
                        },
                    )
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            };
            if let Err(e) = refresh {
                fetch_err = Some(match fetch_err {
                    Some(previous) => format!("{previous}\n{e}"),
                    None => e,
                });
            }
        }
        // A pruned stack parent must still be checked so its merged/closed state can guide recovery.
        let trunk = status::trunk_of(wt, prev_local);
        let parent = crate::stack::parent_of(&base, &trunk)
            .filter(|_| ask_pr && !review && !trunk.is_empty())
            .map(|p| status::fetch_parent(wt, p));
        let known_parent = match &parent {
            Some(Ok(parent)) => parent.as_ref(),
            None => previous
                .filter(|r| r.base == base)
                .and_then(|r| r.parent.as_ref()),
            Some(Err(_)) => None,
        };
        let recovering_parent =
            known_parent.is_some_and(|p| !p.review_checkout && p.state != PrState::Open);
        // Only a known retired parent permits the existing fallback used by restacking.
        let target = own_base
            .filter(|_| !recovering_parent)
            .map(|b| format!("refs/remotes/origin/{b}^{{commit}}"));
        let verified = target.as_ref().map_or(Ok(String::new()), |target| {
            crate::git::ref_oid(wt, target).ok_or_else(|| {
                format!("PR base `{base}` is unavailable locally; fetch its target first")
            })
        });
        let local = verified.and_then(|_| {
            probe(
                wt,
                branch,
                &base,
                prev_local,
                authoritative && job.fetch && fetch_err.is_none() && !review,
            )
            .map(|mut l| {
                l.review |= review;
                l.restack &= known_base;
                l
            })
        });
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
                crate::prs::fetch_repo_prs(name, src, true).ok(),
            )
        }),
        false => vec![],
    };
    Probed {
        repos: job.repos.clone(),
        locals,
        prs: ask_pr.then_some(prs),
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
        prev: last.to_vec(),
        ..Job::prs(ws)
    });
    background_answer(ws, p.locals, p.prs, p.parents, last, reviewed)
}

pub(super) fn background_answer(
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
pub(super) fn needs_you(a: &Action) -> bool {
    a.attention || !a.reviews.is_empty() || a.kind == Kind::Merge
}

pub(super) fn repo_diff_base(repo: &Repo) -> Option<String> {
    // Review checkouts have private refs from the target repository, which may not be origin.
    repo.pr()
        .filter(|p| {
            (p.state == PrState::Open || repo.base.starts_with("refs/shark/pull/"))
                && !p.base.is_empty()
                && !repo.is_review()
                && repo.local.is_ok()
        })
        .map(|pr| format!("origin/{}", pr.base))
}

pub(super) fn check_review_checkout(repos: &[Repo], rs: &[String]) -> Result<(), String> {
    if let Some(r) = repos.iter().find(|r| in_scope(rs, r) && r.is_review()) {
        return Err(format!(
            "{}: this is a local PR review checkout; use the original PR on GitHub to manage it",
            r.name
        ));
    }
    Ok(())
}

/// [`status::step_args`] against the workspace's saved base: [`repos_from`] follows an open PR's
/// base for display, which would hide a retargeted PR from the merge guard.
pub(super) fn planned_args(
    ws: &Workspace,
    kind: &Kind,
    r: &Repo,
) -> Result<Option<Vec<String>>, String> {
    let saved = ws.repos.iter().find(|w| w.name == r.name);
    let base = saved.map_or(&r.base, |w| &w.base).clone();
    status::step_args(kind, &Repo { base, ..r.clone() })
}

/// One repo's part of a step: `Ok(None)` when it has nothing to do.
type Step<'a> = dyn Fn(&std::path::Path, &Repo) -> Result<Option<String>, String> + 'a;

/// Runs a step on the target repos. Blocking; called on a background thread.
pub(super) fn execute(
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
    let wanted =
        |r: &Repo| in_scope(&a.repos, r) || (primary && a.kind.creates_pr() && !r.is_review());
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
    let planned = |r: &Repo| planned_args(ws, &a.kind, r);
    match &a.kind {
        Kind::Pull
        | Kind::Push
        | Kind::ForcePush
        | Kind::Reopen
        | Kind::UpdateBranch
        | Kind::Ready => each(&|wt, r| {
            let Some(args) = planned(r)? else {
                return Ok(None);
            };
            let (prog, secs) = match a.kind {
                Kind::Pull | Kind::Push | Kind::ForcePush => ("git", 120),
                _ => ("gh", 60),
            };
            cmd_bounded(wt, prog, &argv(&args), secs).map_err(|e| match a.kind {
                Kind::Pull if diverged(&e) => {
                    format!("{e}\nThe branches diverged; use “Sync with remote”.")
                }
                Kind::Push => format!("{e}\nIf the remote moved, pull first."),
                _ => e,
            })?;
            let n = r.pr().map_or(0, |p| p.number);
            Ok(Some(match a.kind {
                Kind::Pull => "pulled".into(),
                Kind::Push => "pushed".into(),
                Kind::ForcePush => "force-pushed".into(),
                Kind::Reopen => format!("#{n} reopened"),
                Kind::UpdateBranch => format!("#{n} updated from base"),
                _ => format!("#{n} ready for review"),
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
            let (prs, warning) = pr::create_linked_prs(&selected, &dir, &landed, new_pr)?;
            let mut lines: Vec<String> = prs.iter().map(|(r, u)| format!("{r}: {u}")).collect();
            lines.extend(warning);
            Ok(lines.join("\n"))
        }
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
pub(super) fn worktrees(ws: &Workspace) -> Vec<(PathBuf, String, String)> {
    let dir = ws.dir();
    ws.repos
        .iter()
        .map(|r| (dir.join(&r.name), r.base.clone(), ws.branch_of(r).into()))
        .collect()
}

/// Assemble probe results identically for polling, background attention and execution.
pub(super) fn repos_from(
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
            let same_identity = old.filter(|old| match (&old.local, &local) {
                (Ok(old), Ok(local)) => old.review == local.review,
                _ => false,
            });
            let pr = fresh_or_last(pr.as_ref(), &same_identity.and_then(|r| r.pr.clone()));
            let mut repo = Repo {
                local,
                pr,
                ..Repo::fresh(r, ws)
            };
            if let Some(base) = repo_diff_base(&repo) {
                repo.base = base;
            }
            repo.parent = match parent {
                Some(Ok(parent)) => parent,
                _ => old
                    .filter(|old| old.base == repo.base)
                    .and_then(|r| r.parent.clone()),
            };
            repo
        })
        .collect()
}

/// A merge needs a fresh parent answer; cached display state must never authorize it.
pub(super) fn execution_repos(
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
pub(super) fn fresh_or_last(
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

/// One whole step off the UI thread: re-probe (never trusting the snapshot in `job.prev`), with
/// `check` confirm `a` is still the next step, then run it. `Ok` is the step and its report.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_step(
    a: Action,
    ws: &Workspace,
    job: Job,
    handled: &[String],
    order: &[String],
    new_pr: &pr::NewPr,
    delete_remote: bool,
    check: bool,
) -> Result<(Action, String), String> {
    // Fresh state, never the snapshot: the decision and the act must agree.
    let fresh = run_job(&job);
    let repos = execution_repos(ws, fresh, &job.prev, &a)?;
    if check {
        let now = next_action(&repos, handled);
        if now.kind != a.kind || now.repos != a.repos {
            return Err(format!(
                "State changed; the next step is now “{}”",
                now.label
            ));
        }
    }
    execute(&a, ws, &repos, order, new_pr, delete_remote, check).map(|msg| (a, msg))
}
