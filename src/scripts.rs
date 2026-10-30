//! Repo scripts from the repo settings: setup after a worktree is made, run on cmd-r, archive
//! before a worktree is removed. Each runs in `$SHELL -c` in the repo's worktree, output appended
//! to the workspace's log (`log_path`).
use crate::state::{State, Workspace};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

#[derive(Clone, Copy)]
pub enum Phase {
    Setup,
    Run,
    Archive,
}

/// One script: the repo it's from, its source checkout and worktree, the command.
#[derive(Clone)]
pub struct Job {
    pub repo: String,
    pub src: PathBuf,
    pub dir: PathBuf,
    pub script: String,
}

/// `phase`'s scripts for `ws`: repos in the sidebar's order (drag to change), each repo's in its
/// own. `only` limits them to one repo (a repo just added).
pub fn jobs(state: &State, ws: &Workspace, phase: Phase, only: Option<&str>) -> Vec<Job> {
    let mut repos: Vec<_> = ws.repos.iter().collect();
    repos.sort_by_key(|r| state.repos.iter().position(|x| x.name == r.name));
    (repos.into_iter())
        .filter(|r| only.is_none_or(|o| o == r.name))
        .flat_map(|r| {
            let s = state.repo_settings.get(&r.name);
            let list = s.map_or(&[][..], |s| match phase {
                Phase::Setup => &s.setup,
                Phase::Run => &s.run,
                Phase::Archive => &s.archive,
            });
            (list.iter().filter(|c| !c.trim().is_empty())).map(|c| Job {
                repo: r.name.clone(),
                src: r.src.clone(),
                dir: ws.dir().join(&r.name),
                script: c.clone(),
            })
        })
        .collect()
}

pub fn log_path(ws: &str) -> PathBuf {
    crate::state::support_dir()
        .join("scripts")
        .join(format!("{ws}.log"))
}

