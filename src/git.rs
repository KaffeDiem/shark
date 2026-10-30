use crate::state::{Repo, Workspace, WsRepo};
mod changes;
mod copy;
use copy::{copy_included, untracked_fingerprint};
#[cfg(test)]
mod tests;
pub use changes::{discard_file, uncommitted_files};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub fn run(dir: &Path, prog: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(prog)
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| crate::proc::spawn_err(dir, prog, e))
}

pub(crate) fn finish(prog: &str, args: &[&str], out: Output) -> Result<String, String> {
    finish_bytes(prog, args, out).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

fn finish_bytes(prog: &str, args: &[&str], out: Output) -> Result<Vec<u8>, String> {
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "{prog} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Like `git`, for any `prog` making network calls: never prompts for credentials, and is killed
/// after `secs` so a hung fetch or gh can't stall whoever waits on it.
pub fn cmd_bounded(dir: &Path, prog: &str, args: &[&str], secs: u64) -> Result<String, String> {
    let mut c = Command::new(if prog == "gh" {
        crate::proc::gh()
    } else {
        prog.into()
    });
    // GIT_TERMINAL_PROMPT doesn't reach ssh, which asks for host keys and passphrases on
    // /dev/tty itself. Only when the user hasn't picked their own ssh command.
    if prog == "git"
        && std::env::var_os("GIT_SSH_COMMAND").is_none()
        && std::env::var_os("GIT_SSH").is_none()
        && git(dir, &["config", "core.sshCommand"]).is_err()
    {
        c.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=15",
        );
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1");
    crate::proc::wait_bounded(c, dir, prog, args, secs, false)
}

/// Runs git, returning trimmed stdout, or stderr as the error on non-zero exit.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    finish("git", args, run(dir, "git", args)?)
}

/// Runs git, returning stdout untouched (no trim, any bytes), or stderr as the error.
pub fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    finish_bytes("git", args, run(dir, "git", args)?)
}

/// Commits with a fixed identity; `extra` is the rest of the `commit` args. Returns git's output.
#[cfg(test)]
pub fn commit(dir: &Path, extra: &[&str]) -> String {
    let args = [
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q"][..],
        extra,
    ]
    .concat();
    git(dir, &args).unwrap()
}

/// Uncommitted changes; `Err` when `git status` fails.
pub fn dirty(wt: &Path) -> Result<bool, String> {
    git(wt, &["status", "--porcelain"]).map(|s| !s.is_empty())
}

/// `git worktree list --porcelain`: each worktree's path and its other lines.
fn worktree_list(src: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for line in git(src, &["worktree", "list", "--porcelain"])?.lines() {
        match (line.strip_prefix("worktree "), out.last_mut()) {
            (Some(p), _) => out.push((p.into(), vec![])),
            (None, Some((_, attrs))) => attrs.push(line.into()),
            (None, None) => {}
        }
    }
    Ok(out)
}

pub fn add_repo(path: &Path, existing: &[Repo]) -> Result<Repo, String> {
    let root = git(path, &["rev-parse", "--show-toplevel"]).map_err(|e| {
        match !path.is_dir() || e.contains("not a git repository") {
            true => format!("Not a git repository: {}", path.display()),
            false => e,
        }
    })?;
    let root = PathBuf::from(root);
    let name = root.file_name().unwrap().to_string_lossy().to_string();
    if existing.iter().any(|r| r.name == name) {
        return Err(format!("a repo named `{name}` is already added"));
    }
    Ok(Repo { name, path: root })
}

