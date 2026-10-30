//! Shared, lazy language sessions. Editors own sessions; the workspace holds only weak references.
pub mod client;
pub mod document;
mod install;
pub mod providers;
mod swift;
#[cfg(test)]
pub(crate) mod test_support;
pub mod tools;
pub mod watch;

use gpui_kit::*;
use serde_json::Value;
use std::cell::RefCell;

pub enum SessionEvent {
    Notification {
        method: String,
        params: Value,
        companion: bool,
    },
    Request(RefCell<Option<client::Event>>),
}

/// One `$/progress` token's work, as the server last reported it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Work {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    Preparing,
    Ready,
    SetupNeeded,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Preparing => "Preparing index",
            Self::Ready => "Ready",
            Self::SetupNeeded => "Setup needed",
        }
    }
}

pub struct Session {
    pub project: tools::Project,
    pub client: Option<client::Client>,
    pub companion: Option<client::Client>,
    pub status: Status,
    pub detail: Option<String>,
    pub generation: u64,
    pub watcher: Option<watch::Watcher>,
    companion_watcher: Option<watch::Watcher>,
    pub choices: Vec<std::path::PathBuf>,
    /// Only an Xcode build, not the server, can finish this project's index.
    awaiting_build: bool,
    /// Work in progress by token, oldest first.
    progress: Vec<(String, Work)>,
    task: Task<()>,
    companion_task: Task<()>,
    watch_tasks: Vec<Task<()>>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    fn build(
        project: tools::Project,
        client: Option<client::Client>,
        status: Status,
        generation: u64,
    ) -> Self {
        Self {
            project,
            client,
            companion: None,
            status,
            detail: None,
            generation,
            watcher: None,
            companion_watcher: None,
            choices: vec![],
            awaiting_build: false,
            progress: vec![],
            task: Task::ready(()),
            companion_task: Task::ready(()),
            watch_tasks: vec![],
        }
    }

    #[cfg(all(test, feature = "test-support"))]
    pub(crate) fn with_client(project: tools::Project, client: client::Client) -> Self {
        Self::build(project, Some(client), Status::Ready, 1)
    }

    pub fn new(project: tools::Project, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut session = Self::build(project, None, Status::Starting, 0);
        session.restart(window, cx);
        session
    }

    /// What the server is busy with, for a loading indicator: the newest reported work, or
    /// "Starting" while the server launches. None when idle or broken.
    pub fn activity(&self) -> Option<Work> {
        match self.status {
            Status::Starting => Some(Work {
                title: "Starting".into(),
                ..Default::default()
            }),
            Status::SetupNeeded => None,
            _ => self.progress.last().map(|(_, w)| w.clone()),
        }
    }

    /// The companion is gone; its task is the caller, so it is left running.
    fn drop_companion(&mut self) {
        self.companion = None;
        self.companion_watcher = None;
        self.generation += 1;
    }

