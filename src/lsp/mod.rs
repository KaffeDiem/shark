//! Shared, lazy language sessions. Editors own sessions; the workspace holds only weak references.
pub mod client;
pub mod document;
pub mod providers;
pub mod tools;
pub mod watch;

use gpui_kit::*;
use serde_json::Value;
use std::{cell::RefCell, collections::HashSet};

pub enum SessionEvent {
    Notification {
        method: String,
        params: Value,
        companion: bool,
    },
    Request(RefCell<Option<client::Event>>),
}

pub struct Session {
    pub project: tools::Project,
    pub client: Option<client::Client>,
    pub companion: Option<client::Client>,
    pub status: String,
    pub detail: Option<String>,
    pub generation: u64,
    pub watcher: Option<watch::Watcher>,
    companion_watcher: Option<watch::Watcher>,
    pub choices: Vec<std::path::PathBuf>,
    progress: HashSet<String>,
    task: Task<()>,
    companion_task: Task<()>,
    watch_tasks: Vec<Task<()>>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn with_client(project: tools::Project, client: client::Client) -> Self {
        Self {
            project,
            client: Some(client),
            companion: None,
            status: "Ready".into(),
            detail: None,
            generation: 1,
            watcher: None,
            companion_watcher: None,
            choices: vec![],
            progress: HashSet::new(),
            task: Task::ready(()),
            companion_task: Task::ready(()),
            watch_tasks: vec![],
        }
    }