/// The trunk to compare against, or empty when unknown.
pub(crate) fn default_base(src: &Path) -> String {
    git(
        src,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    // `fetch --prune` after a remote rename leaves origin/HEAD pointing at a deleted ref.
    .and_then(|b| match has_ref(src, &format!("refs/remotes/{b}")) {
        true => Ok(b),
        false => Err(String::new()),
    })
    // Only clone sets origin/HEAD; `git init` + `remote add` leaves it unset.
    .or_else(|_| {
        ["origin/main", "origin/master"]
            .into_iter()
            .find(|b| has_ref(src, &format!("refs/remotes/{b}")))
            .map(String::from)
            .ok_or_else(String::new)
    })
    // Empty when unknown: the current branch is no trunk (in a worktree it is the workspace's own).
    .unwrap_or_default()
}

/// `branch` is the workspace's own; each repo comes with the branch it gets, usually the same.
#[cfg(test)]
pub fn create_workspace(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[(Repo, String)],
) -> Result<Workspace, String> {
    create_workspace_with_prs(dir, name, branch, repos, &[], &[])
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub base: String,
}

/// PR selections use fresh local review branches, preserving colleagues' branches. `parents`
/// `(repo, branch)` stack a repo's new branch on another branch: it starts there and targets it.
pub fn create_workspace_with_prs(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[(Repo, String)],
    prs: &[(String, PullRequest)],
    parents: &[(String, String)],
) -> Result<Workspace, String> {
    if repos.is_empty() {
        return Err("pick at least one repo".into());
    }
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    for b in std::iter::once(branch).chain(repos.iter().map(|(_, b)| b.as_str())) {
        git(&repos[0].0.path, &["check-ref-format", "--branch", b])?;
    }
    // Every repo is checked (and fetched) before any worktree is added, so a refusal in one
    // repo leaves nothing behind in the others.
    let plans = crate::proc::par_map(repos, |(r, b)| {
        match prs.iter().find(|(name, _)| name == &r.name) {
            Some((_, pr)) => plan_pr_worktree(b, r, pr),
            None => {
                let parent = parents.iter().find(|(n, _)| n == &r.name);
                plan_worktree(b, r, parent.map(|(_, p)| p.as_str()))
            }
        }
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    fs::create_dir_all(dir.parent().ok_or("Workspace has no parent")?)
        .map_err(|e| e.to_string())?;
    fs::create_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let root_identity = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    let attempts = crate::proc::par_map(repos.iter().zip(&plans), |((r, b), p)| {
        let mut owned = OwnedWorktree::default();
        let result = add_worktree(dir, b, r, p, &mut owned).map(|w| WsRepo {
            branch: (b != branch).then(|| b.clone()),
            ..w
        });
        (result, owned)
    });
    let result = (|| {
        let done = attempts
            .iter()
            .map(|(result, _)| result.clone())
            .collect::<Result<Vec<_>, _>>()?;
        let mut ws = Workspace {
            name: name.into(),
            branch: branch.into(),
            repos: done,
            ..Default::default()
        };
        ws.migrate();
        write_root_docs(&ws, dir)?;
        Ok(ws)
    })();
    if let Err(error) = result {
        let mut remaining = Vec::new();
        for ((r, b), (_, owned)) in repos.iter().zip(&attempts) {
            remaining.extend(undo_worktree(dir, b, r, owned));
        }
        if !fs::symlink_metadata(dir)
            .is_ok_and(|m| m.dev() == root_identity.dev() && m.ino() == root_identity.ino())
            || fs::remove_dir(dir).is_err()
        {
            remaining.push(dir.display().to_string());
        }
        return Err(rollback_error(error, remaining));
    }
    result
}

/// Every repo on `branch`, for `create_workspace`.
#[cfg(test)]
pub fn on_branch(repos: &[Repo], branch: &str) -> Vec<(Repo, String)> {
    repos.iter().map(|r| (r.clone(), branch.into())).collect()
}

/// Adds one more repo to an existing workspace on its branch and rewrites the root CLAUDE.md.
/// Blocking: it fetches.
pub fn add_to_workspace(ws: &Workspace, dir: &Path, r: &Repo) -> Result<Workspace, String> {
    if ws.repos.iter().any(|w| w.name == r.name) {
        return Err(format!("{} is already in {}", r.name, ws.name));
    }
    if dir.join(&r.name).exists() {
        return Err(format!("{} already exists", dir.join(&r.name).display()));
    }
    let plan = plan_worktree(&ws.branch, r, None)?;
    let mut ws = ws.clone();
    let mut owned = OwnedWorktree::default();
    let added = add_worktree(dir, &ws.branch, r, &plan, &mut owned).and_then(|wr| {
        ws.repos.push(wr);
        write_root_docs(&ws, dir)
    });
    if let Err(e) = added {
        return Err(rollback_error(e, undo_worktree(dir, &ws.branch, r, &owned)));
    }
    Ok(ws)
}

/// A branch of a source repo, as the New workspace dialog lists it.
#[derive(Clone, Debug)]
pub struct Branch {
    pub name: String,
    pub local: bool,
    pub origin: bool,
    /// The worktree that has it checked out, the source checkout included.
    pub checked_out: Option<String>,
}

/// The worktree with `branch` checked out. A deleted worktree directory (prunable) no longer
/// holds its branch.
fn holder<'a>(wts: &'a [(String, Vec<String>)], branch: &str) -> Option<&'a str> {
    let head = format!("branch refs/heads/{branch}");
    (wts.iter())
        .find(|(_, attrs)| {
            attrs.contains(&head) && !attrs.iter().any(|l| l.starts_with("prunable"))
        })
        .map(|(p, _)| p.as_str())
}

/// `src`'s local and origin branches, most recent commit first, as of the last fetch.
pub fn branches(src: &Path) -> Result<Vec<Branch>, String> {
    let wts = worktree_list(src)?;
    let refs = git(
        src,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    )?;
    let mut out: Vec<Branch> = vec![];
    let mut seen = std::collections::HashMap::new();
    for rf in refs.lines() {
        let (name, local) = match rf.strip_prefix("refs/heads/") {
            Some(n) => (n, true),
            None => (rf.trim_start_matches("refs/remotes/origin/"), false),
        };
        if !local && name == "HEAD" {
            continue;
        }
        if let Some(&i) = seen.get(name) {
            let b: &mut Branch = &mut out[i];
            b.local |= local;
            b.origin |= !local;
            continue;
        }
        seen.insert(name, out.len());
        out.push(Branch {
            name: name.into(),
            local,
            origin: !local,
            checked_out: holder(&wts, name).map(String::from),
        });
    }
    Ok(out)
}

/// NUL-separated `git ls-files -z` output; avoids git's quoting of unusual filenames.
pub fn ls_files(dir: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = git_bytes(dir, &[&["ls-files", "-z"], args].concat())?;
    Ok(out
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect())
}

/// `origin/*` branches of `dir` as `(commit time, origin/name)`, newest first.
pub fn remote_branches(dir: &Path) -> Vec<(i64, String)> {
    let fmt = "--format=%(committerdate:unix) %(refname:short)";
    let out = git(
        dir,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            fmt,
            "refs/remotes/origin",
        ],
    );
    (out.unwrap_or_default().lines())
        .filter_map(|l| {
            let (t, r) = l.split_once(' ')?;
            Some((t.parse().ok()?, r.to_string()))
        })
        // `origin/HEAD` shortens to `origin`.
        .filter(|(_, r)| r.starts_with("origin/") && r != "origin/HEAD")
        .collect()
}

