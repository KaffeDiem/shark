//! The repo scripts in a workspace: cmd-r starts and stops its run scripts; setup runs after a
//! worktree is made (`create_workspace_on`, `repo_added`), archive before removal (`workspace.rs`).
use super::*;
use crate::scripts::{self, Phase, Running};

impl Shark {
    /// cmd-r: the active workspace's run scripts, in order; again stops them. On Home, the
    /// highlighted row's.
    pub(super) fn toggle_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = match self.home_shown() {
            true => self.home.sel.as_ref(),
            false => self.active.as_ref(),
        };
        let Some(ws) = name.and_then(|n| self.ws(n)).cloned() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        if self.archiving.contains(&ws.name) {
            return notify_err(window, cx, format!("{} is being archived", ws.name));
        }
        if self.runs.remove(&ws.name).is_some() {
            window.push_notification(format!("Stopped {}'s run scripts", ws.name), cx);
            return cx.notify();
        }
        let jobs = scripts::jobs(&self.state, &ws, Phase::Run, None);
        if jobs.is_empty() {
            return notify_err(
                window,
                cx,
                "No run scripts: click a repo in the sidebar to add them",
            );
        }
        let port = scripts::free_port(self.runs.values().map(|r| r.port));
        match port.and_then(|port| Running::start(&ws.name, &jobs, port)) {
            Ok(r) => {
                let port = r.port;
                self.runs.insert(ws.name.clone(), r);
                let n = jobs.len();
                let msg = format!(
                    "Started {n} script{} in order, without waiting on each other (SHARK_PORT {port}); cmd-r stops",
                    if n == 1 { "" } else { "s" }
                );
                window.push_notification(Notification::success(msg), cx);
                self.watch_run(ws.name, window, cx);
            }
            Err(e) => notify_err(window, cx, e),
        }
        cx.notify();
    }

    /// Tells when a run script exits by itself; forgets the run once all have.
    fn watch_run(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let gone = this.update_in(cx, |s, window, cx| {
                    let Some(run) = s.runs.get_mut(&name) else {
                        return true;
                    };
                    let (exited, all) = run.reap();
                    for e in exited {
                        let msg = format!("{name}: run script exited: {e}");
                        window.push_notification(Notification::warning(msg), cx);
                    }
                    if all {
                        s.runs.remove(&name);
                        cx.notify();
                    }
                    all
                });
                if gone.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn running(&self, ws: &str) -> bool {
        self.runs.contains_key(ws)
    }

    /// Setup scripts for a new workspace, or only `repo`'s for one just added, in the background.
    /// `then`'s prompt is sent to its chat once they succeed, so the agent never works on a
    /// half-set-up worktree; if they fail or the chat is busy, it waits in the composer.
    /// False when there's nothing to run (and `then` wasn't taken).
    pub(super) fn run_setup(
        &mut self,
        ws: &Workspace,
        repo: Option<&str>,
        then: Option<(String, Entity<Chat>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let jobs = scripts::jobs(&self.state, ws, Phase::Setup, repo);
        if jobs.is_empty() {
            return false;
        }
        let name = ws.name.clone();
        window.push_notification(format!("Running setup scripts in {name}…"), cx);
        // Archiving waits for it: the worktree mustn't go while setup writes to it.
        self.setting_up.push(name.clone());
        cx.spawn_in(window, async move |this, cx| {
            let ws = name.clone();
            let result = (cx.background_executor())
                .spawn(async move { scripts::run_all(&ws, &jobs) })
                .await;
            this.update_in(cx, |s, window, cx| {
                if let Some(i) = s.setting_up.iter().position(|n| *n == name) {
                    s.setting_up.remove(i);
                }
                let ok = result.is_ok();
                match result {
                    Ok(()) => window.push_notification(
                        Notification::success(format!("Setup done in {name}")),
                        cx,
                    ),
                    Err(e) => script_failed(&name, e, window, cx),
                }
                if let Some((p, chat)) = then {
                    chat.update(cx, |c, cx| {
                        if !(ok && c.ask(&p, window, cx)) {
                            c.composer.update(cx, |t, cx| {
                                // Keep what was typed meanwhile, after the prompt.
                                let draft = t.value().trim().to_string();
                                let text = match draft.is_empty() || draft == p.trim() {
                                    true => p,
                                    false => format!("{p}\n\n{draft}"),
                                };
                                t.set_value(text, window, cx)
                            });
                        }
                    });
                }
            })
            .ok();
        })
        .detach();
        true
    }

    pub(super) fn show_script_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.active.clone() else {
            return notify_err(window, cx, "Open a workspace first");
        };
        let log = scripts::log_path(&name);
        match log.exists() {
            true => cx.open_with_system(&log),
            false => notify_err(window, cx, "No script has run in this workspace yet"),
        }
    }
}

/// An error notice with the end of the log, kept until dismissed.
pub(super) fn script_failed(ws: &str, e: String, window: &mut Window, cx: &mut App) {
    let n = Notification::error(format!("{e}\n{}", scripts::tail(ws)))
        .title("Script failed")
        .autohide(false);
    window.push_notification(n, cx);
}
