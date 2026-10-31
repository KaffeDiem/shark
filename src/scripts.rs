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
    sync::Mutex,
    thread::JoinHandle,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Setup,
    Run,
    Archive,
}

impl Phase {
    pub const ALL: [Phase; 3] = [Phase::Setup, Phase::Run, Phase::Archive];
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
            let list = s.map_or(&[][..], |s| s.phase(phase));
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

/// `$SHELL`, or zsh.
pub fn shell() -> String {
    (std::env::var("SHELL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into())
}

fn spawn(ws: &str, job: &Job, port: Option<u16>, log: &mut File) -> Result<Child, String> {
    let _ = writeln!(log, "\n$ {}: {}", job.repo, job.script);
    let out = log.try_clone().map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(shell());
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

/// The first block of `PORTS` not held by a running or stopping workspace and with all its ports
/// free, so a server still shutting down (or started outside Shark) isn't handed out again.
pub fn free_port(held: impl Iterator<Item = u16>) -> Result<u16, String> {
    let held: Vec<u16> = held.chain(stopping().iter().map(|s| s.1)).collect();
    (0..(u16::MAX - FIRST_PORT) / PORTS)
        .map(|i| FIRST_PORT + i * PORTS)
        .find(|p| !held.contains(p) && (*p..*p + PORTS).all(port_free))
        .ok_or_else(|| format!("No {PORTS} free ports from {FIRST_PORT} up for SHARK_PORT"))
}

/// Nothing listens on `port` at any address a dev server binds: macOS lets a bind to one
/// through while another (`127.0.0.1` vs `0.0.0.0`) holds it.
fn port_free(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
    let addrs: [std::net::IpAddr; 4] = [
        Ipv4Addr::LOCALHOST.into(),
        Ipv4Addr::UNSPECIFIED.into(),
        Ipv6Addr::LOCALHOST.into(),
        Ipv6Addr::UNSPECIFIED.into(),
    ];
    // Only "in use" counts: no IPv6 on the host isn't a taken port.
    addrs.into_iter().all(|a| {
        !matches!(TcpListener::bind((a, port)), Err(e) if e.kind() == std::io::ErrorKind::AddrInUse)
    })
}

/// Runs dropped (cmd-r again) and still stopping in the background, by workspace and port: their
/// ports stay taken, and archiving and quit wait for them.
static STOPPING: Mutex<Vec<(String, u16, JoinHandle<()>)>> = Mutex::new(vec![]);

fn stopping() -> std::sync::MutexGuard<'static, Vec<(String, u16, JoinHandle<()>)>> {
    let mut s = STOPPING.lock().unwrap_or_else(|e| e.into_inner());
    s.retain(|(.., h)| !h.is_finished());
    s
}

/// Blocks until `ws`'s dropped runs (every workspace's with `None`) have stopped.
pub fn wait_stopping(ws: Option<&str>) {
    let mine: Vec<_> = {
        let mut s = stopping();
        let (mine, rest) = std::mem::take(&mut *s)
            .into_iter()
            .partition(|(w, ..)| ws.is_none_or(|ws| ws == w));
        *s = rest;
        mine
    };
    for (.., h) in mine {
        let _ = h.join();
    }
}

/// A workspace's run scripts, started in order; dropping it stops them and their children.
pub struct Running {
    ws: String,
    pub port: u16,
    /// Repo and process group per script; a group outlives its shell while what it started runs.
    groups: Vec<(String, Child, bool)>,
}

impl Running {
    pub fn start(ws: &str, jobs: &[Job], port: u16) -> Result<Self, String> {
        let mut log = open_log(ws)?;
        let mut running = Running {
            ws: ws.into(),
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
            signal_group(crate::proc::SIGTERM, c);
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
            signal_group(crate::proc::SIGKILL, c);
            let _ = c.wait();
        }
    }

    /// Stops `runs` together and waits, up to `secs` before KILL (at quit, before archiving).
    pub fn stop_all(runs: impl IntoIterator<Item = Running>, secs: u64) {
        let groups = (runs.into_iter())
            .flat_map(|mut r| std::mem::take(&mut r.groups))
            .collect();
        Self::stop(groups, secs);
    }
}

fn signal_group(sig: i32, c: &Child) -> bool {
    crate::proc::signal(-(c.id() as i32), sig)
}

/// Any process left in the script's group (its pid is the group id).
fn group_alive(c: &Child) -> bool {
    signal_group(0, c)
}

impl Drop for Running {
    /// Stops in the background, so servers get 5s to shut down cleanly; tracked in `STOPPING`.
    fn drop(&mut self) {
        let groups = std::mem::take(&mut self.groups);
        if !groups.is_empty() {
            let h = std::thread::spawn(move || Self::stop(groups, 5));
            stopping().push((std::mem::take(&mut self.ws), self.port, h));
        }
    }
}

/// The last lines of `ws`'s log, for an error notice. Reads only its end: a dev server's log grows.
pub fn tail(ws: &str) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = vec![];
    if let Ok(mut f) = File::open(log_path(ws)) {
        let len = f.metadata().map_or(0, |m| m.len());
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(8192)));
        let _ = f.read_to_end(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes);
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
        // The log goes under the support dir: the UI tests' home, never the real one.
        #[cfg(feature = "test-support")]
        let _turn = crate::app::ui_tests::UI_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::state::TEST_HOME
            .get_or_init(|| std::env::temp_dir().join(format!("shark-ui-{}", std::process::id())));
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

        // A script that leaves a process behind stays running until that's gone too; stopping
        // takes what it started with it, even one that ignores TERM.
        let reaped = |r: &mut Running| {
            let mut exited = vec![];
            for _ in 0..100 {
                let (e, all) = r.reap();
                exited.extend(e);
                if !exited.is_empty() {
                    return (exited, all);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            (exited, false)
        };
        let script = "trap '' TERM; (trap '' TERM; sleep 30) &";
        let mut running = Running::start(&ws, &[job(&dir, script)], 1).unwrap();
        let (exited, all) = reaped(&mut running);
        assert_eq!((exited.len(), all), (1, false), "its sleep is still up");
        let shell = running.groups[0].1.id();
        Running::stop_all([running], 1);
        // KILLed children are reaped by launchd, so give it a moment.
        let alive = || crate::proc::signal(-(shell as i32), 0);
        assert!((0..40).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            !alive()
        }));
        let mut quick =
            Running::start(&ws, &[job(&dir, "echo $SHARK_PORT > port")], 51010).unwrap();
        assert_eq!(
            reaped(&mut quick),
            (vec!["api (exit status: 0)".into()], true)
        );
        assert_eq!(fs::read_to_string(dir.join("port")).unwrap(), "51010\n");
        let held = free_port([51000, 51010].into_iter()).unwrap();
        assert!(![51000, 51010].contains(&held));
        let p = free_port(std::iter::empty()).unwrap();
        let taken = std::net::TcpListener::bind(("0.0.0.0", p + 3)).unwrap();
        assert_ne!(
            free_port(std::iter::empty()),
            Ok(p),
            "a block with a port in use is skipped"
        );
        drop(taken);

        // A run stopped by dropping it keeps its ports until its servers are gone.
        let script = "trap '' TERM; sleep 30";
        let stopped = Running::start(&ws, &[job(&dir, script)], p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(stopped);
        assert_ne!(free_port(std::iter::empty()), Ok(p), "still stopping");
        wait_stopping(Some(&ws));
        assert_eq!(free_port(std::iter::empty()), Ok(p));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(log_path(&ws));
    }
}
