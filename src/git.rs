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

/// Run a command, returning trimmed stdout, or stderr as the error on non-zero exit.
pub fn cmd(dir: &Path, prog: &str, args: &[&str]) -> Result<String, String> {
    finish(prog, args, run(dir, prog, args)?)
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

/// `cmd` for network calls: never prompts for credentials, and is killed after `secs`
/// so a hung fetch or gh can't stall whoever waits on it.
pub fn cmd_bounded(dir: &Path, prog: &str, args: &[&str], secs: u64) -> Result<String, String> {
    let child = Command::new(prog)
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{prog}: {e}"))?;
    let pid = child.id().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    // wait_with_output drains both pipes, so a big gh response can't fill one and deadlock.
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(out) => finish(prog, args, out.map_err(|e| format!("{prog}: {e}"))?),
        Err(_) => {
            let _ = Command::new("kill").args(["-TERM", &pid]).status(); // the waiting thread reaps it
            Err(format!(
                "{prog} {}: timed out after {secs}s",
                args.join(" ")
            ))
        }
    }
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    cmd(dir, "git", args)
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
    let _ = git(src, &["fetch", "--quiet", "origin"]); // best effort, may be offline or have no origin
    git(
        src,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
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
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let done = repos
            .iter()
            .map(|r| add_worktree(dir, branch, r))
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
        for r in repos {
            let _ = git(
                &r.path,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    &dir.join(&r.name).to_string_lossy(),
                ],
            );
            let _ = git(&r.path, &["worktree", "prune"]);
        }
        let _ = fs::remove_dir_all(dir);
    }
    result
}

/// NUL-separated `git ls-files -z` output; avoids git's quoting of unusual filenames.
fn ls_files(dir: &Path, args: &[&str]) -> Vec<String> {
    let out = run(dir, "git", &[&["ls-files", "-z"], args].concat())
        .map(|o| o.stdout)
        .unwrap_or_default();
    out.split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

fn add_worktree(dir: &Path, branch: &str, r: &Repo) -> Result<WsRepo, String> {
    let wt = dir.join(&r.name);
    let wt_s = wt.to_string_lossy();
    let porcelain = git(&r.path, &["worktree", "list", "--porcelain"])?;
    let mut path = "";
    for line in porcelain.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = p;
        } else if line == format!("branch refs/heads/{branch}") {
            return Err(format!(
                "{}: branch `{branch}` is already checked out at {path}",
                r.name
            ));
        }
    }
    let base = default_base(&r.path);
    if git(
        &r.path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
    {
        git(&r.path, &["worktree", "add", &wt_s, branch])?;
    } else {
        // --no-track: otherwise the branch tracks its base, and `git pull` would merge main.
        git(
            &r.path,
            &["worktree", "add", "--no-track", "-b", branch, &wt_s, &base],
        )?;
    }
    copy_included(&r.path, &wt)?;
    Ok(WsRepo {
        name: r.name.clone(),
        src: r.path.clone(),
        base,
    })
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

pub fn remove_workspace(ws: &Workspace, dir: &Path, force: bool) -> Result<(), String> {
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

/// One unified patch for all repos. Every path is prefixed with the repo name.
pub fn combined_diff(ws: &Workspace, dir: &Path) -> String {
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
        if let Ok(o) = git_tmp(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            // Full hashes: abbreviations grow with the repo and would change viewed fingerprints.
            "--full-index",
            &src,
            &dst,
            "--merge-base",
            &r.base,
        ]) {
            out += &String::from_utf8_lossy(&o.stdout);
        }
        let _ = fs::remove_file(&tmp);
    }
    out
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

        fs::write(dir.join("api/README.md"), "changed\n").unwrap();
        fs::write(dir.join("web/new.txt"), "new\n").unwrap();
        fs::write(dir.join("web/résumé.txt"), "new\n").unwrap();
        let diff = combined_diff(&ws, &dir);
        assert!(diff.contains("+++ b/api/README.md"), "{diff}");
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
        fs::remove_dir_all(&root).unwrap();
    }
}