    pub fn new(project: tools::Project, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut session = Self {
            project,
            client: None,
            companion: None,
            status: "Starting".into(),
            detail: None,
            generation: 0,
            watcher: None,
            companion_watcher: None,
            choices: vec![],
            progress: HashSet::new(),
            task: Task::ready(()),
            companion_task: Task::ready(()),
            watch_tasks: vec![],
        };
        session.restart(window, cx);
        session
    }

    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.client = None;
        self.companion = None;
        self.companion_task = Task::ready(());
        self.watch_tasks.clear();
        self.watcher = None;
        self.companion_watcher = None;
        self.generation += 1;
        self.status = "Starting".into();
        self.detail = None;
        self.choices.clear();
        self.progress.clear();
        let project = self.project.clone();
        self.task = cx.spawn_in(window, async move |this, cx| {
            let prepared = cx.background_executor().spawn(async move {
                let launch = tools::prepare(&project)?;
                let mut command = std::process::Command::new(&launch.program);
                command.args(&launch.args).envs(launch.env);
                let client = client::Client::start(command, &launch.root, launch.settings)?;
                let companion = launch.companion.map(|launch| {
                    let tools::Launch { program, args, env, root, settings, initialization_options, .. } = *launch;
                    let mut command = std::process::Command::new(program);
                    command.args(args).envs(env);
                    client::Client::start(command, &root, settings).map(|client| (client, initialization_options))
                }).transpose()?;
                Ok::<_, anyhow::Error>((client, launch.initialization_options, launch.detail, companion))
            }).await;
            let result = match prepared {
                Ok((client, options, detail, companion)) => {
                    async {
                        client.initialize_with_options(options).await?;
                        let companion = if let Some((client, options)) = companion {
                            client.initialize_with_options(options).await?;
                            Some(client)
                        } else { None };
                        Ok((client, detail, companion))
                    }.await
                }
                Err(error) => Err(error),
            };
            let (client, detail, companion) = match result {
                Ok(ready) => ready,
                Err(error) => {
                    let _ = this.update(cx, |s, cx| {
                        s.status = "Setup needed".into();
                        s.detail = Some(format!("{error:#}"));
                        s.choices = error.downcast_ref::<tools::XcodeChoice>().map(|e| e.options.clone()).unwrap_or_default();
                        cx.notify();
                    });
                    return;
                }
            };
            let events = client.events();
            if this.update(cx, |s, cx| {
                s.status = if detail.is_some() { "Preparing index" } else { "Ready" }.into();
                s.detail = detail;
                for (client, is_companion) in [(Some(&client), false), (companion.as_ref(), true)] {
                    let Some(client) = client else { continue };
                    match watch::Watcher::new(&s.project.worktree, client.clone()) {
                        Ok(watcher) => {
                            let errors = watcher.errors();
                            s.watch_tasks.push(cx.spawn(async move |this, cx| {
                                while let Ok(error) = errors.recv().await {
                                    if this.update(cx, |s, cx| {
                                        s.status = "Setup needed".into();
                                        s.detail = Some(error);
                                        cx.notify();
                                    }).is_err() { break; }
                                }
                            }));
                            if is_companion { s.companion_watcher = Some(watcher); } else { s.watcher = Some(watcher); }
                        }
                        Err(error) => { s.status = "Setup needed".into(); s.detail = Some(format!("File watching: {error}")); }
                    }
                }
                s.client = Some(client);
                if let Some(companion) = &companion {
                    let events = companion.events();
                    s.companion_task = cx.spawn(async move |this, cx| {
                        while let Ok(event) = events.recv().await {
                            if this.update(cx, |s, cx| match event {
                                client::Event::Notification { method, params } => cx.emit(SessionEvent::Notification { method, params, companion: true }),
                                client::Event::Stopped(error) => {
                                    s.status = "Setup needed".into();
                                    s.detail = Some(format!("Ruff: {error}"));
                                    s.companion = None;
                                    s.companion_watcher = None;
                                    s.generation += 1;
                                    cx.notify();
                                }
                                client::Event::RegisterCapability { params, reply } => {
                                    match s.companion_watcher.as_ref().ok_or_else(|| anyhow::anyhow!("Ruff file watcher is unavailable")).and_then(|watcher| watcher.register(&params)) {
                                        Ok(ready) => cx.spawn(async move |_, _| {
                                            match ready.recv().await {
                                                Ok(Ok(())) => reply.respond(Value::Null),
                                                Ok(Err(error)) => reply.reject(error),
                                                Err(_) => reply.reject("Ruff file watcher stopped"),
                                            }
                                        }).detach(),
                                        Err(error) => reply.reject(error.to_string()),
                                    }
                                }
                                client::Event::UnregisterCapability { params, reply } => {
                                    match s.companion_watcher.as_ref().ok_or_else(|| anyhow::anyhow!("Ruff file watcher is unavailable")).and_then(|watcher| watcher.unregister(&params)) {
                                        Ok(()) => reply.respond(Value::Null),
                                        Err(error) => reply.reject(error.to_string()),
                                    }
                                }
                                client::Event::ApplyEdit { reply, .. } => reply.respond(serde_json::json!({"applied":false,"failureReason":"Use Ruff quick fixes from the editor"})),
                                _ => {}
                            }).is_err() { break; }
                        }
                    });
                }
                s.companion = companion;
                cx.notify();
            }).is_err() { return; }
            while let Ok(event) = events.recv().await {
                if this.update(cx, |s, cx| {
                    match event {
                        client::Event::Notification { method, params } => {
                            if method == "$/progress" {
                                let token = params["token"].to_string();
                                match params["value"]["kind"].as_str() {
                                    Some("begin") => { s.progress.insert(token); }
                                    Some("end") => { s.progress.remove(&token); }
                                    _ => {}
                                }
                                s.status = progress_status(&s.status, !s.progress.is_empty() || s.detail.is_some()).into();
                                cx.notify();
                            }
                            cx.emit(SessionEvent::Notification { method, params, companion: false });
                        }
                        client::Event::Stopped(error) => {
                            s.status = "Setup needed".into();
                            s.detail = Some(error);
                            s.client = None;
                            s.companion = None;
                            s.watcher = None;
                            s.companion_watcher = None;
                            s.watch_tasks.clear();
                            s.companion_task = Task::ready(());
                            s.generation += 1;
                            cx.notify();
                        }
                        event => cx.emit(SessionEvent::Request(RefCell::new(Some(event)))),
                    }
                }).is_err() { break; }
            }
        });
        cx.notify();
    }
}

fn progress_status(current: &str, indexing: bool) -> &'static str {
    if current == "Setup needed" {
        "Setup needed"
    } else if indexing {
        "Preparing index"
    } else {
        "Ready"
    }
}

#[cfg(test)]
mod tests {
    use super::progress_status;
    #[test]
    fn progress_does_not_hide_setup_failures() {
        assert_eq!(progress_status("Setup needed", true), "Setup needed");
        assert_eq!(progress_status("Setup needed", false), "Setup needed");
        assert_eq!(progress_status("Ready", true), "Preparing index");
        assert_eq!(progress_status("Preparing index", false), "Ready");
    }
}