    /// Drops both servers and everything that serves them.
    fn drop_servers(&mut self) {
        self.client = None;
        self.watcher = None;
        self.watch_tasks.clear();
        self.companion_task = Task::ready(());
        self.drop_companion();
    }

    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_servers();
        self.status = Status::Starting;
        self.detail = None;
        self.choices.clear();
        self.awaiting_build = false;
        self.progress.clear();
        let project = self.project.clone();
        self.task = cx.spawn_in(window, async move |this, cx| {
            let prepared = cx.background_executor().spawn(async move {
                let launch = tools::prepare(&project)?;
                let client = start(&launch)?;
                let companion = launch.companion.as_deref().map(|c| start(c).map(|client| (client, c.initialization_options.clone())));
                Ok::<_, anyhow::Error>((client, launch, companion))
            }).await;
            let result = match prepared {
                Ok((client, launch, companion)) => {
                    async {
                        client.initialize_with_options(launch.initialization_options).await?;
                        let (companion, note) = initialize_companion(companion).await;
                        let detail = [launch.detail, note].into_iter().flatten().reduce(|a, b| format!("{a}\n{b}"));
                        Ok((client, detail, launch.awaiting_build, companion))
                    }.await
                }
                Err(error) => Err(error),
            };
            let (client, detail, awaiting_build, companion) = match result {
                Ok(ready) => ready,
                Err(error) => {
                    let _ = this.update(cx, |s, cx| {
                        s.status = Status::SetupNeeded;
                        s.detail = Some(format!("{error:#}"));
                        s.choices = error.downcast_ref::<tools::XcodeChoice>().map(|e| e.options.clone()).unwrap_or_default();
                        cx.notify();
                    });
                    return;
                }
            };
            let events = client.events();
            if this.update(cx, |s, cx| {
                s.awaiting_build = awaiting_build;
                s.status = progress_status(Status::Starting, awaiting_build);
                s.detail = detail;
                for (client, is_companion) in [(Some(&client), false), (companion.as_ref(), true)] {
                    if let Some(client) = client {
                        s.attach_watcher(client, is_companion, cx);
                    }
                }
                s.client = Some(client);
                if let Some(companion) = &companion {
                    s.spawn_companion_loop(companion, cx);
                }
                s.companion = companion;
                cx.notify();
            }).is_err() { return; }
            while let Ok(event) = events.recv().await {
                if this.update(cx, |s, cx| {
                    match event {
                        client::Event::Notification { method, params } => {
                            if method == "$/progress" {
                                track_progress(&mut s.progress, &params);
                                s.status = progress_status(s.status, !s.progress.is_empty() || s.awaiting_build);
                                cx.notify();
                            }
                            cx.emit(SessionEvent::Notification { method, params, companion: false });
                        }
                        client::Event::Stopped(error) => {
                            s.status = Status::SetupNeeded;
                            s.detail = Some(error);
                            s.drop_servers();
                            cx.notify();
                        }
                        event @ (client::Event::RegisterCapability { .. } | client::Event::UnregisterCapability { .. }) => {
                            watch_request(s.watcher.as_ref(), event, cx)
                        }
                        event => cx.emit(SessionEvent::Request(RefCell::new(Some(event)))),
                    }
                }).is_err() { break; }
            }
        });
        cx.notify();
    }

    /// Watches the worktree for this server; watcher failures only update the detail line.
    fn attach_watcher(&mut self, client: &client::Client, is_companion: bool, cx: &mut Context<Self>) {
        match watch::Watcher::new(&self.project.worktree, client.clone()) {
            Ok(watcher) => {
                let errors = watcher.errors();
                self.watch_tasks.push(cx.spawn(async move |this, cx| {
                    while let Ok(error) = errors.recv().await {
                        // The server keeps working without change notifications.
                        if this.update(cx, |s, cx| {
                            s.detail = Some(error);
                            cx.notify();
                        }).is_err() { break; }
                    }
                }));
                if is_companion { self.companion_watcher = Some(watcher); } else { self.watcher = Some(watcher); }
            }
            Err(error) => self.detail = Some(format!("File watching: {error}")),
        }
    }

    fn spawn_companion_loop(&mut self, companion: &client::Client, cx: &mut Context<Self>) {
        let events = companion.events();
        self.companion_task = cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |s, cx| match event {
                    client::Event::Notification { method, params } => cx.emit(SessionEvent::Notification { method, params, companion: true }),
                    client::Event::Stopped(error) => {
                        s.detail = Some(tools::companion_note(&anyhow::anyhow!(error)));
                        s.drop_companion();
                        cx.notify();
                    }
                    event @ (client::Event::RegisterCapability { .. } | client::Event::UnregisterCapability { .. }) => {
                        watch_request(s.companion_watcher.as_ref(), event, cx)
                    }
                    client::Event::ApplyEdit { reply, .. } => reply.respond(serde_json::json!({"applied":false,"failureReason":"Use Ruff quick fixes from the editor"})),
                    _ => {}
                }).is_err() { break; }
            }
        });
    }
}

/// Answers a server's file-watcher (un)registration from that server's own watcher.
fn watch_request(
    watcher: Option<&watch::Watcher>,
    event: client::Event,
    cx: &mut Context<Session>,
) {
    let watcher = watcher.ok_or_else(|| anyhow::anyhow!("File watcher unavailable"));
    match event {
        client::Event::RegisterCapability { params, reply } => {
            match watcher.and_then(|w| w.register(&params)) {
                Ok(ready) => cx
                    .spawn(async move |_, _| match ready.recv().await {
                        Ok(Ok(())) => reply.respond(Value::Null),
                        Ok(Err(error)) => reply.reject(error),
                        Err(_) => reply.reject("File watcher stopped before registration"),
                    })
                    .detach(),
                Err(error) => reply.reject(error.to_string()),
            }
        }
        client::Event::UnregisterCapability { params, reply } => {
            match watcher.and_then(|w| w.unregister(&params)) {
                Ok(()) => reply.respond(Value::Null),
                Err(error) => reply.reject(error.to_string()),
            }
        }
        _ => {}
    }
}

