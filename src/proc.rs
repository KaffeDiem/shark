//! Process helpers shared by git, gh, the agents and the language-server tooling.
use crate::git::finish;
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

/// Sends `sig` to `pid` (a negative pid is a process group; sig 0 only probes). True when
/// delivered; on failure `io::Error::last_os_error()` still holds the errno.
pub(crate) fn signal(pid: i32, sig: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: POSIX kill takes no pointers.
    unsafe { kill(pid, sig) == 0 }
}

pub(crate) const SIGTERM: i32 = 15;
pub(crate) const SIGKILL: i32 = 9;

/// Asks `pid` to terminate.
pub fn term(pid: u32) {
    signal(pid as i32, SIGTERM);
}

/// A missing program reads better than `git: No such file or directory`. `dir` is checked
/// because a missing working directory fails the same way.
pub(crate) fn spawn_err(dir: &Path, prog: &str, e: std::io::Error) -> String {
    match e.kind() == std::io::ErrorKind::NotFound && dir.is_dir() {
        true => format!("{prog} not found on PATH"),
        false => format!("{prog}: {e}"),
    }
}

/// Runs `c` bounded to `secs`: killed after that so a hung fetch or gh can't stall whoever
/// waits on it. With `group` it gets its own session (and process group), all of which a
/// timeout kills: a shell's children would otherwise hold stdout open and the waiter with it.
/// A session, not just a group: an interactive shell in a background group of the terminal
/// Shark was started from stops on SIGTTOU.
pub fn wait_bounded(
    c: Command,
    dir: &Path,
    prog: &str,
    args: &[&str],
    secs: u64,
    group: bool,
) -> Result<String, String> {
    finish(prog, args, wait_output(c, dir, prog, args, secs, group)?)
}

/// `wait_bounded`, returning the raw output whatever the exit status.
pub(crate) fn wait_output(
    mut c: Command,
    dir: &Path,
    prog: &str,
    args: &[&str],
    secs: u64,
    group: bool,
) -> Result<std::process::Output, String> {
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
        Ok(out) => out.map_err(|e| format!("{prog}: {e}")),
        Err(_) => {
            // The waiting thread reaps it. An interactive shell ignores TERM.
            match group {
                true => signal(-(pid as i32), SIGKILL),
                false => signal(pid as i32, SIGTERM),
            };
            Err(format!(
                "{prog} {}: timed out after {secs}s",
                args.join(" ")
            ))
        }
    }
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

/// gh, or `SHARK_GH` (tests point it at a fake, as `SHARK_CLAUDE` for claude).
pub(crate) fn gh() -> String {
    std::env::var("SHARK_GH").unwrap_or_else(|_| "gh".into())
}

/// claude, or `SHARK_CLAUDE`: the UI tests use a fake that swallows stdin.
pub(crate) fn claude_bin() -> OsString {
    std::env::var_os("SHARK_CLAUDE").unwrap_or("claude".into())
}

/// codex, or `SHARK_CODEX`.
pub(crate) fn codex_bin() -> OsString {
    std::env::var_os("SHARK_CODEX").unwrap_or("codex".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_probes_and_kills() {
        let mut c = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = c.id() as i32;
        assert!(signal(pid, 0));
        assert!(signal(pid, SIGKILL));
        c.wait().unwrap();
        assert!(!signal(pid, 0));
    }
}