/// A repository, its own branch, and its timestamped remote branches.
pub type RepoBranches = (String, String, Vec<(i64, String)>);

/// The bases to offer for `repos` (`(repo, its own branch, remote_branches)`): every branch any
/// of them has, newest first, with the repos that have it; a repo's own branch left out.
pub fn base_choices(repos: &[RepoBranches]) -> Vec<(String, Vec<String>)> {
    let mut all: Vec<(i64, &str, &str)> = (repos.iter())
        .flat_map(|(repo, own, bs)| {
            (bs.iter())
                .filter(move |(_, b)| b.strip_prefix("origin/") != Some(own))
                .map(move |(t, b)| (*t, b.as_str(), repo.as_str()))
        })
        .collect();
    all.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for (_, b, repo) in all {
        match out.iter_mut().find(|(o, _)| o == b) {
            Some((_, rs)) => rs.push(repo.to_string()),
            None => out.push((b.to_string(), vec![repo.to_string()])),
        }
    }
    out
}

/// Whether `rf` resolves in `dir`.
pub(crate) fn has_ref(dir: &Path, rf: &str) -> bool {
    ref_oid(dir, rf).is_some()
}

/// The commit `rf` resolves to in `dir`, if any.
pub(crate) fn ref_oid(dir: &Path, rf: &str) -> Option<String> {
    git(dir, &["rev-parse", "--verify", "--quiet", rf]).ok()
}

struct Plan {
    base: String,
    pr_url: Option<String>,
    /// Where a new branch starts: `origin/<branch>` when only origin has it, the base when
    /// nobody does. None reuses the local branch: origin has it too, it holds unpushed work,
    /// or origin was unreachable.
    from: Option<String>,
    /// Stacked: the parent commit it starts on.
    tip: Option<String>,
}

impl Plan {
    fn new(base: String, from: Option<String>) -> Self {
        Plan {
            base,
            from,
            pr_url: None,
            tip: None,
        }
    }
}

