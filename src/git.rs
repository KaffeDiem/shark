use crate::proc::{finish, finish_bytes};
use crate::state::{AgentLink, Attachment, Repo, Task, TaskRepo};
mod changes;
mod copy;
pub mod history;
use copy::{copy_included, untracked_fingerprint};
#[cfg(test)]
mod tests;
pub use changes::{discard_file, uncommitted_files};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Mutex,
};

pub fn run(dir: &Path, prog: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(prog)
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| crate::proc::spawn_err(dir, prog, e))
}

/// Like `git`, for any `prog` making network calls: never prompts for credentials, and is killed
/// after `secs` so a hung fetch or gh can't stall whoever waits on it.
pub fn cmd_bounded(dir: &Path, prog: &str, args: &[&str], secs: u64) -> Result<String, String> {
    let mut c = Command::new(if prog == "gh" {
        crate::proc::gh()
    } else {
        prog.into()
    });
    if prog == "git" {
        no_ssh_prompt(&mut c, dir);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1");
    crate::proc::wait_bounded(c, dir, prog, args, secs, false)
}

/// GIT_TERMINAL_PROMPT doesn't reach ssh, which asks for host keys and passphrases on
/// /dev/tty itself. Only when the user hasn't picked their own ssh command.
fn no_ssh_prompt(c: &mut Command, dir: &Path) {
    if std::env::var_os("GIT_SSH_COMMAND").is_none()
        && std::env::var_os("GIT_SSH").is_none()
        && git(dir, &["config", "core.sshCommand"]).is_err()
    {
        c.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=15",
        );
    }
}

/// Runs git, returning trimmed stdout, or stderr as the error on non-zero exit.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    finish("git", args, run(dir, "git", args)?)
}

/// Runs git, returning stdout untouched (no trim, any bytes), or stderr as the error.
pub fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    finish_bytes("git", args, run(dir, "git", args)?)
}

/// Like `git_bytes`, but a hung process is killed after `secs`.
pub fn git_bytes_bounded(dir: &Path, args: &[&str], secs: u64) -> Result<Vec<u8>, String> {
    // A partial clone may fetch missing objects here.
    let mut c = Command::new("git");
    no_ssh_prompt(&mut c, dir);
    c.env("GIT_TERMINAL_PROMPT", "0");
    finish_bytes(
        "git",
        args,
        crate::proc::wait_output(c, dir, "git", args, secs, false)?,
    )
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
    // Explicit: `status.showUntrackedFiles=no` would hide files `reset --keep` can't protect.
    git(wt, &["status", "--porcelain", "--untracked-files=normal"]).map(|s| !s.is_empty())
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
    // Empty when unknown: the current branch is no trunk (in a worktree it is the task's own).
    .unwrap_or_default()
}

/// `branch` is the task's own; each repo comes with the branch it gets, usually the same.
#[cfg(test)]
pub fn create_task(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[(Repo, String)],
) -> Result<Task, String> {
    create_task_with_prs(dir, name, branch, repos, &[], &[], "", None, false)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub base: String,
}

/// PR selections use fresh local review branches, preserving colleagues' branches. `parents`
/// `(repo, branch)` stack a repo's new branch on another branch: it starts there and targets it.
/// Automatic task launches set `fresh` so retained branches cannot become a new task's work.
#[allow(clippy::too_many_arguments)]
pub fn create_task_with_prs(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[(Repo, String)],
    prs: &[(String, PullRequest)],
    parents: &[(String, String)],
    title: &str,
    agent: Option<AgentLink>,
    fresh: bool,
) -> Result<Task, String> {
    let fresh_repos: Vec<_> = repos
        .iter()
        .filter(|_| fresh)
        .map(|(r, _)| r.name.clone())
        .collect();
    create_task_with_sources(
        dir,
        name,
        branch,
        repos,
        prs,
        parents,
        title,
        agent,
        &fresh_repos,
    )
}

/// Drafts may mix new branches, existing branches and PR checkouts.
#[allow(clippy::too_many_arguments)]
pub fn create_task_with_sources(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[(Repo, String)],
    prs: &[(String, PullRequest)],
    parents: &[(String, String)],
    title: &str,
    agent: Option<AgentLink>,
    fresh_repos: &[String],
) -> Result<Task, String> {
    create_task_planned(
        dir,
        name,
        branch,
        repos,
        prs,
        parents,
        title,
        agent,
        fresh_repos,
        &[],
    )
}

