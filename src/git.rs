use crate::state::{Repo, Workspace, WsRepo};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

pub fn run(dir: &Path, prog: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(prog)
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| spawn_err(dir, prog, e))
}

/// A missing program reads better than `git: No such file or directory`. `dir` is checked
/// because a missing working directory fails the same way.
pub(crate) fn spawn_err(dir: &Path, prog: &str, e: std::io::Error) -> String {
    match e.kind() == std::io::ErrorKind::NotFound && dir.is_dir() {
        true => format!("{prog} not found on PATH"),
        false => format!("{prog}: {e}"),
    }
}

fn finish(prog: &str, args: &[&str], out: Output) -> Result<String, String> {
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
    let mut c = Command::new(prog);
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
    wait_bounded(c, dir, prog, args, secs, false)
}

/// Runs `c` like `cmd_bounded`. With `group` it gets its own session (and process group), all
/// of which a timeout kills: a shell's children would otherwise hold stdout open and the waiter
/// with it. A session, not just a group: an interactive shell in a background group of the
/// terminal Shark was started from stops on SIGTTOU.
pub fn wait_bounded(
    mut c: Command,
    dir: &Path,
    prog: &str,
    args: &[&str],
    secs: u64,
    group: bool,
) -> Result<String, String> {
    if group {
        unsafe extern "C" {
            fn setsid() -> i32;
        }
        // SAFETY: setsid is async-signal-safe; it fails only for a group leader, which a
        // freshly forked child isn't.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut c, || {
                setsid();
                Ok(())
            })
        };
    }
    let child = c
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_err(dir, prog, e))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    // wait_with_output drains both pipes, so a big gh response can't fill one and deadlock.
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(out) => finish(prog, args, out.map_err(|e| format!("{prog}: {e}"))?),
        Err(_) => {
            // The waiting thread reaps it. An interactive shell ignores TERM.
            match group {
                true => kill("-KILL", &format!("-{pid}")),
                false => term(pid),
            }
            Err(format!(
                "{prog} {}: timed out after {secs}s",
                args.join(" ")
            ))
        }
    }
}

/// Runs git, returning trimmed stdout, or stderr as the error on non-zero exit.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    finish("git", args, run(dir, "git", args)?)
}

pub fn term(pid: u32) {
    kill("-TERM", &pid.to_string())
}