/// Checks a repo can take the branch and decides where it starts. Fetches, so it blocks.
/// With a `parent` branch, a new branch starts on it and the base is `origin/<parent>`.
fn plan_worktree(branch: &str, r: &Repo, parent: Option<&str>) -> Result<Plan, String> {
    // A workspace dir deleted by hand leaves a worktree entry that still claims its branch.
    let _ = git(&r.path, &["worktree", "prune"]);
    if let Some(path) = holder(&worktree_list(&r.path)?, branch) {
        return Err(format!(
            "{}: branch `{branch}` is already checked out at {path}",
            r.name
        ));
    }
    // Best effort, may be offline or have no origin. --prune: a stale origin/<branch> would
    // make a merged, deleted branch look current.
    let fetched = cmd_bounded(
        &r.path,
        "git",
        &["fetch", "--quiet", "--prune", "origin"],
        30,
    )
    .is_ok();
    let (local, remote) = (
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
    );
    let (has_local, has_remote) = (has_ref(&r.path, &local), has_ref(&r.path, &remote));
    if let Some(p) = parent {
        // An existing branch has its own history, not the parent's.
        if has_local || has_remote {
            return Err(format!(
                "{}: `{branch}` already exists; stacking on `{p}` needs a new branch name",
                r.name
            ));
        }
        return plan_stacked(r, p);
    }
    let base = Some(default_base(&r.path))
        .filter(|b| !b.is_empty())
        .or_else(|| git(&r.path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok())
        .unwrap_or_else(|| "HEAD".into());
    let from = match (has_local, has_remote) {
        (true, true) => {
            // Behind origin with nothing of its own: catch up first. Nothing has it checked out
            // (see above). Diverged keeps local; the git button offers "Sync with remote".
            if let Some(old) = ref_oid(&r.path, &local)
                && git(&r.path, &["merge-base", "--is-ancestor", &local, &remote]).is_ok()
            {
                let _ = git(&r.path, &["update-ref", &local, &remote, &old]);
            }
            None
        }
        // Already in the base: left over from an archived workspace whose PR merged and lost
        // its remote branch; checking it out would restart on old, merged code. Anything else
        // is unpushed work and is reused. Only refused when origin answered: offline or
        // without origin, a local branch is all there is.
        (true, false)
            if fetched && git(&r.path, &["merge-base", "--is-ancestor", &local, &base]).is_ok() =>
        {
            return Err(format!(
                "{}: a local branch `{branch}` exists that origin doesn't have and is already merged into {base} (left from an archived workspace?). Pick another name, or rename it: git -C {} branch -m {branch} {branch}-old",
                r.name,
                r.path.display()
            ));
        }
        (true, false) => None,
        (false, true) => Some(format!("origin/{branch}")),
        (false, false) => Some(base.clone()),
    };
    Ok(Plan::new(base, from))
}

/// A new branch stacked on `p`: it starts on `origin/<p>`, which is also its base.
fn plan_stacked(r: &Repo, p: &str) -> Result<Plan, String> {
    let from = parent_start(r, p)?;
    let tip = git(&r.path, &["rev-parse", &from])?;
    Ok(Plan {
        tip: Some(tip),
        ..Plan::new(format!("origin/{p}"), Some(from))
    })
}

/// Where a branch stacked on `p` starts: `origin/<p>`, what its PR will be measured against.
/// Unpushed commits of a local `p` would land in the child's PR, so `p` must be pushed first.
fn parent_start(r: &Repo, p: &str) -> Result<String, String> {
    let remote = format!("refs/remotes/origin/{p}");
    match (
        has_ref(&r.path, &remote),
        has_ref(&r.path, &format!("refs/heads/{p}")),
    ) {
        (true, _) => Ok(format!("origin/{p}")),
        (false, true) => Err(format!(
            "{}: `{p}` isn't on GitHub yet; push it first (from its workspace)",
            r.name
        )),
        (false, false) => Err(format!(
            "{}: `{p}` isn't a branch here (a PR from a fork can't be stacked on)",
            r.name
        )),
    }
}

/// The git config key holding the parent commit `branch` is built on: its commits after it are
/// its own. Shark keeps it current (`status::probe`); restacks rebase what follows it.
pub fn parent_tip_key(branch: &str) -> String {
    format!("branch.{branch}.shark-parent-tip")
}

/// A branch-local identity survives restarts without confusing same-named fork branches.
pub fn review_pr_url(src: &Path, branch: &str) -> Option<String> {
    git(
        src,
        &["config", "--get", &format!("branch.{branch}.shark-pr-url")],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

fn pr_remote(src: &Path, pr: &PullRequest) -> Result<String, String> {
    if pr.base.is_empty() {
        return Err(
            "Pull request base is missing; refresh pull requests and select it again".into(),
        );
    }
    let t = crate::pr::git_feature::Target::from_url(&pr.url)
        .ok()
        .filter(|t| t.number == pr.number)
        .ok_or("Invalid pull request URL")?;
    git(
        src,
        &["check-ref-format", &format!("refs/heads/{}", pr.base)],
    )?;
    let (host, repo) = (&t.host, format!("{}/{}", t.owner, t.repo));
    let remote = format!("https://{host}/{repo}.git");
    let origin = git(src, &["remote", "get-url", "origin"]).unwrap_or_default();
    // GitHub owners and repo names are case-insensitive.
    let origin = origin
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase();
    let matching_origin = [
        format!("https://{host}/{repo}"),
        format!("git@{host}:{repo}"),
        format!("ssh://git@{host}/{repo}"),
    ]
    .iter()
    .any(|c| c.to_ascii_lowercase() == origin);
    Ok(if matching_origin {
        "origin".into()
    } else {
        remote
    })
}

/// Refresh only the review comparison ref, from the PR's target repository (possibly not origin).
pub(crate) fn refresh_review_base(src: &Path, pr: &PullRequest) -> Result<(), String> {
    let remote = pr_remote(src, pr)?;
    cmd_bounded(
        src,
        "git",
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            &remote,
            &format!("+refs/heads/{}:refs/shark/pull/{}/base", pr.base, pr.number),
        ],
        30,
    )?;
    Ok(())
}

fn plan_pr_worktree(branch: &str, r: &Repo, pr: &PullRequest) -> Result<Plan, String> {
    if has_ref(&r.path, &format!("refs/heads/{branch}")) {
        return Err(format!(
            "{}: review branch `{branch}` already exists; pick another branch name",
            r.name
        ));
    }
    let remote = pr_remote(&r.path, pr)?;
    let head = format!("refs/shark/pull/{}/head", pr.number);
    let base = format!("refs/shark/pull/{}/base", pr.number);
    cmd_bounded(
        &r.path,
        "git",
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            &remote,
            &format!("+refs/pull/{}/head:{head}", pr.number),
            &format!("+refs/heads/{}:{base}", pr.base),
        ],
        30,
    )?;
    let from = git(
        &r.path,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    Ok(Plan {
        pr_url: Some(pr.url.clone()),
        ..Plan::new(base, Some(from))
    })
}

#[derive(Default)]
struct OwnedWorktree {
    branch: Option<String>,
    worktree: Option<(u64, u64, String)>,
    untracked: Option<(u64, Vec<String>)>,
}

fn add_worktree(
    dir: &Path,
    branch: &str,
    r: &Repo,
    plan: &Plan,
    owned: &mut OwnedWorktree,
) -> Result<WsRepo, String> {
    let wt = dir.join(&r.name);
    if let Some(from) = &plan.from {
        let commit = git(
            &r.path,
            &["rev-parse", "--verify", &format!("{from}^{{commit}}")],
        )?;
        git(&r.path, &["branch", "--no-track", branch, &commit])?;
        owned.branch = Some(commit);
    }
    let head = match &owned.branch {
        Some(commit) => commit.clone(),
        None => git(
            &r.path,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        )?,
    };
    git(&r.path, &["worktree", "add", &wt.to_string_lossy(), branch])?;
    let metadata = fs::symlink_metadata(&wt).map_err(|e| e.to_string())?;
    owned.worktree = Some((metadata.dev(), metadata.ino(), head));
    if let Some(tip) = &plan.tip {
        git(&r.path, &["config", &parent_tip_key(branch), tip])?;
    }
    if let Some(url) = &plan.pr_url {
        git(
            &r.path,
            &["config", &format!("branch.{branch}.shark-pr-url"), url],
        )?;
    }
    let copied = copy_included(&r.path, &wt)?;
    owned.untracked = untracked_fingerprint(&wt, &copied)
        .ok()
        .map(|fp| (fp, copied));
    Ok(WsRepo {
        name: r.name.clone(),
        src: r.path.clone(),
        base: plan.base.clone(),
        branch: None,
    })
}

fn rollback_error(error: String, remaining: Vec<String>) -> String {
    if remaining.is_empty() {
        error
    } else {
        format!("{error}\nPreserved resources: {}", remaining.join(", "))
    }
}

fn undo_worktree(dir: &Path, branch: &str, r: &Repo, owned: &OwnedWorktree) -> Vec<String> {
    let wt = dir.join(&r.name);
    let mut remaining = Vec::new();
    if let Some((dev, ino, head)) = &owned.worktree {
        let unchanged = fs::symlink_metadata(&wt).is_ok_and(|m| m.dev() == *dev && m.ino() == *ino)
            && git(&wt, &["rev-parse", "HEAD"]).is_ok_and(|tip| tip == *head)
            && owned
                .untracked
                .as_ref()
                .is_some_and(|(fp, copied)| untracked_fingerprint(&wt, copied).ok() == Some(*fp));
        if !unchanged || git(&r.path, &["worktree", "remove", &wt.to_string_lossy()]).is_err() {
            remaining.push(wt.display().to_string());
        }
    }
    if let Some(commit) = &owned.branch {
        let reference = format!("refs/heads/{branch}");
        let unchecked = worktree_list(&r.path).is_ok_and(|wts| holder(&wts, branch).is_none());
        if !unchecked || git(&r.path, &["update-ref", "-d", &reference, commit]).is_err() {
            remaining.push(format!("{}:{reference}", r.path.display()));
        } else if !has_ref(&r.path, &reference) {
            let _ = git(
                &r.path,
                &["config", "--remove-section", &format!("branch.{branch}")],
            );
        }
    }
    remaining
}

/// Rewrites the root CLAUDE.md and AGENTS.md from the workspace's repo list; a failure only
/// leaves them stale.
pub fn write_claude_md(ws: &Workspace) {
    let _ = write_root_docs(ws, &ws.dir());
}

/// The files the agent reads at the workspace root: CLAUDE.md for claude, AGENTS.md for codex.
const ROOT_DOCS: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

fn write_root_docs(ws: &Workspace, dir: &Path) -> Result<(), String> {
    let [claude, agents] = ROOT_DOCS;
    fs::write(dir.join(claude), claude_md(ws, dir))
        .and_then(|()| fs::write(dir.join(agents), agents_md(ws, dir)))
        .map_err(|e| e.to_string())
}

/// What Shark itself leaves in a workspace root: its docs, Finder litter, and the `.context`
/// folder of attachments.
fn root_owned() -> [&'static str; 4] {
    let [claude, agents] = ROOT_DOCS;
    [claude, agents, ".DS_Store", attachments_parts().0]
}

fn attachments_parts() -> (&'static str, &'static str) {
    crate::complete::ATTACHMENTS
        .split_once('/')
        .unwrap_or((crate::complete::ATTACHMENTS, ""))
}

/// Whether `name` in the root is Shark's, not the user's: `.context` only counts while it
/// holds nothing but attachments (unreadable, or a file, is unexpected: refuse before removing).
fn shark_owned(dir: &Path, name: &str) -> bool {
    let (context, attachments) = attachments_parts();
    root_owned().contains(&name)
        && (name != context
            || fs::read_dir(dir.join(name))
                .is_ok_and(|mut es| es.all(|e| e.is_ok_and(|e| e.file_name() == attachments))))
}

fn claude_md(ws: &Workspace, dir: &Path) -> String {
    let mut s = header(ws);
    for r in &ws.repos {
        if dir.join(&r.name).join("CLAUDE.md").exists() {
            s += &format!("@{}/CLAUDE.md\n", r.name);
        }
    }
    s
}

/// The rules file a repo has, AGENTS.md before CLAUDE.md.
pub fn rules_file(repo_dir: &Path) -> Option<&'static str> {
    ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .find(|f| repo_dir.join(f).exists())
}

/// Codex has no `@` imports and reads AGENTS.md only from here, so it is pointed at each repo's.
fn agents_md(ws: &Workspace, dir: &Path) -> String {
    let mut s = header(ws);
    for r in &ws.repos {
        if let Some(f) = rules_file(&dir.join(&r.name)) {
            s += &format!(
                "- Before working in `{0}/`, read `{0}/{f}`: its instructions apply there.\n",
                r.name
            );
        }
    }
    s
}

fn header(ws: &Workspace) -> String {
    let first = ws
        .repos
        .first()
        .map_or(ws.branch.as_str(), |r| ws.branch_of(r));
    let shared = ws.repos.iter().all(|r| ws.branch_of(r) == first);
    let mut s = format!(
        "# Workspace `{}`\n\nThis directory is NOT a git repo. Each subdirectory is a git worktree{}:\n",
        ws.name,
        match shared {
            true => format!(" on branch `{first}`"),
            false => String::new(),
        }
    );
    for r in &ws.repos {
        let on = match shared {
            true => String::new(),
            false => format!(" on `{}`", ws.branch_of(r)),
        };
        // A PR URL names the GitHub repo, which needn't match the folder.
        let gh = (git(&r.src, &["remote", "get-url", "origin"]).ok())
            .and_then(|o| crate::sync::github_url(&o))
            .map_or(String::new(), |u| format!(", GitHub {u}"));
        s += &format!(
            "- `{}/`: worktree of {}{on} (base `{}`{gh})\n",
            r.name,
            r.src.display(),
            r.base
        );
    }
    s += "\nRun git per repo (e.g. `git -C <repo> status`) and commit in each repo separately. Never run git in this root directory.\n\n";
    s
}

/// Why `git worktree remove` would refuse this worktree even when it's clean: `locked` (needs
/// `--force` twice; we never do that), or it has submodules (needs `--force`).
fn remove_blocker(src: &Path, wt: &Path, force: bool) -> Option<&'static str> {
    let canon = fs::canonicalize(wt).ok()?;
    let locked =
        |attrs: &[String]| (attrs.iter()).any(|l| l == "locked" || l.starts_with("locked "));
    if (worktree_list(src).unwrap_or_default().into_iter())
        .any(|(p, attrs)| locked(&attrs) && fs::canonicalize(p).is_ok_and(|p| p == canon))
    {
        return Some("locked (git worktree unlock)");
    }
    if force {
        return None;
    }
    // Populated submodules live in the worktree's own git dir, or (old style) in place.
    let gitdir = git(wt, &["rev-parse", "--absolute-git-dir"]).unwrap_or_default();
    let gitlink = |l: &str| {
        l.starts_with("160000 ")
            && l.split_once('\t')
                .is_some_and(|(_, p)| wt.join(p).join(".git").exists())
    };
    let staged = git(wt, &["ls-files", "--stage"]).unwrap_or_default();
    let absorbed = !gitdir.is_empty() && Path::new(&gitdir).join("modules").exists();
    (absorbed || staged.lines().any(gitlink))
        .then_some("has submodules (force archive removes them)")
}

