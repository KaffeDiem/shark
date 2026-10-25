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
        .map_err(|e| format!("{prog}: {e}"))
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
    let child = c
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    // wait_with_output drains both pipes, so a big gh response can't fill one and deadlock.
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(out) => finish(prog, args, out.map_err(|e| format!("{prog}: {e}"))?),
        Err(_) => {
            term(pid); // the waiting thread reaps it
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
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
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
    let root = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?);
    let name = root.file_name().unwrap().to_string_lossy().to_string();
    if existing.iter().any(|r| r.name == name) {
        return Err(format!("a repo named `{name}` is already added"));
    }
    Ok(Repo { name, path: root })
}

fn default_base(src: &Path) -> String {
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

pub fn create_workspace(
    dir: &Path,
    name: &str,
    branch: &str,
    repos: &[Repo],
) -> Result<Workspace, String> {
    if repos.is_empty() {
        return Err("pick at least one repo".into());
    }
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    git(&repos[0].path, &["check-ref-format", "--branch", branch])?;
    // Every repo is checked (and fetched) before any worktree is added, so a refusal in one
    // repo leaves nothing behind in the others.
    let plans = par_map(repos, |r| plan_worktree(branch, r))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let done = par_map(repos.iter().zip(&plans), |(r, p)| {
            add_worktree(dir, branch, r, p)
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
        fs::write(dir.join("CLAUDE.md"), claude_md(&ws, dir)).map_err(|e| e.to_string())?;
        Ok(ws)
    })();
    if result.is_err() {
        for (r, p) in repos.iter().zip(&plans) {
            undo_worktree(dir, branch, r, p);
        }
        let _ = fs::remove_dir_all(dir);
    }
    result
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
    let plan = plan_worktree(&ws.branch, r)?;
    let mut ws = ws.clone();
    let added = add_worktree(dir, &ws.branch, r, &plan).and_then(|wr| {
        ws.repos.push(wr);
        fs::write(dir.join("CLAUDE.md"), claude_md(&ws, dir)).map_err(|e| e.to_string())
    });
    if let Err(e) = added {
        undo_worktree(dir, &ws.branch, r, &plan);
        return Err(e);
    }
    Ok(ws)
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

fn has_ref(dir: &Path, rf: &str) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", rf]).is_ok()
}

struct Plan {
    base: String,
    /// Where a new branch starts: `origin/<branch>` when only origin has it, the base when
    /// nobody does. None reuses the local branch: origin has it too, it holds unpushed work,
    /// or origin was unreachable.
    from: Option<String>,
}

/// Checks a repo can take the branch and decides where it starts. Fetches, so it blocks.
fn plan_worktree(branch: &str, r: &Repo) -> Result<Plan, String> {
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
    let base = default_base(&r.path);
    let from = match (
        has_ref(&r.path, &format!("refs/heads/{branch}")),
        has_ref(&r.path, &format!("refs/remotes/origin/{branch}")),
    ) {
        (true, true) => None,
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
    Ok(Plan { base, from })
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
    copy_included(&r.path, &wt)?;
    Ok(WsRepo {
        name: r.name.clone(),
        src: r.path.clone(),
        base: plan.base.clone(),
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

/// Rewrites the root CLAUDE.md from the workspace's repo list; a failure only leaves it stale.
pub fn write_claude_md(ws: &Workspace) {
    let dir = ws.dir();
    let _ = fs::write(dir.join("CLAUDE.md"), claude_md(ws, &dir));
}

fn claude_md(ws: &Workspace, dir: &Path) -> String {
    let mut s = format!(
        "# Workspace `{}`\n\nThis directory is NOT a git repo. Each subdirectory is a git worktree on branch `{}`:\n",
        ws.name, ws.branch
    );
    for r in &ws.repos {
        s += &format!(
            "- `{}/`: worktree of {} (base `{}`)\n",
            r.name,
            r.src.display(),
            r.base
        );
    }
    s += "\nRun git per repo (e.g. `git -C <repo> status`) and commit in each repo separately. Never run git in this root directory.\n\n";
    for r in &ws.repos {
        if dir.join(&r.name).join("CLAUDE.md").exists() {
            s += &format!("@{}/CLAUDE.md\n", r.name);
        }
    }
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
            .filter(|n| n != "CLAUDE.md" && !ws.repos.iter().any(|r| &r.name == n))
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
        let _ = fs::remove_file(dir.join("CLAUDE.md"));
        fs::remove_dir(dir)
            .map_err(|e| format!("{}: {e} (unexpected files left)", dir.display()))?;
    }
    Ok(())
}

/// After an archive: deletes the workspace branch in each `(repo, head)` source repo, but only
/// while the branch still points at `head`, the commit that was found merged. `-D` because a
/// squash merge leaves the branch unmerged as far as git can tell. Returns the repos done.
pub fn delete_merged_branches(ws: &Workspace, merged: &[(String, String)]) -> Vec<String> {
    let at = format!("refs/heads/{}", ws.branch);
    ws.repos
        .iter()
        .filter(|r| {
            merged.iter().any(|(n, head)| {
                n == &r.name
                    && git(&r.src, &["rev-parse", "--verify", "--quiet", &at]).as_deref()
                        == Ok(head.as_str())
                    && git(&r.src, &["branch", "-D", &ws.branch]).is_ok()
            })
        })
        .map(|r| r.name.clone())
        .collect()
}

/// One unified patch for all repos. Every path is prefixed with the repo name.
/// A failed `git diff` (e.g. a missing base) is an error naming the repo, not an empty patch.
pub fn combined_diff(ws: &Workspace, dir: &Path) -> Result<String, String> {
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
        let o = git_tmp(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            // Full hashes: abbreviations grow with the repo and would change viewed fingerprints.
            "--full-index",
            // Rename headers carry no --src/--dst prefix, so a renamed file would lose its repo.
            "--no-renames",
            &src,
            &dst,
            "--merge-base",
            &r.base,
        ]);
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
    fn workspace_roundtrip() {
        let root = std::env::temp_dir().join(format!("shark-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repos = vec![init_repo(&root, "api"), init_repo(&root, "web")];
        let dir = root.join("ws/feat");
        let ws = create_workspace(&dir, "feat", "feat-x", &repos).unwrap();

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
        assert!(
            create_workspace(&root.join("ws/other"), "other", "feat-x", &repos).is_err(),
            "branch collision"
        );
        // `git init` + `remote add` leaves origin/HEAD unset; the base is still origin/main.
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]).unwrap();
        let api = &repos[0].path;
        git(api, &["remote", "add", "origin", &origin.to_string_lossy()]).unwrap();
        git(api, &["push", "-q", "origin", "main"]).unwrap();
        git(api, &["checkout", "-q", "-b", "side"]).unwrap();
        let other =
            create_workspace(&root.join("ws/other"), "other", "feat-y", &repos[..1]).unwrap();
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
        let diff = combined_diff(&ws, &dir).unwrap();
        assert!(diff.contains("+++ b/api/README.md"), "{diff}");
        assert!(
            diff.contains("--- a/web/CLAUDE.md") && diff.contains("+++ b/web/RULES.md"),
            "pure rename keeps the repo prefix: {diff}"
        );
        let mut bad = ws.clone();
        bad.repos[0].base = "origin/nope".into();
        assert!(combined_diff(&bad, &dir).unwrap_err().starts_with("api: "));
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
        let ws = create_workspace(&dir, "r", "remote-only", &repos[..1]).unwrap();
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
        let err = create_workspace(&root.join("ws/l"), "l", "local-only", &repos)
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
        let uws = create_workspace(&u, "u", "unpushed", &repos[..1]).unwrap();
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