fn open_log(ws: &str) -> Result<File, String> {
    let path = log_path(ws);
    let _ = fs::create_dir_all(path.parent().unwrap());
    (OpenOptions::new().create(true).append(true).open(&path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn spawn(ws: &str, job: &Job, port: Option<u16>, log: &mut File) -> Result<Child, String> {
    let _ = writeln!(log, "\n$ {}: {}", job.repo, job.script);
    let out = log.try_clone().map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut cmd = Command::new(shell);
    if let Some(p) = port {
        cmd.env("SHARK_PORT", p.to_string());
    }
    cmd.args(["-c", &job.script])
        .current_dir(&job.dir)
        .env("SHARK_WORKSPACE", ws)
        .env("SHARK_WORKSPACE_PATH", job.dir.parent().unwrap_or(&job.dir))
        .env("SHARK_ROOT_PATH", &job.src)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        // Its own group, so stopping takes the dev servers it started with it.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{}: {e}", job.repo))
}

/// Runs `jobs` one after another and stops at the first that fails.
// ponytail: no timeout, a hung setup or archive script waits forever; add one if that bites.
pub fn run_all(ws: &str, jobs: &[Job]) -> Result<(), String> {
    let mut log = open_log(ws)?;
    for job in jobs {
        let status = spawn(ws, job, None, &mut log)?
            .wait()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("{}: `{}` failed ({status})", job.repo, job.script));
        }
    }
    Ok(())
}

/// Run scripts get `SHARK_PORT`, the first of `PORTS` free ports, so two workspaces' dev servers
/// don't fight over one.
pub const PORTS: u16 = 10;
const FIRST_PORT: u16 = 51000;

/// The first block of `PORTS` not held by a running workspace.
pub fn free_port(held: impl Iterator<Item = u16> + Clone) -> u16 {
    (0..)
        .map(|i| FIRST_PORT + i * PORTS)
        .find(|p| !held.clone().any(|h| h == *p))
        .unwrap()
}

/// A workspace's run scripts, started in order; dropping it stops them and their children.
pub struct Running {
    pub port: u16,
    /// Repo and process group per script; a group outlives its shell while what it started runs.
    groups: Vec<(String, Child, bool)>,
}

impl Running {
    pub fn start(ws: &str, jobs: &[Job], port: u16) -> Result<Self, String> {
        let mut log = open_log(ws)?;
        let mut running = Running {
            port,
            groups: vec![],
        };
        for job in jobs {
            let child = spawn(ws, job, Some(port), &mut log)?;
            running.groups.push((job.repo.clone(), child, false));
        }
        Ok(running)
    }

    /// Scripts whose shell exited since the last call, with how; true once no group is left.
    pub fn reap(&mut self) -> (Vec<String>, bool) {
        let mut done = vec![];
        self.groups.retain_mut(|(repo, c, exited)| {
            if !*exited && let Ok(Some(status)) = c.try_wait() {
                done.push(format!("{repo} ({status})"));
                *exited = true;
            }
            !*exited || group_alive(c)
        });
        (done, self.groups.is_empty())
    }

    /// TERM, then KILL what's still up after `secs`; blocks, so call it off the UI thread or at quit.
    fn stop(mut groups: Vec<(String, Child, bool)>, secs: u64) {
        for (_, c, _) in &groups {
            signal_group("-TERM", c);
        }
        for _ in 0..secs * 10 {
            groups.retain_mut(|(_, c, _)| {
                let _ = c.try_wait();
                group_alive(c)
            });
            if groups.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        for (_, c, _) in &mut groups {
            signal_group("-KILL", c);
            let _ = c.wait();
        }
    }

    /// At quit: stop everything before Shark exits.
    pub fn shutdown(mut self) {
        Self::stop(std::mem::take(&mut self.groups), 2);
    }
}

fn signal_group(sig: &str, c: &Child) -> bool {
    (Command::new("/bin/kill").args([sig, "--", &format!("-{}", c.id())]))
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Any process left in the script's group (its pid is the group id).
fn group_alive(c: &Child) -> bool {
    signal_group("-0", c)
}

impl Drop for Running {
    /// Stops in the background, so servers get 5s to shut down cleanly.
    fn drop(&mut self) {
        let groups = std::mem::take(&mut self.groups);
        if !groups.is_empty() {
            std::thread::spawn(move || Self::stop(groups, 5));
        }
    }
}

/// The last lines of `ws`'s log, for an error notice.
pub fn tail(ws: &str) -> String {
    let text = fs::read_to_string(log_path(ws)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

#[cfg(test)]
pub(crate) fn job(dir: &std::path::Path, script: &str) -> Job {
    Job {
        repo: "api".into(),
        src: dir.into(),
        dir: dir.into(),
        script: script.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_in_order_and_stops_at_failure() {
        let dir = std::env::temp_dir().join(format!("shark-scripts-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ws = format!("scripts-test-{}", std::process::id());
        let jobs = [
            job(&dir, "echo first >> out; echo $SHARK_WORKSPACE >> out"),
            job(&dir, "exit 3"),
            job(&dir, "echo never >> out"),
        ];
        let err = run_all(&ws, &jobs).unwrap_err();
        assert!(err.contains("`exit 3` failed"), "{err}");
        let out = fs::read_to_string(dir.join("out")).unwrap();
        assert_eq!(out, format!("first\n{ws}\n"));
        assert!(tail(&ws).contains("$ api: exit 3"));

        // Dropping stops a long-running script and what it started.
        // A script that leaves a process behind stays running until that's gone too.
        let mut running = Running::start(&ws, &[job(&dir, "sleep 30 &")], 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (exited, all) = running.reap();
        assert_eq!((exited.len(), all), (1, false), "its sleep is still up");
        running.shutdown();
        let mut quick =
            Running::start(&ws, &[job(&dir, "echo $SHARK_PORT > port")], 51010).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(quick.reap(), (vec!["api (exit status: 0)".into()], true));
        assert_eq!(fs::read_to_string(dir.join("port")).unwrap(), "51010\n");
        assert_eq!(free_port([51000, 51020].into_iter()), 51010);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(log_path(&ws));
    }
}