/// The refusals of `remove_workspace`, without removing anything: callers run it before any
/// side effect (stopping servers, archive scripts).
pub fn archive_preflight(ws: &Workspace, dir: &Path, force: bool) -> Result<(), String> {
    // Up front, so a refusal in the second repo can't leave the first one removed.
    let blocked: Vec<String> = ws
        .repos
        .iter()
        .filter_map(|r| {
            let why = remove_blocker(&r.src, &dir.join(&r.name), force)?;
            Some(format!("{}: {why}", r.name))
        })
        .collect();
    if !blocked.is_empty() {
        return Err(blocked.join("\n"));
    }
    if !force {
        // Check everything first, so a dirty second repo can't leave the first one removed.
        let dirty: Vec<&str> = ws
            .repos
            .iter()
            .filter(|r| {
                let wt = dir.join(&r.name);
                wt.exists() && dirty(&wt).unwrap_or(true)
            })
            .map(|r| r.name.as_str())
            .collect();
        if !dirty.is_empty() {
            return Err(format!("uncommitted changes in: {}", dirty.join(", ")));
        }
        let extra: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !ws.repos.iter().any(|r| &r.name == n) && !shark_owned(dir, n))
            .collect();
        if !extra.is_empty() {
            return Err(format!(
                "{}: unexpected files: {}",
                dir.display(),
                extra.join(", ")
            ));
        }
    }
    Ok(())
}