/// A task Shark starts from a goal's plan row: a new branch in every repo, from the trunk, or for
/// `local_parents` `(repo, branch)` from that local branch's tip, pushed or not. The parent's
/// branch is its base: the diff compares against it and its PR targets it.
pub fn create_row_task(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[Repo],
    local_parents: &[(String, String)],
    title: &str,
    agent: Option<AgentLink>,
) -> Result<Task, String> {
    let fresh: Vec<String> = repos.iter().map(|r| r.name.clone()).collect();
    let repos: Vec<(Repo, String)> = repos.iter().map(|r| (r.clone(), branch.into())).collect();
    create_task_planned(
        dir,
        name,
        branch,
        &repos,
        &[],
        &[],
        title,
        agent,
        &fresh,
        local_parents,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_task_planned(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[(Repo, String)],
    prs: &[(String, PullRequest)],
    parents: &[(String, String)],
    title: &str,
    agent: Option<AgentLink>,
    fresh_repos: &[String],
    local_parents: &[(String, String)],
) -> Result<Task, String> {
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
            None if let Some((_, p)) = local_parents.iter().find(|(n, _)| n == &r.name) => {
                plan_local_child(b, r, p)
            }
            None if fresh_repos.contains(&r.name) => {
                let base = parents.iter().find(|(n, _)| n == &r.name);
                plan_fresh(b, r, base.map_or("", |(_, b)| b.as_str()))
            }
            None => {
                let parent = parents.iter().find(|(n, _)| n == &r.name);
                plan_worktree(b, r, parent.map(|(_, p)| p.as_str()))
            }
        }
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    fs::create_dir_all(dir.parent().ok_or("Task has no parent")?).map_err(|e| e.to_string())?;
    fs::create_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let root_identity = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    let attempts = crate::proc::par_map(repos.iter().zip(&plans), |((r, b), p)| {
        let mut owned = OwnedWorktree::default();
        let result = add_worktree(dir, b, r, p, &mut owned).map(|w| TaskRepo {
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
        let mut ws = Task {
            title: title.into(),
            agents: agent.into_iter().collect(),
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

/// An agent with no repositories yet: its directory, root docs and first chat, in Plan mode.
/// Nothing is left behind on failure.
pub fn create_empty_task(
    dir: &Path,
    name: &str,
    title: &str,
    agent: Option<AgentLink>,
) -> Result<Task, String> {
    fs::create_dir_all(dir.parent().ok_or("Task has no parent")?).map_err(|e| e.to_string())?;
    fs::create_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut ws = Task {
        agents: agent.into_iter().collect(),
        name: name.into(),
        title: title.into(),
        branch: name.into(),
        ..Default::default()
    };
    ws.migrate();
    ws.chats[0].plan = true;
    if let Err(e) = write_root_docs(&ws, dir) {
        let _ = fs::remove_dir_all(dir);
        return Err(e);
    }
    Ok(ws)
}

/// Every repo on `branch`, for `create_workspace`.
#[cfg(test)]
pub fn on_branch(repos: &[Repo], branch: &str) -> Vec<(Repo, String)> {
    repos.iter().map(|r| (r.clone(), branch.into())).collect()
}

/// A refused attachment can leave owned resources that rollback could not safely delete.
#[derive(Debug)]
pub struct AttachError {
    pub message: String,
    pub preserved: bool,
}

impl From<String> for AttachError {
    fn from(message: String) -> Self {
        Self {
            message,
            preserved: false,
        }
    }
}

/// Adds one more repo to an existing task on `branch` and rewrites the root CLAUDE.md; the
/// first repo's branch becomes the task's. With `fresh`, the base (empty: the repo's default)
/// of a branch that must be new; without, an existing branch is used as `plan_worktree` does.
/// Blocking: it fetches.
pub fn add_to_task(
    ws: &Task,
    dir: &Path,
    r: &Repo,
    branch: &str,
    fresh: Option<&str>,
) -> Result<Task, AttachError> {
    if ws.repos.iter().any(|w| w.name == r.name) {
        return Err(format!("{} is already in {}", r.name, ws.display_title()).into());
    }
    if dir.join(&r.name).exists() {
        return Err(format!("{} already exists", dir.join(&r.name).display()).into());
    }
    let plan = match fresh {
        Some(base) => plan_fresh(branch, r, base)?,
        None => plan_worktree(branch, r, None)?,
    };
    let before = ws;
    let mut ws = ws.clone();
    if ws.repos.is_empty() {
        ws.branch = branch.into();
    }
    let mut owned = OwnedWorktree::default();
    let added = add_worktree(dir, branch, r, &plan, &mut owned).and_then(|wr| {
        ws.repos.push(TaskRepo {
            branch: (branch != ws.branch).then(|| branch.into()),
            ..wr
        });
        write_root_docs(&ws, dir)
    });
    if let Err(e) = added {
        let _ = write_root_docs(before, dir); // one of the two may have gone out
        let remaining = undo_worktree(dir, branch, r, &owned);
        return Err(AttachError {
            preserved: !remaining.is_empty(),
            message: rollback_error(e, remaining),
        });
    }
    Ok(ws)
}

/// What an interrupted attachment left behind.
pub enum Found {
    Nothing,
    /// The worktree, on its branch: the work finished.
    Worktree(TaskRepo),
    /// Something that can't be told from someone else's; says what.
    Unclear(String),
}

/// Looks at `a`'s source repo, branch and path. Reads only.
pub fn find_attachment(ws: &Task, a: &Attachment) -> Found {
    let wt = ws.dir().join(&a.repo);
    let Ok(wts) = worktree_list(&a.src) else {
        return Found::Unclear(format!("{} can't be read", a.src.display()));
    };
    // The root docs list it only once it is whole: a `git worktree add` killed mid-checkout, or a
    // crash before the included files were copied, looks like a worktree on its branch.
    let [_, last] = ROOT_DOCS;
    let base = fs::read_to_string(ws.dir().join(last)).ok().and_then(|d| {
        let prefix = listed(&a.repo);
        let line = d.lines().find(|line| line.starts_with(&prefix))?;
        let (_, base) = line.rsplit_once(" (base `")?;
        Some(base.rsplit_once('`')?.0.to_string())
    });
    let here = fs::canonicalize(&wt).ok();
    let on_branch =
        holder(&wts, &a.branch).is_some_and(|p| here.is_some() && fs::canonicalize(p).ok() == here);
    if let Some(base) = base.filter(|_| on_branch) {
        // A replaced .git can leave the source's worktree registration pointing at another repo.
        let common = |dir: &Path| {
            git(
                dir,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .ok()
            .and_then(|p| fs::canonicalize(p).ok())
        };
        let source = common(&a.src);
        if source.is_some()
            && common(&wt) == source
            && git(&wt, &["symbolic-ref", "HEAD"])
                .is_ok_and(|b| b == format!("refs/heads/{}", a.branch))
        {
            return Found::Worktree(TaskRepo {
                name: a.repo.clone(),
                src: a.src.clone(),
                base,
                branch: None,
            });
        }
    }
    let branch = a.fresh && has_ref(&a.src, &format!("refs/heads/{}", a.branch));
    match (wt.exists(), branch) {
        (false, false) => Found::Nothing,
        (false, true) => Found::Unclear(format!(
            "branch `{}` exists in {} with no worktree",
            a.branch,
            a.src.display()
        )),
        (true, _) => Found::Unclear(format!(
            "{} exists but isn't a finished worktree on `{}`",
            wt.display(),
            a.branch
        )),
    }
}

/// A branch of a source repo, as the New task dialog lists it.
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
    // A task dir deleted by hand leaves a worktree entry that still claims its branch.
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
    let base = resolve_base(&r.path, "");
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
        // Already in the base: left over from an archived task whose PR merged and lost
        // its remote branch; checking it out would restart on old, merged code. Anything else
        // is unpushed work and is reused. Only refused when origin answered: offline or
        // without origin, a local branch is all there is.
        (true, false)
            if fetched && git(&r.path, &["merge-base", "--is-ancestor", &local, &base]).is_ok() =>
        {
            return Err(format!(
                "{}: a local branch `{branch}` exists that origin doesn't have and is already merged into {base} (left from a deleted task?). Pick another name, or rename it: git -C {} branch -m {branch} {branch}-old",
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

/// `base` as a ref: origin's when origin has it. Empty is the repo's default.
pub fn resolve_base(src: &Path, base: &str) -> String {
    let base = base.trim();
    if base.is_empty() {
        return Some(default_base(src))
            .filter(|b| !b.is_empty())
            .or_else(|| git(src, &["rev-parse", "--abbrev-ref", "HEAD"]).ok())
            .unwrap_or_else(|| "HEAD".into());
    }
    match has_ref(src, &format!("refs/remotes/origin/{base}")) {
        true => format!("origin/{base}"),
        false => base.into(),
    }
}

/// A branch that must be new: one of that name here or on origin is refused.
fn refuse_existing(branch: &str, r: &Repo) -> Result<(), String> {
    let taken = has_ref(&r.path, &format!("refs/heads/{branch}"))
        || has_ref(&r.path, &format!("refs/remotes/origin/{branch}"));
    match taken {
        true => Err(format!(
            "{}: branch `{branch}` already exists; pick another name",
            r.name
        )),
        false => Ok(()),
    }
}

/// `plan_worktree` for a branch that must be new: `branch` existing here or on origin is refused
/// before anything is made. Fetches, so it blocks.
fn plan_fresh(branch: &str, r: &Repo, base: &str) -> Result<Plan, String> {
    git(&r.path, &["check-ref-format", "--branch", branch])
        .map_err(|_| format!("`{branch}` isn't a valid branch name"))?;
    let _ = git(&r.path, &["worktree", "prune"]);
    // Best effort, as in `plan_worktree`; offline, the last fetch's refs are checked.
    let args = ["fetch", "--quiet", "--prune", "origin"];
    let _ = cmd_bounded(&r.path, "git", &args, 30);
    refuse_existing(branch, r)?;
    let base = resolve_base(&r.path, base);
    let tip = git(
        &r.path,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )
    .map_err(|_| format!("{}: base `{base}` not found", r.name))?;
    Ok(Plan {
        tip: (base != resolve_base(&r.path, "")).then_some(tip),
        ..Plan::new(base.clone(), Some(base))
    })
}

/// A new branch on the tip of the local branch `p`, which origin may not have yet. `p` itself is
/// the base (`origin/<p>` could be missing or older), so the diff shows only this branch's work.
fn plan_local_child(branch: &str, r: &Repo, p: &str) -> Result<Plan, String> {
    let _ = git(&r.path, &["worktree", "prune"]);
    refuse_existing(branch, r)?;
    let tip = ref_oid(&r.path, &format!("refs/heads/{p}^{{commit}}"))
        .ok_or_else(|| format!("{}: `{p}` isn't a branch here", r.name))?;
    Ok(Plan {
        tip: Some(tip.clone()),
        ..Plan::new(p.into(), Some(tip))
    })
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
            "{}: `{p}` isn't on GitHub yet; push it first (from its task)",
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
    let t = crate::pr::Target::from_url(&pr.url)
        .ok()
        .filter(|t| t.number == pr.number)
        .ok_or("Invalid pull request URL")?;
    git(
        src,
        &["check-ref-format", &format!("refs/heads/{}", pr.base)],
    )?;
    Ok(if origin_is(src, &t) {
        "origin".into()
    } else {
        format!("https://{}/{}/{}.git", t.host, t.owner, t.repo)
    })
}

/// Whether `src`'s origin is the PR's repository, in its https, scp-style or ssh form. Both
/// the configured URL and its `insteadOf` expansion count: an alias expands to GitHub, and a
/// GitHub URL may be rewritten to a mirror.
pub fn origin_is(src: &Path, t: &crate::pr::Target) -> bool {
    let (host, repo) = (&t.host, format!("{}/{}", t.owner, t.repo));
    // GitHub owners and repo names are case-insensitive.
    let forms = [
        format!("https://{host}/{repo}"),
        format!("git@{host}:{repo}"),
        format!("ssh://git@{host}/{repo}"),
    ]
    .map(|f| f.to_ascii_lowercase());
    [
        ["config", "--get", "remote.origin.url"],
        ["remote", "get-url", "origin"],
    ]
    .iter()
    .filter_map(|args| git(src, args).ok())
    .any(|url| {
        let url = url.trim_end_matches('/').trim_end_matches(".git");
        forms.contains(&url.to_ascii_lowercase())
    })
}

/// A review checkout's private ref for PR `n`: `side` is `head` or `base`.
pub(crate) fn review_ref(n: u64, side: &str) -> String {
    format!("refs/shark/pull/{n}/{side}")
}

/// Fetches the PR's base ref, and with `head` its head ref, from the PR's target repository
/// (possibly not origin).
pub(crate) fn fetch_review_refs(src: &Path, pr: &PullRequest, head: bool) -> Result<(), String> {
    let remote = pr_remote(src, pr)?;
    let mut specs = vec![format!(
        "+refs/heads/{}:{}",
        pr.base,
        review_ref(pr.number, "base")
    )];
    if head {
        specs.push(format!(
            "+refs/pull/{}/head:{}",
            pr.number,
            review_ref(pr.number, "head")
        ));
    }
    // --atomic: if the base fails to update, the head must not advance alone.
    let mut args = vec!["fetch", "--quiet", "--no-tags", "--atomic", &remote];
    args.extend(specs.iter().map(String::as_str));
    cmd_bounded(src, "git", &args, 30)?;
    Ok(())
}

/// Completion lookups must not move an open review's comparison or update baseline.
pub(crate) fn fetch_composer_pr_head(src: &Path, pr: &PullRequest) -> Result<String, String> {
    let remote = pr_remote(src, pr)?;
    let reference = format!("refs/shark/composer/pull/{}/head", pr.number);
    let spec = format!("+refs/pull/{}/head:{reference}", pr.number);
    cmd_bounded(
        src,
        "git",
        &["fetch", "--quiet", "--no-tags", &remote, &spec],
        30,
    )?;
    Ok(reference)
}

/// Refresh only the review comparison ref; the head stays put so "Fetch updates" stays offered.
pub(crate) fn refresh_review_base(src: &Path, pr: &PullRequest) -> Result<(), String> {
    fetch_review_refs(src, pr, false)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

/// The merge, rebase, cherry-pick, revert or bisect under way in `wt`.
pub(crate) fn op_in_progress(wt: &Path) -> Result<Option<Op>, String> {
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
    Ok(paths
        .lines()
        .zip(OPS)
        .find(|(p, _)| Path::new(p).exists())
        .map(|(_, (_, op))| op))
}

/// Refuses unless `wt` is clean and on `branch` at `old`, the previously fetched PR head.
fn on_review_head(wt: &Path, branch: &str, old: &str) -> Result<(), String> {
    if dirty(wt)? {
        return Err("uncommitted changes; commit or discard them first".into());
    }
    if let Some(op) = op_in_progress(wt)? {
        let op = format!("{op:?}").to_lowercase();
        return Err(format!("a {op} is in progress; finish or abort it first"));
    }
    // Not --short: it prints `heads/<branch>` when a tag has the same name.
    if git(wt, &["symbolic-ref", "HEAD"]).is_ok_and(|b| b == format!("refs/heads/{branch}")) {
        if git(wt, &["rev-parse", "HEAD"])? == old {
            return Ok(());
        }
        return Err("has commits on top of the PR's previous head; nothing changed".into());
    }
    Err(format!("not on branch `{branch}`; nothing changed"))
}

/// Refuses if moving `wt` from `old` to `new` would overwrite an ignored file, which `dirty` and
/// `reset --keep` both miss: an ignored path at or under a path `new` adds, or an added path that
/// exists on disk inside an ignored directory.
fn no_ignored_clobber(wt: &Path, old: &str, new: &str) -> Result<(), String> {
    let args = [
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=A",
        "-z",
        old,
        new,
    ];
    let added = git_bytes(wt, &args)?;
    let ls = [
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ];
    let ignored = git_bytes(wt, &ls)?;
    let paths = |b: &[u8]| -> Vec<PathBuf> {
        b.split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
            .collect()
    };
    let ignored = paths(&ignored);
    for a in paths(&added) {
        // Collapsed ignored directories end in `/`; `Path` comparison drops it.
        let hit = ignored.iter().any(|i| {
            i.starts_with(&a) || (a.starts_with(i) && wt.join(&a).symlink_metadata().is_ok())
        });
        if hit {
            let a = a.display();
            return Err(format!(
                "would overwrite ignored file `{a}`; move it aside first"
            ));
        }
    }
    Ok(())
}

/// Moves review branch `branch` to the PR's new head. Refuses, before fetching, unless the tree
/// is clean and HEAD is the previously fetched PR head, so nothing local can be lost.
// ponytail: refs/shark/pull/N/head is shared by every review checkout of PR N; a second one
// misses the signal once the first fetches. Record the applied head per branch if that matters.
pub(crate) fn update_review_branch(
    wt: &Path,
    branch: &str,
    pr: &PullRequest,
) -> Result<Option<String>, String> {
    let head = review_ref(pr.number, "head");
    let old = ref_oid(wt, &format!("{head}^{{commit}}")).ok_or("no recorded PR head")?;
    on_review_head(wt, branch, &old)?;
    fetch_review_refs(wt, pr, true)?;
    // ponytail: GitHub's API can show a new headRefOid before refs/pull/N/head moves; then
    // this is "Nothing to do" and the marker stays until the next poll.
    let new = git(
        wt,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    if new == old {
        return Ok(None);
    }
    // Checked again: a commit or checkout during the fetch must not be reset away. A clean tree
    // on the old head: --keep loses nothing, and the old commits stay in the reflog.
    let moved = on_review_head(wt, branch, &old)
        .and_then(|_| no_ignored_clobber(wt, &old, &new))
        .and_then(|_| git(wt, &["reset", "--quiet", "--keep", &new]));
    if let Err(e) = moved {
        // Keep "Fetch updates" offered: the branch didn't move.
        return Err(match git(wt, &["update-ref", &head, &old]) {
            Ok(_) => e,
            Err(r) => format!(
                "{e}; also failed to restore {head}, so \"Fetch updates\" may not be offered again: {r}"
            ),
        });
    }
    Ok(Some(format!("{} → {}", &old[..7], &new[..7])))
}

fn plan_pr_worktree(branch: &str, r: &Repo, pr: &PullRequest) -> Result<Plan, String> {
    if has_ref(&r.path, &format!("refs/heads/{branch}")) {
        return Err(format!(
            "{}: review branch `{branch}` already exists; pick another branch name",
            r.name
        ));
    }
    fetch_review_refs(&r.path, pr, true)?;
    let (head, base) = (review_ref(pr.number, "head"), review_ref(pr.number, "base"));
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
) -> Result<TaskRepo, String> {
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
    Ok(TaskRepo {
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

/// Rewrites the root CLAUDE.md, AGENTS.md and skill links from the task snapshot.
pub fn write_claude_md(ws: &Task) -> Result<(), String> {
    write_root_docs(ws, &ws.dir())
}

/// The files the agent reads at the task root: CLAUDE.md for claude, AGENTS.md for codex.
const ROOT_DOCS: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

fn write_root_docs(ws: &Task, dir: &Path) -> Result<(), String> {
    write_instructions(ws, dir)?;
    link_skills(ws, dir)
}

pub(crate) fn write_instructions(ws: &Task, dir: &Path) -> Result<(), String> {
    let [claude, agents] = ROOT_DOCS;
    fs::write(dir.join(claude), claude_md(ws, dir))
        .and_then(|()| fs::write(dir.join(agents), agents_md(ws, dir)))
        .map_err(|e| e.to_string())
}

/// Whether agents get the repos' skills: `State::no_repo_skills`, here for the spawn paths.
static REPO_SKILLS: Mutex<bool> = Mutex::new(true);

pub fn repo_skills() -> bool {
    *REPO_SKILLS.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn set_repo_skills(on: bool) {
    *REPO_SKILLS.lock().unwrap_or_else(|e| e.into_inner()) = on;
}

/// `--add-dir` for each repo: claude loads a repo's `.claude/skills` and `.claude/commands` at
/// start only from its own cwd upward or from these. A clash shows as `/repo:skill`.
pub fn claude_skill_args(ws: &Task) -> Vec<String> {
    if !repo_skills() {
        return vec![];
    }
    (ws.repos.iter())
        .map(|r| ws.dir().join(&r.name))
        .filter(|p| p.exists())
        .flat_map(|p| ["--add-dir".into(), p.to_string_lossy().into_owned()])
        .collect()
}

/// Where each harness discovers skills in its working directory (the task root).
const HARNESS_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills"];

const SKILL_OWNERS: &str = ".shark-skills.json";

// Never follow a user's directory symlink while managing task-owned files.
fn skill_dirs_owned(dir: &Path, folder: &str) -> bool {
    let folder = Path::new(folder);
    [folder.parent().unwrap(), folder]
        .iter()
        .all(|p| match fs::symlink_metadata(dir.join(p)) {
            Ok(m) => m.is_dir(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        })
}

fn owned_skill_links(dir: &Path, folder: &str) -> Option<BTreeMap<String, PathBuf>> {
    let path = dir.join(folder).parent().unwrap().join(SKILL_OWNERS);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(BTreeMap::new()),
        Ok(m) if m.is_file() => {
            let links: BTreeMap<String, PathBuf> =
                serde_json::from_slice(&fs::read(path).ok()?).ok()?;
            links
                .keys()
                .all(|name| {
                    let mut parts = Path::new(name).components();
                    matches!(parts.next(), Some(std::path::Component::Normal(_)))
                        && parts.next().is_none()
                })
                .then_some(links)
        }
        _ => None,
    }
}

/// Link agent skills for every harness, and repo skills for Codex. Only recorded, unchanged
/// links belong to Shark; pre-existing links stay user-owned.
pub fn link_skills(ws: &Task, dir: &Path) -> Result<(), String> {
    // Keep the current preference and filesystem reconciliation under the same lock.
    let on = REPO_SKILLS.lock().unwrap_or_else(|e| e.into_inner());
    reconcile_skills(ws, dir, *on)
}

fn reconcile_skills(ws: &Task, dir: &Path, on: bool) -> Result<(), String> {
    for folder in HARNESS_SKILL_DIRS {
        reconcile_skill_folder(ws, dir, folder, on)
            .map_err(|e| format!("{}: {e}", dir.join(folder).display()))?;
    }
    Ok(())
}

pub(crate) fn skill_entries(dir: &Path) -> std::io::Result<Vec<fs::DirEntry>> {
    let read = || {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut found = vec![];
        for entry in entries {
            let entry = entry?;
            match fs::metadata(entry.path().join("SKILL.md")) {
                Ok(meta) if meta.is_file() => found.push(entry),
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        found.sort_by_key(|e| e.file_name());
        Ok(found)
    };
    read().map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", dir.display())))
}

/// The name used by the task's Codex skill links when skill names collide.
pub(crate) fn skill_link_name(skill: &str, prefix: &str, used: &mut HashSet<String>) -> String {
    let mut name = skill.to_string();
    if used.contains(&name) {
        name = format!("{prefix}-{skill}");
    }
    let base = name.clone();
    let mut suffix = 2;
    while used.contains(&name) {
        name = format!("{base}-{suffix}");
        suffix += 1;
    }
    used.insert(name.clone());
    name
}

fn reconcile_skill_folder(ws: &Task, dir: &Path, folder: &str, on: bool) -> std::io::Result<()> {
    if !skill_dirs_owned(dir, folder) {
        return Err(std::io::Error::other(
            "Skill directory is unreadable or not an owned directory",
        ));
    }
    let Some(mut owned) = owned_skill_links(dir, folder) else {
        return Err(std::io::Error::other(
            "Skill ownership manifest is unreadable or invalid",
        ));
    };
    let links = dir.join(folder);
    owned.retain(|name, target| fs::read_link(links.join(name)).is_ok_and(|t| &t == target));
    let mut used = HashSet::new();
    let mut user_targets = HashSet::new();
    match fs::read_dir(&links) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !owned.contains_key(&name) {
                    used.insert(name);
                    if let Ok(target) = fs::read_link(entry.path()) {
                        user_targets.insert(target);
                    }
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut want = BTreeMap::new();
    let mut add = |skill: String, target: PathBuf, prefix: &str| {
        if user_targets.contains(&target) {
            return;
        }
        let name = skill_link_name(&skill, prefix, &mut used);
        want.insert(name, target);
    };
    // Agent skills claim plain names before repo skills, even when repo skills are disabled. Every
    // agent its chats use is linked: discovery folders belong to the task, not to one chat.
    for agent in &ws.agents {
        for entry in skill_entries(&crate::state::agent_skills_dir(&agent.slug))? {
            add(
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
                &agent.slug,
            );
        }
    }
    for r in ws
        .repos
        .iter()
        .filter(|_| on && folder == HARNESS_SKILL_DIRS[0])
    {
        let mut own = HashSet::new();
        for src in HARNESS_SKILL_DIRS {
            for e in skill_entries(&dir.join(&r.name).join(src))? {
                let skill = e.file_name().to_string_lossy().into_owned();
                if !own.insert(skill.clone()) {
                    continue;
                }
                let target = Path::new("../..").join(&r.name).join(src).join(&skill);
                add(skill, target, &r.name);
            }
        }
    }
    for (name, target) in &owned {
        if want.get(name) != Some(target) {
            fs::remove_file(links.join(name))?;
        }
    }
    owned.retain(|name, target| want.get(name) == Some(target));
    let manifest = links.parent().unwrap().join(SKILL_OWNERS);
    if want.is_empty() && owned.is_empty() {
        match fs::remove_file(manifest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let _ = fs::remove_dir(&links).and_then(|()| fs::remove_dir(links.parent().unwrap()));
        return Ok(());
    }
    fs::create_dir_all(&links)?;
    // Record before creating links: a failed write must never leave untracked owned links.
    for (name, target) in &want {
        if !owned.contains_key(name) {
            owned.insert(name.clone(), target.clone());
        }
    }
    let bytes = serde_json::to_vec(&owned)?;
    crate::state::write(&manifest, &bytes)?;
    for (name, target) in want {
        let link = links.join(name);
        if !fs::read_link(&link).is_ok_and(|current| current == target) {
            std::os::unix::fs::symlink(target, link)?;
        }
    }
    Ok(())
}

/// What Shark itself leaves in a task root: its docs, Finder litter, the `.context` folder
/// (plans, scratch and attachments) and the skill links.
fn root_owned() -> impl Iterator<Item = &'static str> {
    let [claude, agents] = ROOT_DOCS;
    [claude, agents, ".DS_Store", attachments_parts().0]
        .into_iter()
        .chain(
            HARNESS_SKILL_DIRS
                .iter()
                .map(|p| p.split_once('/').unwrap().0),
        )
}

fn attachments_parts() -> (&'static str, &'static str) {
    crate::complete::ATTACHMENTS
        .split_once('/')
        .unwrap_or((crate::complete::ATTACHMENTS, ""))
}

/// Whether `name` in the root is Shark's, not the user's: `.context` is whatever it holds, and
/// harness folders count only while they hold nothing but skill links (unreadable, or a file,
/// is unexpected: refuse before removing).
fn shark_owned(dir: &Path, name: &str) -> bool {
    root_owned().any(|owned| owned == name)
        && HARNESS_SKILL_DIRS.iter().all(|folder| {
            folder.split_once('/').unwrap().0 != name || only_skill_links(dir, folder)
        })
}

fn only_skill_links(dir: &Path, folder: &str) -> bool {
    if !skill_dirs_owned(dir, folder) {
        return false;
    }
    let Some(owned) = owned_skill_links(dir, folder) else {
        return false;
    };
    let links = dir.join(folder);
    fs::read_dir(links.parent().unwrap()).is_ok_and(|mut es| {
        es.all(|e| e.is_ok_and(|e| e.file_name() == "skills" || e.file_name() == SKILL_OWNERS))
    }) && (!links.exists()
        || fs::read_dir(&links).is_ok_and(|mut es| {
            es.all(|e| {
                e.is_ok_and(|e| {
                    owned
                        .get(&e.file_name().to_string_lossy().into_owned())
                        .is_some_and(|target| fs::read_link(e.path()).is_ok_and(|t| &t == target))
                })
            })
        }))
}

fn claude_md(ws: &Task, dir: &Path) -> String {
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
fn agents_md(ws: &Task, dir: &Path) -> String {
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

/// How the root docs start `repo`'s line.
fn listed(repo: &str) -> String {
    format!("- `{repo}/`: worktree of ")
}

/// Where agents put what isn't code. Claude's plan files go there too (`plansDirectory`).
const CONTEXT_NOTE: &str = "Keep plans, notes, research and temporary files in `.context/` inside this directory: plans in `.context/plans/`, everything else in `.context/scratch/`. Never write them elsewhere, including your home folder. `.context` is deleted with the task and never committed. Files the user attaches are in `.context/attachments/`.\n\n";

fn header(ws: &Task) -> String {
    if ws.repos.is_empty() {
        format!(
            "# Task `{}`\n\nNo repositories are attached. This directory is NOT a git repo and holds no code: discuss and plan here. Building starts once the user attaches a repository.\n\n{CONTEXT_NOTE}",
            ws.name
        )
    } else {
        let first = ws
            .repos
            .first()
            .map_or(ws.branch.as_str(), |r| ws.branch_of(r));
        let shared = ws.repos.iter().all(|r| ws.branch_of(r) == first);
        let mut s = format!(
            "# Task `{}`\n\nThis directory is NOT a git repo. Each subdirectory is a git worktree{}:\n",
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
                .and_then(|o| crate::pr::github_url(&o))
                .map_or(String::new(), |u| format!(", GitHub {u}"));
            s += &format!(
                "{}{}{on} (base `{}`{gh})\n",
                listed(&r.name),
                r.src.display(),
                r.base
            );
        }
        s += "\nRun git per repo (e.g. `git -C <repo> status`) and commit in each repo separately. Never run git in this root directory.\n\nThis task is already an isolated worktree managed by Shark. Don't create more git worktrees (no `isolation: \"worktree\"`, `--worktree` or EnterWorktree); split parallel work by file instead.\n\n";
        s += CONTEXT_NOTE;
        s
    }
}

/// What a chat's agent adds to the harness's system prompt; root docs are shared by every chat.
pub fn agent_prompt(ws: &Task, chat: &crate::state::ChatMeta) -> Option<String> {
    let agent = ws.agent_link(chat.agent.as_deref()?)?;
    Some(format!(
        "## Agent: {}\n\n{}",
        agent.name, agent.instructions
    ))
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
pub fn archive_preflight(ws: &Task, dir: &Path, force: bool) -> Result<(), String> {
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

pub fn remove_task(ws: &Task, dir: &Path, force: bool) -> Result<(), String> {
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

/// After an archive: deletes the task branch in each `(repo, head)` source repo, but only
/// while the branch still points at `head`, the commit that was found merged. `-D` because a
/// squash merge leaves the branch unmerged as far as git can tell. Returns the repos done.
pub fn delete_merged_branches(ws: &Task, merged: &[(String, String)]) -> Vec<String> {
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

/// Deletes the branches of a removed goal's task that never got a commit: the lead only reads.
/// A leftover one would make a later task of the same name look already merged.
pub fn delete_empty_branches(ws: &Task) {
    for r in &ws.repos {
        let b = ws.branch_of(r);
        let at = format!("refs/heads/{b}");
        if git(&r.src, &["merge-base", "--is-ancestor", &at, &r.base]).is_ok() {
            let _ = git(&r.src, &["branch", "-D", b]);
        }
    }
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
pub fn combined_diff(ws: &Task, dir: &Path, uncommitted_only: bool) -> Result<String, String> {
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
            // Whole files, so the diff can open the lines between changes. Viewed fingerprints
            // hash the blob and the review narrows its hunks back (`review::narrow_hunks`).
            "--unified=1000000",
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

/// `origin/main` → `main`, for labels.
pub fn short(base: &str) -> &str {
    base.strip_prefix("origin/").unwrap_or(base)
}