fn kill(sig: &str, target: &str) {
    let _ = Command::new("kill").args([sig, "--", target]).status();
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

pub(crate) fn default_base(src: &Path) -> String {
    git(
        src,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    // Only clone sets origin/HEAD; `git init` + `remote add` leaves it unset.
    .or_else(|_| {
        ["origin/main", "origin/master"]
            .into_iter()
            .find(|b| has_ref(src, &format!("refs/remotes/{b}")))
            .map(String::from)
            .ok_or_else(String::new)
    })
    .or_else(|_| git(src, &["rev-parse", "--abbrev-ref", "HEAD"]))
    .unwrap_or_else(|_| "HEAD".into())
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
    let plans = par_map(repos, |(r, b)| {
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
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let done = par_map(repos.iter().zip(&plans), |((r, b), p)| {
            let own = (b != branch).then(|| b.clone());
            add_worktree(dir, b, r, p).map(|w| WsRepo { branch: own, ..w })
        })
        .into_iter()
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
    if result.is_err() {
        for ((r, b), p) in repos.iter().zip(&plans) {
            undo_worktree(dir, b, r, p);
        }
        let _ = fs::remove_dir_all(dir);
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
    let added = add_worktree(dir, &ws.branch, r, &plan).and_then(|wr| {
        ws.repos.push(wr);
        write_root_docs(&ws, dir)
    });
    if let Err(e) = added {
        undo_worktree(dir, &ws.branch, r, &plan);
        return Err(e);
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
        let head = format!("branch refs/heads/{name}");
        seen.insert(name, out.len());
        out.push(Branch {
            name: name.into(),
            local,
            origin: !local,
            checked_out: (wts.iter().find(|(_, attrs)| attrs.contains(&head)))
                .map(|(p, _)| p.clone()),
        });
    }
    Ok(out)
}

/// NUL-separated `git ls-files -z` output; avoids git's quoting of unusual filenames.
pub fn ls_files(dir: &Path, args: &[&str]) -> Vec<String> {
    let out = run(dir, "git", &[&["ls-files", "-z"], args].concat())
        .map(|o| o.stdout)
        .unwrap_or_default();
    out.split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

/// Runs `f` on every item on its own thread; results in input order.
pub(crate) fn par_map<T: Send, R: Send>(
    items: impl IntoIterator<Item = T>,
    f: impl Fn(T) -> R + Sync,
) -> Vec<R> {
    let f = &f;
    std::thread::scope(|s| {
        let jobs: Vec<_> = items.into_iter().map(|i| s.spawn(move || f(i))).collect();
        jobs.into_iter().map(|j| j.join().unwrap()).collect()
    })
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

fn has_ref(dir: &Path, rf: &str) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", rf]).is_ok()
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

/// Checks a repo can take the branch and decides where it starts. Fetches, so it blocks.
/// With a `parent` branch, a new branch starts on it and the base is `origin/<parent>`.
fn plan_worktree(branch: &str, r: &Repo, parent: Option<&str>) -> Result<Plan, String> {
    let head = format!("branch refs/heads/{branch}");
    if let Some((path, _)) = worktree_list(&r.path)?
        .iter()
        .find(|(_, attrs)| attrs.contains(&head))
    {
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
    let base = match parent {
        Some(p) => format!("origin/{p}"),
        None => default_base(&r.path),
    };
    if let Some(p) = parent {
        // An existing branch has its own history, not the parent's.
        let exists = |rf: String| has_ref(&r.path, &rf);
        if exists(format!("refs/heads/{branch}")) || exists(format!("refs/remotes/origin/{branch}"))
        {
            return Err(format!(
                "{}: `{branch}` already exists; stacking on `{p}` needs a new branch name",
                r.name
            ));
        }
        let from = parent_start(r, p)?;
        let tip = git(&r.path, &["rev-parse", &from])?;
        return Ok(Plan {
            base,
            from: Some(from),
            pr_url: None,
            tip: Some(tip),
        });
    }
    let from = match (
        has_ref(&r.path, &format!("refs/heads/{branch}")),
        has_ref(&r.path, &format!("refs/remotes/origin/{branch}")),
    ) {
        (true, true) => {
            // Behind origin with nothing of its own: catch up first. Nothing has it checked out
            // (see above). Diverged keeps local; the git button offers "Sync with remote".
            let (local, remote) = (
                format!("refs/heads/{branch}"),
                format!("refs/remotes/origin/{branch}"),
            );
            if let Ok(old) = git(&r.path, &["rev-parse", &local])
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
            if fetched
                && git(
                    &r.path,
                    &[
                        "merge-base",
                        "--is-ancestor",
                        &format!("refs/heads/{branch}"),
                        &base,
                    ],
                )
                .is_ok() =>
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
    Ok(Plan {
        base,
        from,
        pr_url: None,
        tip: None,
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

fn plan_pr_worktree(branch: &str, r: &Repo, pr: &PullRequest) -> Result<Plan, String> {
    if pr.base.is_empty() {
        return Err(
            "Pull request base is missing; refresh pull requests and select it again".into(),
        );
    }
    if has_ref(&r.path, &format!("refs/heads/{branch}")) {
        return Err(format!(
            "{}: review branch `{branch}` already exists; pick another branch name",
            r.name
        ));
    }
    let path = pr
        .url
        .strip_prefix("https://")
        .ok_or("Invalid pull request URL")?;
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 5
        || parts[3] != "pull"
        || parts[4].parse::<u64>().ok() != Some(pr.number)
        || pr.number == 0
        || parts[..3].iter().any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-_:".contains(&b))
        })
    {
        return Err("Invalid pull request URL".into());
    }
    git(
        &r.path,
        &["check-ref-format", &format!("refs/heads/{}", pr.base)],
    )?;
    let repo = format!("{}/{}", parts[1], parts[2]);
    let remote = format!("https://{}/{repo}.git", parts[0]);
    let origin = git(&r.path, &["remote", "get-url", "origin"]).unwrap_or_default();
    let origin = origin.trim_end_matches('/').trim_end_matches(".git");
    let matching_origin = [
        format!("https://{}/{repo}", parts[0]),
        format!("git@{}:{repo}", parts[0]),
        format!("ssh://git@{}/{repo}", parts[0]),
    ]
    .contains(&origin.to_string());
    let remote = if matching_origin { "origin" } else { &remote };
    // shortcut: review refs refresh during creation only; refresh them when review-update support is added.
    let head = format!("refs/shark/pull/{}/head", pr.number);
    let base = format!("refs/shark/pull/{}/base", pr.number);
    cmd_bounded(
        &r.path,
        "git",
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            remote,
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
        base,
        from: Some(from),
        pr_url: Some(pr.url.clone()),
        tip: None,
    })
}

fn add_worktree(dir: &Path, branch: &str, r: &Repo, plan: &Plan) -> Result<WsRepo, String> {
    let wt = dir.join(&r.name);
    let wt_s = wt.to_string_lossy();
    // --no-track: otherwise the branch tracks its base (or origin), and `git pull` would merge
    // main; ahead/behind is measured against origin/<branch> by name instead.
    match &plan.from {
        None => git(&r.path, &["worktree", "add", &wt_s, branch])?,
        Some(from) => git(
            &r.path,
            &["worktree", "add", "--no-track", "-b", branch, &wt_s, from],
        )?,
    };
    if let Some(tip) = &plan.tip {
        git(&r.path, &["config", &parent_tip_key(branch), tip])?;
    }
    if let Some(url) = &plan.pr_url {
        git(
            &r.path,
            &["config", &format!("branch.{branch}.shark-pr-url"), url],
        )?;
    }
    copy_included(&r.path, &wt)?;
    Ok(WsRepo {
        name: r.name.clone(),
        src: r.path.clone(),
        base: plan.base.clone(),
        branch: None,
    })
}

/// Best effort: removes a worktree `add_worktree` may have made, and the branch if it made one.
fn undo_worktree(dir: &Path, branch: &str, r: &Repo, plan: &Plan) {
    let wt = dir.join(&r.name);
    let _ = git(
        &r.path,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    let _ = git(&r.path, &["worktree", "prune"]);
    if plan.from.is_some() {
        let _ = git(&r.path, &["branch", "-D", branch]);
    }
}

/// Copy gitignored files like `.env` into the new worktree. Patterns come from
/// `.worktreeinclude` (gitignore syntax), defaulting to `.env*`.
fn copy_included(src: &Path, wt: &Path) -> Result<(), String> {
    let exclude = if src.join(".worktreeinclude").exists() {
        "--exclude-from=.worktreeinclude".to_string()
    } else {
        "--exclude=.env*".to_string()
    };
    let ignored = ls_files(src, &["-o", "-i", "--exclude-standard"]);
    for f in ls_files(src, &["-o", "-i", &exclude])
        .iter()
        .filter(|f| ignored.contains(f))
    {
        let to = wt.join(f);
        if let Some(p) = to.parent() {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        fs::copy(src.join(f), to).map_err(|e| format!("copy {f}: {e}"))?;
    }
    Ok(())
}

/// Rewrites the root CLAUDE.md and AGENTS.md from the workspace's repo list; a failure only
/// leaves them stale.
pub fn write_claude_md(ws: &Workspace) {
    let _ = write_root_docs(ws, &ws.dir());
}

/// The files the agent reads at the workspace root: CLAUDE.md for claude, AGENTS.md for codex.
const ROOT_DOCS: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

fn write_root_docs(ws: &Workspace, dir: &Path) -> Result<(), String> {
    fs::write(dir.join("CLAUDE.md"), claude_md(ws, dir))
        .and_then(|()| fs::write(dir.join("AGENTS.md"), agents_md(ws, dir)))
        .map_err(|e| e.to_string())
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

/// Codex has no `@` imports and reads AGENTS.md only from here, so it is pointed at each repo's.
fn agents_md(ws: &Workspace, dir: &Path) -> String {
    let mut s = header(ws);
    for r in &ws.repos {
        if let Some(f) = ["AGENTS.md", "CLAUDE.md"]
            .into_iter()
            .find(|f| dir.join(&r.name).join(f).exists())
        {
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

pub fn remove_workspace(ws: &Workspace, dir: &Path, force: bool) -> Result<(), String> {
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
            .filter(|n| !ROOT_DOCS.contains(&n.as_str()) && !ws.repos.iter().any(|r| &r.name == n))
            // Ours too: `.context` holding only the dropped and pasted files.
            .filter(|n| {
                // Unreadable, or a file, counts as unexpected: refuse before removing anything.
                n != ".context"
                    || fs::read_dir(dir.join(n)).map_or(true, |mut es| {
                        es.any(|e| e.map_or(true, |e| e.file_name() != "attachments"))
                    })
            })
            .collect();
        if !extra.is_empty() {
            return Err(format!(
                "{}: unexpected files: {}",
                dir.display(),
                extra.join(", ")
            ));
        }
    }
    for r in &ws.repos {
        let wt = dir.join(&r.name);
        let wt_s = wt.to_string_lossy();
        if wt.exists() {
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push(&wt_s);
            git(&r.src, &args)?;
        }
        let _ = git(&r.src, &["worktree", "prune"]);
    }
    if force {
        let _ = fs::remove_dir_all(dir);
    } else {
        // Only our generated file may remain; anything else the user must force away.
        for f in ROOT_DOCS {
            let _ = fs::remove_file(dir.join(f));
        }
        // Dropped and pasted files; sent messages may still refer to them until now.
        let _ = fs::remove_dir_all(dir.join(".context"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_choices_merge_newest_first() {
        let b = |t, n: &str| (t, format!("origin/{n}"));
        // web is on its own branch `x`; api's `feat` is still a base web may take.
        let repos = [
            (
                "api".to_string(),
                "feat".to_string(),
                vec![b(9, "feat"), b(5, "main"), b(1, "old")],
            ),
            (
                "web".to_string(),
                "x".to_string(),
                vec![b(8, "feat"), b(7, "main"), b(3, "x")],
            ),
        ];
        let got = base_choices(&repos);
        let names: Vec<_> = got
            .iter()
            .map(|(n, rs)| (n.as_str(), rs.join(",")))
            .collect();
        assert_eq!(
            names,
            [
                ("origin/feat", "web".into()),
                ("origin/main", "web,api".into()),
                ("origin/old", "api".into())
            ]
        );
    }

    /// A timeout kills the whole group: a background child holding stdout open dies too.
    #[test]
    fn group_timeout_kills_grandchildren() {
        let pidfile = std::env::temp_dir().join(format!("shark-grp-{}", std::process::id()));
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let started = std::time::Instant::now();
        let c = Command::new("sh");
        let e = wait_bounded(c, Path::new("/"), "sh", &["-c", &script], 1, true).unwrap_err();
        assert!(
            e.contains("timed out") && started.elapsed().as_secs() < 5,
            "{e}"
        );
        let pid = fs::read_to_string(&pidfile).unwrap();
        // Killed at once, but `kill -0` sees it until launchd reaps the orphan.
        let alive = || {
            let s = Command::new("kill").args(["-0", pid.trim()]).status();
            s.unwrap().success()
        };
        let reaped = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            !alive()
        });
        assert!(reaped, "sleep {pid} outlived the timeout");
        fs::remove_file(&pidfile).ok();
    }

    fn init_repo(root: &Path, name: &str) -> Repo {
        let p = root.join("src").join(name);
        fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-q", "-b", "main"]).unwrap();
        fs::write(p.join("README.md"), "hi\n").unwrap();
        fs::write(p.join("CLAUDE.md"), format!("{name} rules\n")).unwrap();
        fs::write(p.join(".gitignore"), ".env\n").unwrap();
        fs::write(p.join(".env"), "SECRET=1\n").unwrap();
        git(&p, &["add", "."]).unwrap();
        fs::write(p.join(".env.local"), "untracked, not ignored\n").unwrap();
        git(
            &p,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        )
        .unwrap();
        add_repo(&p, &[]).unwrap()
    }

    #[test]
    fn pr_checkout_uses_exact_tip_base_and_rolls_back() {
        let root = std::env::temp_dir().join(format!("shark-pr-checkout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repo = init_repo(&root, "api");
        let other = init_repo(&root, "web");
        let remote = root.join("upstream.git");
        git(&root, &["init", "-q", "--bare", &remote.to_string_lossy()]).unwrap();
        let base = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
        git(
            &repo.path,
            &[
                "push",
                "-q",
                &remote.to_string_lossy(),
                "HEAD:refs/heads/release",
            ],
        )
        .unwrap();
        git(
            &repo.path,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "PR tip",
            ],
        )
        .unwrap();
        let tip = git(&repo.path, &["rev-parse", "HEAD"]).unwrap();
        git(
            &repo.path,
            &[
                "push",
                "-q",
                &remote.to_string_lossy(),
                "HEAD:refs/pull/7/head",
            ],
        )
        .unwrap();
        git(
            &repo.path,
            &[
                "config",
                &format!("url.{}.insteadOf", remote.display()),
                "https://github.com/o/upstream.git",
            ],
        )
        .unwrap();
        let pr = PullRequest {
            number: 7,
            url: "https://github.com/o/upstream/pull/7".into(),
            base: "release".into(),
        };
        let picked = [(repo.clone(), "pr/7".into())];
        let prs = [(repo.name.clone(), pr.clone())];
        let dir = root.join("ws/review");
        let ws = create_workspace_with_prs(&dir, "review", "pr/7", &picked, &prs, &[]).unwrap();
        let wt = dir.join("api");
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(git(&wt, &["rev-parse", &ws.repos[0].base]).unwrap(), base);
        assert_eq!(review_pr_url(&wt, "pr/7"), Some(pr.url));
        assert_eq!(
            git(&repo.path, &["branch", "--show-current"]).unwrap(),
            "main"
        );
        let collision = root.join("ws/collision");
        assert!(
            create_workspace_with_prs(&collision, "collision", "pr/7", &picked, &prs, &[])
                .err()
                .unwrap()
                .contains("already exists")
        );
        assert!(!collision.exists());
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]).unwrap(), tip);
        let missing = [(
            repo.name.clone(),
            PullRequest {
                number: 404,
                url: "https://github.com/o/upstream/pull/404".into(),
                base: "release".into(),
            },
        )];
        let failed = root.join("ws/missing");
        assert!(
            create_workspace_with_prs(
                &failed,
                "missing",
                "pr/404",
                &[(repo.clone(), "pr/404".into())],
                &missing,
                &[]
            )
            .is_err()
        );
        assert!(!failed.exists() && !has_ref(&repo.path, "refs/heads/pr/404"));
        git(&other.path, &["branch", "blocked"]).unwrap();
        let failed = root.join("ws/rollback");
        let picked = [
            (repo.clone(), "review-again".into()),
            (other, "blocked/child".into()),
        ];
        assert!(
            create_workspace_with_prs(&failed, "rollback", "review-again", &picked, &prs, &[])
                .is_err()
        );
        assert!(!failed.exists() && !has_ref(&repo.path, "refs/heads/review-again"));
        assert_eq!(review_pr_url(&repo.path, "review-again"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_roundtrip() {
        let root = std::env::temp_dir().join(format!("shark-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repos = vec![init_repo(&root, "api"), init_repo(&root, "web")];
        let dir = root.join("ws/feat");
        let ws = create_workspace(&dir, "feat", "feat-x", &on_branch(&repos, "feat-x")).unwrap();

        assert_eq!(
            git(&dir.join("api"), &["branch", "--show-current"]).unwrap(),
            "feat-x"
        );
        assert!(dir.join("web/.env").exists(), ".env copied");
        let md = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(
            md.contains("@api/CLAUDE.md") && md.contains("@web/CLAUDE.md"),
            "{md}"
        );
        let agents = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(
            agents.contains("read `api/CLAUDE.md`") && agents.contains("branch `feat-x`"),
            "{agents}"
        );
        assert!(
            create_workspace(
                &root.join("ws/other"),
                "other",
                "feat-x",
                &on_branch(&repos, "feat-x")
            )
            .is_err(),
            "branch collision"
        );
        // `git init` + `remote add` leaves origin/HEAD unset; the base is still origin/main.
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        let api = &repos[0].path;
        git(api, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
        git(api, &["push", "-q", "origin", "main"]).unwrap();
        git(api, &["checkout", "-q", "-b", "side"]).unwrap();
        let other = create_workspace(
            &root.join("ws/other"),
            "other",
            "feat-y",
            &on_branch(&repos[..1], "feat-y"),
        )
        .unwrap();
        assert_eq!(other.repos[0].base, "origin/main");
        remove_workspace(&other, &root.join("ws/other"), true).unwrap();

        fs::write(dir.join("web/dirty.txt"), "x\n").unwrap();
        let err = remove_workspace(&ws, &dir, false).unwrap_err();
        assert!(err.contains("web") && !err.contains("api"), "{err}");
        assert!(
            dir.join("api/README.md").exists(),
            "clean api kept while web is dirty"
        );
        fs::remove_file(dir.join("web/dirty.txt")).unwrap();

        fs::write(dir.join("api/README.md"), "changed\n").unwrap();
        fs::write(dir.join("web/new.txt"), "new\n").unwrap();
        fs::write(dir.join("web/résumé.txt"), "new\n").unwrap();
        git(&dir.join("web"), &["mv", "CLAUDE.md", "RULES.md"]).unwrap();
        let diff = combined_diff(&ws, &dir, false).unwrap();
        assert!(diff.contains("+++ b/api/README.md"), "{diff}");
        assert!(
            diff.contains("--- a/web/CLAUDE.md") && diff.contains("+++ b/web/RULES.md"),
            "pure rename keeps the repo prefix: {diff}"
        );
        let mut bad = ws.clone();
        bad.repos[0].base = "origin/nope".into();
        assert!(
            combined_diff(&bad, &dir, false)
                .unwrap_err()
                .starts_with("api: ")
        );
        assert!(diff.contains("+++ b/web/new.txt"), "{diff}");
        assert!(diff.contains("b/web/r"), "{diff}");
        assert!(
            !dir.join("api/.env.local").exists(),
            "non-ignored files are not copied"
        );

        fs::write(dir.join("notes.md"), "mine\n").unwrap();
        assert!(
            remove_workspace(&ws, &dir, false).is_err(),
            "unexpected root file blocks non-forced archive"
        );
        assert!(dir.join("notes.md").exists());
        remove_workspace(&ws, &dir, true).unwrap();
        assert!(!dir.exists());
        assert!(
            !git(&repos[0].path, &["worktree", "list"])
                .unwrap()
                .contains("feat")
        );
        // Only a branch still at the merged head goes; a moved one (web) is kept.
        let head = |r: &Repo| git(&r.path, &["rev-parse", "feat-x"]).unwrap();
        let merged = [
            ("api".into(), head(&repos[0])),
            ("web".into(), "moved".into()),
        ];
        assert_eq!(delete_merged_branches(&ws, &merged), ["api"]);
        assert!(
            git(
                &repos[0].path,
                &["rev-parse", "--verify", "--quiet", "feat-x"]
            )
            .is_err()
        );
        assert!(
            git(
                &repos[1].path,
                &["rev-parse", "--verify", "--quiet", "feat-x"]
            )
            .is_ok()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn per_repo_branches() {
        let root = std::env::temp_dir().join(format!("shark-test-prb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (api, web) = (init_repo(&root, "api"), init_repo(&root, "web"));
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        let a = &api.path;
        git(a, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
        git(a, &["push", "-q", "origin", "main", "main:kd/x"]).unwrap();
        git(a, &["remote", "set-head", "origin", "main"]).unwrap();
        // `behind`: origin has one commit more than the local branch.
        git(a, &["checkout", "-q", "-b", "behind"]).unwrap();
        let commit = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q"];
        git(a, &[&commit[..], &["--allow-empty", "-m", "more"]].concat()).unwrap();
        git(a, &["push", "-q", "origin", "behind"]).unwrap();
        let tip = git(a, &["rev-parse", "HEAD"]).unwrap();
        git(a, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        git(a, &["checkout", "-q", "main"]).unwrap();

        // api on an origin-only branch, web on the new one.
        let dir = root.join("ws/feat");
        let ws = create_workspace(
            &dir,
            "feat",
            "feat",
            &[(api.clone(), "kd/x".into()), (web.clone(), "feat".into())],
        )
        .unwrap();
        let on = |r: &str| git(&dir.join(r), &["branch", "--show-current"]).unwrap();
        assert_eq!((on("api"), on("web")), ("kd/x".into(), "feat".into()));
        assert_eq!(ws.repos[0].branch.as_deref(), Some("kd/x"));
        assert_eq!(ws.repos[1].branch, None);
        let md = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert!(
            md.contains("worktree:\n") && md.contains(" on `kd/x` ") && md.contains(" on `feat` "),
            "{md}"
        );

        let bs = branches(a).unwrap();
        let get = |n: &str| bs.iter().find(|b| b.name == n).cloned();
        let x = get("kd/x").unwrap();
        assert!(x.local && x.origin, "{x:?}");
        assert!(x.checked_out.unwrap().ends_with("ws/feat/api"));
        let main = get("main").unwrap();
        assert!(
            main.local && main.origin && main.checked_out.is_some(),
            "{main:?}"
        );
        assert!(get("HEAD").is_none(), "origin/HEAD is dropped");

        // A branch checked out in the source checkout is refused; nothing is left behind.
        let err = create_workspace(
            &root.join("ws/m"),
            "m",
            "m",
            &[(web.clone(), "m".into()), (api.clone(), "main".into())],
        )
        .err()
        .unwrap();
        assert!(
            err.contains("api: branch `main` is already checked out"),
            "{err}"
        );
        assert!(!root.join("ws/m").exists());
        assert!(
            git(
                &web.path,
                &["rev-parse", "--verify", "--quiet", "refs/heads/m"]
            )
            .is_err()
        );

        // A local branch behind origin is fast-forwarded first.
        let b = root.join("ws/b");
        let bws = create_workspace(
            &b,
            "b",
            "behind",
            &on_branch(std::slice::from_ref(&api), "behind"),
        )
        .unwrap();
        assert_eq!(git(&b.join("api"), &["rev-parse", "HEAD"]).unwrap(), tip);
        remove_workspace(&bws, &b, true).unwrap();

        // Archive deletes each repo's own branch.
        remove_workspace(&ws, &dir, true).unwrap();
        let x_head = git(a, &["rev-parse", "kd/x"]).unwrap();
        let merged = [("api".into(), x_head), ("web".into(), "moved".into())];
        assert_eq!(delete_merged_branches(&ws, &merged), ["api"]);
        assert!(git(a, &["rev-parse", "--verify", "--quiet", "refs/heads/kd/x"]).is_err());
        assert!(
            git(
                &web.path,
                &["rev-parse", "--verify", "--quiet", "refs/heads/feat"]
            )
            .is_ok()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// A new branch stacked on a parent starts at `origin/<parent>` (never the local parent's
    /// unpushed commits), targets it, and records it as its parent tip; another repo of the same
    /// workspace stays on its trunk. An unpushed or missing parent (a fork's PR) or an existing
    /// branch name refuses and leaves nothing behind.
    #[test]
    fn stacks_new_branches_on_a_parent() {
        let root = std::env::temp_dir().join(format!("shark-test-stack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (api, web) = (init_repo(&root, "api"), init_repo(&root, "web"));
        let commit = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q"];
        let empty_commit = |dir: &Path, msg: &str| {
            git(dir, &[&commit[..], &["--allow-empty", "-m", msg]].concat()).unwrap();
            git(dir, &["rev-parse", "HEAD"]).unwrap()
        };
        for r in [&api, &web] {
            let origin = root.join(format!("{}.git", r.name));
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            (git(
                &r.path,
                &["remote", "add", "origin", &origin.to_string_lossy()],
            ))
            .unwrap();
            git(&r.path, &["push", "-q", "origin", "main"]).unwrap();
            git(&r.path, &["remote", "set-head", "origin", "main"]).unwrap();
        }
        let a = &api.path;
        let branch = |name: &str, f: &dyn Fn()| {
            git(a, &["checkout", "-q", "-b", name, "main"]).unwrap();
            f();
            git(a, &["checkout", "-q", "main"]).unwrap();
        };
        // Only on origin.
        let mut remote_tip = String::new();
        branch("remote", &|| {
            empty_commit(a, "remote");
        });
        git(a, &["push", "-q", "origin", "remote"]).unwrap();
        remote_tip.push_str(&git(a, &["rev-parse", "remote"]).unwrap());
        git(a, &["branch", "-q", "-D", "remote"]).unwrap();
        // Local is ahead of origin: your own parent workspace with an unpushed commit.
        branch("ahead", &|| {
            empty_commit(a, "pushed");
            git(a, &["push", "-q", "origin", "ahead"]).unwrap();
            empty_commit(a, "unpushed");
        });
        // Local is behind origin: someone pushed to the parent since.
        branch("behind", &|| {
            empty_commit(a, "old");
            empty_commit(a, "new");
            git(a, &["push", "-q", "origin", "behind"]).unwrap();
            git(a, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        });
        // Never pushed.
        branch("local", &|| {
            empty_commit(a, "local");
        });
        let head = |dir: &Path| git(dir, &["rev-parse", "HEAD"]).unwrap();
        let cases = [
            ("remote", remote_tip.clone()),
            ("ahead", git(a, &["rev-parse", "origin/ahead"]).unwrap()),
            ("behind", git(a, &["rev-parse", "origin/behind"]).unwrap()),
        ];
        for (parent, tip) in cases {
            let name = format!("on-{parent}");
            let dir = root.join("ws").join(&name);
            let parents = [("api".to_string(), parent.to_string())];
            let ws = create_workspace_with_prs(
                &dir,
                &name,
                &name,
                &on_branch(&[api.clone(), web.clone()], &name),
                &[],
                &parents,
            )
            .unwrap();
            assert_eq!(head(&dir.join("api")), tip, "{parent}");
            let key = parent_tip_key(&name);
            assert_eq!(git(a, &["config", &key]).unwrap(), tip);
            assert_eq!(ws.repos[0].base, format!("origin/{parent}"));
            assert_eq!(ws.repos[1].base, "origin/main", "web isn't stacked");
            assert_eq!(head(&dir.join("web")), head(&web.path));
        }
        for (parent, name, want) in [
            ("fork-only", "on-fork", "`fork-only` isn't a branch here"),
            (
                "local",
                "on-local",
                "`local` isn't on GitHub yet; push it first",
            ),
            (
                "remote",
                "ahead",
                "`ahead` already exists; stacking on `remote` needs a new",
            ),
        ] {
            let dir = root.join("ws").join(name);
            let parents = [("api".to_string(), parent.to_string())];
            let picked = on_branch(&[web.clone(), api.clone()], name);
            let err = create_workspace_with_prs(&dir, name, name, &picked, &[], &parents)
                .err()
                .unwrap();
            assert!(err.contains(want), "{err}");
            assert!(!dir.exists());
            assert!(!has_ref(&web.path, &format!("refs/heads/{name}")));
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// The edge cases of picking existing branches per repo.
    #[test]
    fn per_repo_branch_edges() {
        let root = std::env::temp_dir().join(format!("shark-test-pre-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (api, web) = (init_repo(&root, "api"), init_repo(&root, "web"));
        let commit = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q"];
        let empty_commit = |dir: &Path, msg: &str| {
            git(dir, &[&commit[..], &["--allow-empty", "-m", msg]].concat()).unwrap();
            git(dir, &["rev-parse", "HEAD"]).unwrap()
        };
        for r in [&api, &web] {
            let origin = root.join(format!("{}.git", r.name));
            git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
            git(
                &r.path,
                &["remote", "add", "origin", &origin.to_string_lossy()],
            )
            .unwrap();
            git(&r.path, &["push", "-q", "origin", "main", "main:shared"]).unwrap();
        }
        let a = &api.path;

        // Diverged: local and origin each have a commit; local is kept, not reset.
        git(a, &["checkout", "-q", "-b", "div"]).unwrap();
        let theirs = empty_commit(a, "theirs");
        git(a, &["push", "-q", "origin", "div"]).unwrap();
        git(a, &["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        let mine = empty_commit(a, "mine");
        git(a, &["checkout", "-q", "main"]).unwrap();
        let d = root.join("ws/d");
        let dws = create_workspace(
            &d,
            "d",
            "div",
            &on_branch(std::slice::from_ref(&api), "div"),
        )
        .unwrap();
        assert_eq!(git(&d.join("api"), &["rev-parse", "HEAD"]).unwrap(), mine);
        assert_eq!(git(a, &["rev-parse", "origin/div"]).unwrap(), theirs);
        remove_workspace(&dws, &d, true).unwrap();

        // The same existing branch in both repos, under another workspace name: each checks
        // out its own, and CLAUDE.md names the one they share.
        let s = root.join("ws/s");
        let mut sws = create_workspace(
            &s,
            "s",
            "s",
            &[
                (api.clone(), "shared".into()),
                (web.clone(), "shared".into()),
            ],
        )
        .unwrap();
        for r in ["api", "web"] {
            let b = git(&s.join(r), &["branch", "--show-current"]).unwrap();
            assert_eq!(b, "shared");
        }
        assert!(
            sws.repos
                .iter()
                .all(|r| r.branch.as_deref() == Some("shared"))
        );
        let md = fs::read_to_string(s.join("CLAUDE.md")).unwrap();
        assert!(
            md.contains("worktree on branch `shared`:") && !md.contains(" on `"),
            "{md}"
        );
        // A repo added later gets the workspace branch, and CLAUDE.md now tells them apart.
        let cli = init_repo(&root, "cli");
        sws = add_to_workspace(&sws, &s, &cli).unwrap();
        assert_eq!(
            git(&s.join("cli"), &["branch", "--show-current"]).unwrap(),
            "s"
        );
        assert_eq!(sws.repos[2].branch, None);
        let md = fs::read_to_string(s.join("CLAUDE.md")).unwrap();
        assert!(
            md.contains(" on `shared` ") && md.contains(" on `s` "),
            "{md}"
        );
        remove_workspace(&sws, &s, true).unwrap();

        // A ref clash (`kd` exists, `kd/y` can't): everything is rolled back.
        git(a, &["branch", "kd"]).unwrap();
        let c = root.join("ws/c");
        let err = create_workspace(
            &c,
            "c",
            "c",
            &[(web.clone(), "c".into()), (api.clone(), "kd/y".into())],
        )
        .err()
        .unwrap();
        assert!(err.contains("kd/y"), "{err}");
        assert!(!c.exists());
        let has = |r: &Repo, b: &str| {
            git(
                &r.path,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{b}"),
                ],
            )
            .is_ok()
        };
        assert!(!has(&web, "c") && !has(&api, "kd/y"));
        for r in [&api, &web] {
            let wts = git(&r.path, &["worktree", "list"]).unwrap();
            assert_eq!(wts.lines().count(), 1, "{wts}");
        }

        // An invalid name for any repo is refused before anything happens.
        let err = create_workspace(
            &root.join("ws/bad"),
            "bad",
            "ok",
            &[
                (web.clone(), "ok".into()),
                (api.clone(), "bad..name".into()),
            ],
        )
        .err()
        .unwrap();
        assert!(err.contains("bad..name"), "{err}");
        assert!(!has(&web, "ok"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn readable_add_repo_errors() {
        let dir = std::env::temp_dir().join(format!("shark-test-norepo-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let e = add_repo(&dir, &[]).err().unwrap();
        assert_eq!(e, format!("Not a git repository: {}", dir.display()));
        let e = run(&dir, "shark-no-such-prog", &[]).unwrap_err();
        assert_eq!(e, "shark-no-such-prog not found on PATH");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn branch_reuse_add_repo_and_blockers() {
        let root = std::env::temp_dir().join(format!("shark-test-br-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repos = vec![init_repo(&root, "api"), init_repo(&root, "web")];
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        let api = &repos[0].path;
        git(api, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
        git(api, &["push", "-q", "origin", "main", "main:remote-only"]).unwrap();
        git(api, &["branch", "local-only"]).unwrap();

        // Origin-only: reused from origin, without tracking it.
        let dir = root.join("ws/r");
        let on = on_branch(&repos[..1], "remote-only");
        let ws = create_workspace(&dir, "r", "remote-only", &on).unwrap();
        let wt = dir.join("api");
        assert_eq!(
            git(&wt, &["branch", "--show-current"]).unwrap(),
            "remote-only"
        );
        assert!(
            git(&wt, &["rev-parse", "--abbrev-ref", "@{u}"]).is_err(),
            "--no-track"
        );

        // Local-only (an archived workspace's leftover): refused, and nothing is left behind.
        let on = on_branch(&repos, "local-only");
        let err = create_workspace(&root.join("ws/l"), "l", "local-only", &on)
            .err()
            .unwrap();
        assert!(err.contains("api: a local branch"), "{err}");
        assert!(!root.join("ws/l").exists());
        assert!(
            git(
                &repos[1].path,
                &["rev-parse", "--verify", "--quiet", "refs/heads/local-only"]
            )
            .is_err()
        );

        // Local-only with unpushed commits: live work, reused.
        git(api, &["checkout", "-q", "-b", "unpushed"]).unwrap();
        git(
            api,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "wip",
            ],
        )
        .unwrap();
        let wip = git(api, &["rev-parse", "HEAD"]).unwrap();
        git(api, &["checkout", "-q", "main"]).unwrap();
        let u = root.join("ws/u");
        let uws =
            create_workspace(&u, "u", "unpushed", &on_branch(&repos[..1], "unpushed")).unwrap();
        assert_eq!(git(&u.join("api"), &["rev-parse", "HEAD"]).unwrap(), wip);
        remove_workspace(&uws, &u, true).unwrap();

        // A third repo joins on the same branch; CLAUDE.md lists it.
        let ws = add_to_workspace(&ws, &dir, &repos[1]).unwrap();
        assert_eq!(
            git(&dir.join("web"), &["branch", "--show-current"]).unwrap(),
            "remote-only"
        );
        assert!(
            fs::read_to_string(dir.join("CLAUDE.md"))
                .unwrap()
                .contains("@web/CLAUDE.md")
        );
        assert!(
            add_to_workspace(&ws, &dir, &repos[1]).is_err(),
            "already in it"
        );

        // A locked worktree blocks archive up front: the other repo stays.
        git(api, &["worktree", "lock", &wt.to_string_lossy()]).unwrap();
        for force in [false, true] {
            let err = remove_workspace(&ws, &dir, force).unwrap_err();
            assert!(err.contains("api: locked"), "{err}");
            assert!(dir.join("web/README.md").exists());
        }
        git(api, &["worktree", "unlock", &wt.to_string_lossy()]).unwrap();
        fs::create_dir_all(dir.join("web/sub/.git")).unwrap();
        git(
            &dir.join("web"),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},sub", git(api, &["rev-parse", "HEAD"]).unwrap()),
            ],
        )
        .unwrap();
        let err = remove_workspace(&ws, &dir, false).unwrap_err();
        assert!(err.contains("web: has submodules"), "{err}");
        assert!(dir.join("api/README.md").exists());
        remove_workspace(&ws, &dir, true).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }
}