pub fn remove_workspace(ws: &Workspace, dir: &Path, force: bool) -> Result<(), String> {
    archive_preflight(ws, dir, force)?;
    for r in &ws.repos {
        let wt = dir.join(&r.name);
        let wt_s = wt.to_string_lossy();
        if wt.exists() {
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&wt_s);
            // A moved or deleted source repo can't remove it; forced, the directory goes anyway.
            if let Err(e) = git(&r.src, &args) {
                match force {
                    true => {
                        fs::remove_dir_all(&wt).map_err(|e| format!("{}: {e}", wt.display()))?
                    }
                    false => return Err(e),
                }
            }
        }
        let _ = git(&r.src, &["worktree", "prune"]);
    }
    if force {
        let _ = fs::remove_dir_all(dir);
    } else {
        // Only our own files may remain; anything else the user must force away. Dropped and
        // pasted files go too; sent messages may still refer to them until now.
        for f in root_owned() {
            let p = dir.join(f);
            let _ = fs::remove_file(&p).or_else(|_| fs::remove_dir_all(&p));
        }
        fs::remove_dir(dir)
            .map_err(|e| format!("{}: {e} (unexpected files left)", dir.display()))?;
    }
    Ok(())
}

/// After an archive: deletes the workspace branch in each `(repo, head)` source repo, but only
/// while the branch still points at `head`, the commit that was found merged. `-D` because a
/// squash merge leaves the branch unmerged as far as git can tell. Returns the repos done.
pub fn delete_merged_branches(ws: &Workspace, merged: &[(String, String)]) -> Vec<String> {
    ws.repos
        .iter()
        .filter(|r| {
            let b = ws.branch_of(r);
            let at = format!("refs/heads/{b}");
            merged.iter().any(|(n, head)| {
                n == &r.name
                    && git(&r.src, &["rev-parse", "--verify", "--quiet", &at]).as_deref()
                        == Ok(head.as_str())
                    && git(&r.src, &["branch", "-D", b]).is_ok()
            })
        })
        .map(|r| r.name.clone())
        .collect()
}