fn start(launch: &tools::Launch) -> anyhow::Result<client::Client> {
    let mut command = std::process::Command::new(&launch.program);
    command.args(&launch.args).envs(launch.env.iter().cloned());
    client::Client::start(command, &launch.root, launch.settings.clone())
}

/// The companion only formats and fixes: its failure is a note, never a setup error.
async fn initialize_companion(
    companion: Option<anyhow::Result<(client::Client, Value)>>,
) -> (Option<client::Client>, Option<String>) {
    let result = async {
        let Some((client, options)) = companion.transpose()? else {
            return Ok(None);
        };
        client.initialize_with_options(options).await?;
        Ok(Some(client))
    };
    match result.await {
        Ok(client) => (client, None),
        Err(error) => (None, Some(tools::companion_note(&error))),
    }
}

fn track_progress(progress: &mut Vec<(String, Work)>, params: &Value) {
    let token = params["token"].to_string();
    let value = &params["value"];
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let percentage = value["percentage"].as_u64().map(|p| p.min(100) as u32);
    match value["kind"].as_str() {
        Some("begin") => {
            progress.retain(|(t, _)| *t != token);
            let title = text("title").unwrap_or_else(|| "Working".into());
            let message = text("message");
            progress.push((
                token,
                Work {
                    title,
                    message,
                    percentage,
                },
            ));
        }
        Some("report") => {
            if let Some((_, work)) = progress.iter_mut().find(|(t, _)| *t == token) {
                work.message = text("message").or(work.message.take());
                work.percentage = percentage.or(work.percentage);
            }
        }
        Some("end") => progress.retain(|(t, _)| *t != token),
        _ => {}
    }
}

fn progress_status(current: Status, indexing: bool) -> Status {
    if current == Status::SetupNeeded {
        Status::SetupNeeded
    } else if indexing {
        Status::Preparing
    } else {
        Status::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::{Status, Work, initialize_companion, progress_status, start, tools, track_progress};
    use serde_json::json;

    #[test]
    fn progress_tracks_the_newest_work() {
        let mut p = vec![];
        let send = |p: &mut Vec<_>, token: i64, value| {
            track_progress(p, &json!({"token": token, "value": value}))
        };
        send(
            &mut p,
            1,
            json!({"kind":"begin","title":"Indexing","percentage":0}),
        );
        send(
            &mut p,
            2,
            json!({"kind":"begin","title":"Loading","message":"cargo"}),
        );
        send(
            &mut p,
            1,
            json!({"kind":"report","message":"3/150 (core)","percentage":2}),
        );
        assert_eq!(p.len(), 2);
        send(&mut p, 2, json!({"kind":"end"}));
        let work = Work {
            title: "Indexing".into(),
            message: Some("3/150 (core)".into()),
            percentage: Some(2),
        };
        assert_eq!(p.last().map(|(_, w)| w.clone()), Some(work));
        // A report without a message keeps the last one.
        send(&mut p, 1, json!({"kind":"report","percentage":250}));
        assert_eq!(p[0].1.percentage, Some(100));
        assert_eq!(p[0].1.message.as_deref(), Some("3/150 (core)"));
        send(&mut p, 1, json!({"kind":"end"}));
        assert!(p.is_empty());
    }
    #[test]
    fn progress_does_not_hide_setup_failures() {
        assert_eq!(progress_status(Status::SetupNeeded, true), Status::SetupNeeded);
        assert_eq!(progress_status(Status::SetupNeeded, false), Status::SetupNeeded);
        assert_eq!(progress_status(Status::Ready, true), Status::Preparing);
        assert_eq!(progress_status(Status::Preparing, false), Status::Ready);
        assert_eq!(Status::Preparing.label(), "Preparing index");
    }

    #[test]
    fn companion_that_cannot_start_leaves_a_note_instead_of_failing_setup() {
        let launch = tools::Launch {
            program: "/nonexistent/ruff".into(),
            args: vec!["server".into()],
            env: vec![],
            root: std::env::temp_dir(),
            initialization_options: json!(null),
            settings: json!({}),
            detail: None,
            awaiting_build: false,
            companion: None,
        };
        let companion = Some(start(&launch).map(|client| (client, json!(null))));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready((client, note)) =
            std::pin::pin!(initialize_companion(companion)).poll(&mut cx)
        else {
            panic!("a failed start needs no server round trip");
        };
        assert!(client.is_none());
        let note = note.unwrap();
        assert!(note.starts_with("Ruff unavailable: "), "{note}");
    }
}