/// The commit the diff of `repo_dir` compares against: HEAD when only uncommitted changes show,
/// else the merge-base with `base`. Empty when git fails.
pub fn compare_rev(repo_dir: &Path, base: &str, uncommitted_only: bool) -> String {
    let args = match uncommitted_only {
        true => vec!["rev-parse", "HEAD"],
        false => vec!["merge-base", base, "HEAD"],
    };
    git(repo_dir, &args).unwrap_or_default()
}

/// One unified patch for all repos. Every path is prefixed with the repo name.
/// A failed `git diff` (e.g. a missing base) is an error naming the repo, not an empty patch.
pub fn combined_diff(ws: &Workspace, dir: &Path, uncommitted_only: bool) -> Result<String, String> {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let mut out = String::new();
    for r in &ws.repos {
        let wt = dir.join(&r.name);
        // Untracked files join the same `git diff` through a throwaway index where they are
        // intent-to-add: one process instead of one per file, and the real index (and the
        // agent's index.lock) is never touched.
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("shark-{}-{seq}.index", std::process::id()));
        if let Ok(index) = git(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        ) {
            let _ = fs::copy(index, &tmp);
        }
        let git_tmp = |args: &[&str]| {
            Command::new("git")
                .current_dir(&wt)
                .env("GIT_INDEX_FILE", &tmp)
                .args(args)
                .output()
        };
        let _ = git_tmp(&["add", "--intent-to-add", "--ignore-errors", "."]);
        let src = format!("--src-prefix=a/{}/", r.name);
        let dst = format!("--dst-prefix=b/{}/", r.name);
        // ponytail: untracked files are diffed whole, add a size cap when big binaries bite
        let mut args = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            // Full hashes: abbreviations grow with the repo and would change viewed fingerprints.
            "--full-index",
            // Rename headers carry no --src/--dst prefix, so a renamed file would lose its repo.
            "--no-renames",
            &src,
            &dst,
        ];
        if uncommitted_only {
            args.push("HEAD");
        } else {
            args.extend(["--merge-base", &r.base]);
        }
        let o = git_tmp(&args);
        let _ = fs::remove_file(&tmp);
        let o = o.map_err(|e| format!("{}: git diff: {e}", r.name))?;
        if !o.status.success() {
            return Err(format!(
                "{}: git diff: {}",
                r.name,
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        out += &String::from_utf8_lossy(&o.stdout);
    }
    Ok(out)
}
