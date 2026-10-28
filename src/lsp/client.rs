//! Ordered JSON-RPC over stdio. No pipe I/O or process waits run on the UI thread.
use anyhow::{Context as _, Result, anyhow, bail};
use lsp_server::{Message, Notification, ReqQueue, RequestId, Response};
use lsp_types::{InitializeResult, PositionEncodingKind, ServerCapabilities};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::BufReader,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    PreparingIndex,
    Ready,
    SetupNeeded(String),
    Stopped,
}

#[derive(Debug)]
pub enum Event {
    Notification {
        method: String,
        params: Value,
    },
    /// The host must preflight the whole edit before replying with `applied: true`.
    ApplyEdit {
        params: Value,
        reply: Reply,
    },
    ShowDocument {
        params: Value,
        reply: Reply,
    },
    /// Registrations are not acknowledged until the host has installed the requested watcher.
    RegisterCapability {
        params: Value,
        reply: Reply,
    },
    UnregisterCapability {
        params: Value,
        reply: Reply,
    },
    Stopped(String),
}

/// An unanswered server request is rejected when its reply handle is dropped.
#[derive(Debug)]
pub struct Reply {
    id: Option<RequestId>,
    tx: mpsc::Sender<CommandMessage>,
}

impl Reply {
    pub fn respond(mut self, result: Value) {
        if let Some(id) = self.id.take() {
            let _ = self
                .tx
                .send(CommandMessage::Reply(Response::new_ok(id, result)));
        }
    }

    pub fn reject(mut self, reason: impl Into<String>) {
        if let Some(id) = self.id.take() {
            let _ = self.tx.send(CommandMessage::Reply(Response::new_err(
                id,
                lsp_server::ErrorCode::RequestFailed as i32,
                reason.into(),
            )));
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.tx.send(CommandMessage::Reply(Response::new_err(
                id,
                lsp_server::ErrorCode::RequestFailed as i32,
                "The editor cannot handle this server request".into(),
            )));
        }
    }
}

#[derive(Clone)]
pub struct Client(Arc<Handle>);

#[derive(Clone)]
struct Cleanup {
    tx: mpsc::Sender<CommandMessage>,
    stopped: async_channel::Receiver<()>,
}

// Coordinators outlive the last editor during graceful shutdown; keep their control channels,
// not Client handles, so app quit can reap them without preventing last-handle shutdown.
static CLIENTS: OnceLock<Mutex<Vec<Cleanup>>> = OnceLock::new();
static QUITTING: AtomicBool = AtomicBool::new(false);

struct Handle {
    tx: mpsc::Sender<CommandMessage>,
    events: async_channel::Receiver<Event>,
    stopped: async_channel::Receiver<()>,
    state: Arc<Mutex<State>>,
    root: Value,
    settings: Value,
    next_token: AtomicU64,
}

struct State {
    status: Status,
    capabilities: Option<ServerCapabilities>,
    progress: std::collections::HashSet<String>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        // The coordinator owns and reaps the process even if the UI has already closed.
        let _ = self.tx.send(CommandMessage::Shutdown);
    }
}

#[derive(Debug)]
enum CommandMessage {
    Request {
        token: u64,
        method: String,
        params: Value,
        reply: async_channel::Sender<Result<Value>>,
        timeout: Duration,
    },
    Notify(Notification),
    Cancel(u64),
    Inbound(Message),
    PipeClosed(String),
    Reply(Response),
    Shutdown,
    Terminate,
}

struct Pending {
    token: u64,
    deadline: Instant,
    reply: async_channel::Sender<Result<Value>>,
}

struct CancelOnDrop {
    token: u64,
    tx: mpsc::Sender<CommandMessage>,
    complete: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.tx.send(CommandMessage::Cancel(self.token));
        }
    }
}

impl Client {
    /// Spawns only; call from a background executor. `command` must already select the toolchain.
    pub fn start(mut command: Command, root: &Path, settings: Value) -> Result<Self> {
        use std::os::unix::process::CommandExt as _;
        if QUITTING.load(Ordering::Acquire) {
            bail!("Shark is shutting down");
        }
        let root_url = url::Url::from_directory_path(root)
            .map_err(|_| anyhow!("LSP root is not an absolute directory: {}", root.display()))?;
        command
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.process_group(0);
        let mut child = command.spawn().context("Could not start language server")?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = mpsc::channel();
        let (write_tx, write_rx) = mpsc::sync_channel::<Message>(32);
        let (events_tx, events) = async_channel::unbounded();
        let (stopped_tx, stopped) = async_channel::bounded::<()>(1);
        let state = Arc::new(Mutex::new(State {
            status: Status::Starting,
            capabilities: None,
            progress: Default::default(),
        }));
        let reader_tx = tx.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match Message::read(&mut reader) {
                    Ok(Some(message)) => {
                        if reader_tx.send(CommandMessage::Inbound(message)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => {
                        let _ = reader_tx.send(CommandMessage::PipeClosed(
                            "Language server closed stdout".into(),
                        ));
                        break;
                    }
                    Err(error) => {
                        let _ = reader_tx.send(CommandMessage::PipeClosed(format!(
                            "Language server protocol error: {error}"
                        )));
                        break;
                    }
                }
            }
        });
        let writer_tx = tx.clone();
        thread::spawn(move || {
            let mut stdin = stdin;
            while let Ok(message) = write_rx.recv() {
                let exiting = matches!(&message, Message::Notification(n) if n.method == "exit");
                if let Err(error) = message.write(&mut stdin) {
                    let _ = writer_tx.send(CommandMessage::PipeClosed(format!(
                        "Language server write failed: {error}"
                    )));
                    break;
                }
                // Finish the transport only after the final frame has been flushed.
                if exiting {
                    break;
                }
            }
        });
        // Draining stderr avoids deadlock without retaining unbounded or sensitive project output.
        thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
        let worker_state = state.clone();
        let worker_tx = tx.clone();
        let worker_settings = settings.clone();
        let root = json!({"uri":root_url.as_str(),"name":root.file_name().unwrap_or_default().to_string_lossy()});
        let worker_root = root.clone();
        thread::spawn(move || {
            coordinate(
                child,
                rx,
                worker_tx,
                write_tx,
                events_tx,
                worker_state,
                worker_settings,
                worker_root,
            );
            drop(stopped_tx);
        });
        let client = Self(Arc::new(Handle {
            tx,
            events,
            stopped,
            state,
            root,
            settings,
            next_token: AtomicU64::new(1),
        }));
        let mut clients = CLIENTS.get_or_init(Default::default).lock().unwrap();
        clients.retain(|client| !client.stopped.is_closed());
        clients.push(Cleanup {
            tx: client.0.tx.clone(),
            stopped: client.0.stopped.clone(),
        });
        if QUITTING.load(Ordering::Acquire) {
            client.terminate();
        }
        Ok(client)
    }

    /// One consumer per shared project session; that host fans notifications out to its documents.
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.0.events.clone()
    }

    pub fn status(&self) -> Status {
        self.0.state.lock().unwrap().status.clone()
    }

    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.0.state.lock().unwrap().capabilities.clone()
    }

    pub fn encoding(&self) -> PositionEncodingKind {
        self.capabilities()
            .and_then(|c| c.position_encoding)
            .unwrap_or(PositionEncodingKind::UTF16)
    }

    pub async fn initialize(&self) -> Result<InitializeResult> {
        self.initialize_with_options(Value::Null).await
    }

    pub async fn initialize_with_options(&self, options: Value) -> Result<InitializeResult> {
        let params = json!({
            "processId":std::process::id(),
            "clientInfo":{"name":"Shark","version":env!("CARGO_PKG_VERSION")},
            "rootUri":self.0.root["uri"],
            "workspaceFolders":[self.0.root],
            "capabilities":{
                "general":{"positionEncodings":["utf-8","utf-16","utf-32"]},
                "workspace":{
                    "applyEdit":true,"workspaceFolders":true,"configuration":true,
                    "workspaceEdit":{"documentChanges":true,"failureHandling":"transactional"},
                    "didChangeWatchedFiles":{"dynamicRegistration":true,"relativePatternSupport":true}
                },
                "window":{"workDoneProgress":true,"showDocument":{"support":true}},
                "textDocument":{
                    "synchronization":{"didSave":true},
                    "completion":{"completionItem":{"snippetSupport":true,"resolveSupport":{"properties":["documentation","detail","additionalTextEdits"]}}},
                    "hover":{"contentFormat":["markdown","plaintext"]},
                    "signatureHelp":{"signatureInformation":{"documentationFormat":["markdown","plaintext"]}},
                    "definition":{"linkSupport":true},
                    "documentSymbol":{"hierarchicalDocumentSymbolSupport":true},
                    "publishDiagnostics":{"versionSupport":true,"relatedInformation":true},
                    "diagnostic":{"relatedDocumentSupport":true},
                    "codeAction":{"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["","quickfix","refactor","source.organizeImports","source.fixAll"]}}},
                    "rename":{"prepareSupport":true}
                }
            },
            "initializationOptions":options,
            "trace":"off"
        });
        let result = self.request("initialize", params).await;
        let result: InitializeResult =
            match result.and_then(|v| serde_json::from_value(v).map_err(Into::into)) {
                Ok(result) => result,
                Err(error) => {
                    self.0.state.lock().unwrap().status = Status::SetupNeeded(error.to_string());
                    return Err(error);
                }
            };
        let encoding = result
            .capabilities
            .position_encoding
            .clone()
            .unwrap_or(PositionEncodingKind::UTF16);
        if ![
            PositionEncodingKind::UTF8,
            PositionEncodingKind::UTF16,
            PositionEncodingKind::UTF32,
        ]
        .contains(&encoding)
        {
            bail!(
                "Language server selected unsupported position encoding: {}",
                encoding.as_str()
            );
        }
        self.0.state.lock().unwrap().capabilities = Some(result.capabilities.clone());
        self.notify("initialized", json!({}))?;
        self.notify(
            "workspace/didChangeConfiguration",
            json!({"settings":self.0.settings}),
        )?;
        let mut state = self.0.state.lock().unwrap();
        state.status = if state.progress.is_empty() {
            Status::Ready
        } else {
            Status::PreparingIndex
        };
        Ok(result)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, REQUEST_TIMEOUT).await
    }

    async fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let token = self.0.next_token.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = async_channel::bounded(1);
        self.0
            .tx
            .send(CommandMessage::Request {
                token,
                method: method.into(),
                params,
                reply: tx,
                timeout,
            })
            .map_err(|_| anyhow!("Language server stopped"))?;
        let mut cancel = CancelOnDrop {
            token,
            tx: self.0.tx.clone(),
            complete: false,
        };
        let result = rx
            .recv()
            .await
            .map_err(|_| anyhow!("Language server stopped"))?;
        cancel.complete = true;
        result
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.0
            .tx
            .send(CommandMessage::Notify(Notification::new(
                method.into(),
                params,
            )))
            .map_err(|_| anyhow!("Language server stopped"))
    }

    /// App quit has a short deadline; skip the graceful handshake and reap immediately.
    pub fn terminate(&self) {
        let _ = self.0.tx.send(CommandMessage::Terminate);
    }

    pub async fn stopped(&self) {
        // Closing a channel wakes every waiter, including clones from different editors.
        let _ = self.0.stopped.recv().await;
    }

    /// Include clients still initializing, which are not attached to an editor yet.
    pub async fn terminate_all() {
        QUITTING.store(true, Ordering::Release);
        let clients = CLIENTS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .clone();
        for client in &clients {
            let _ = client.tx.send(CommandMessage::Terminate);
        }
        for client in clients {
            let _ = client.stopped.recv().await;
        }
    }

    /// Stops this shared session. Existing request futures resolve with an error.
    pub fn shutdown(&self) {
        let _ = self.0.tx.send(CommandMessage::Shutdown);
    }
}

// Never block the coordinator behind a stalled child: it must still cancel and reap it.
struct Outbox {
    tx: mpsc::SyncSender<Message>,
    failed: std::cell::Cell<bool>,
}
impl Outbox {
    fn send(&self, message: Message) -> Result<()> {
        self.tx.try_send(message).map_err(|_| {
            self.failed.set(true);
            anyhow!("Language server stopped reading its input")
        })
    }
}

fn coordinate(
    mut child: Child,
    rx: mpsc::Receiver<CommandMessage>,
    tx: mpsc::Sender<CommandMessage>,
    writer: mpsc::SyncSender<Message>,
    events: async_channel::Sender<Event>,
    state: Arc<Mutex<State>>,
    settings: Value,
    root: Value,
) {
    let writer = Outbox {
        tx: writer,
        failed: std::cell::Cell::new(false),
    };
    let mut queue: ReqQueue<Instant, Pending> = ReqQueue::default();
    let mut outgoing: HashMap<u64, (RequestId, Instant)> = HashMap::new();
    let mut incoming: HashMap<RequestId, Instant> = HashMap::new();
    let mut shutdown: Option<(RequestId, Instant)> = None;
    let mut reason = "Language server stopped".to_string();
    loop {
        let command = rx.recv_timeout(Duration::from_millis(25));
        match command {
            Ok(CommandMessage::Request {
                token,
                method,
                params,
                reply,
                timeout,
            }) if shutdown.is_none() => {
                let deadline = Instant::now() + timeout;
                let request = queue.outgoing.register(
                    method,
                    params,
                    Pending {
                        token,
                        deadline,
                        reply,
                    },
                );
                outgoing.insert(token, (request.id.clone(), deadline));
                if writer.send(request.into()).is_err() {
                    break;
                }
            }
            Ok(CommandMessage::Request { reply, .. }) => {
                let _ = reply.try_send(Err(anyhow!("Language server is shutting down")));
            }
            Ok(CommandMessage::Notify(notification)) if shutdown.is_none() => {
                let _ = writer.send(notification.into());
            }
            Ok(CommandMessage::Cancel(token)) => {
                if let Some((id, _)) = outgoing.remove(&token) {
                    queue.outgoing.complete(id.clone());
                    let _ = writer
                        .send(Notification::new("$/cancelRequest".into(), json!({"id":id})).into());
                }
            }
            Ok(CommandMessage::Inbound(Message::Response(response))) => {
                if shutdown.as_ref().is_some_and(|(id, _)| *id == response.id) {
                    let _ = writer.send(Notification::new("exit".into(), Value::Null).into());
                    // Give the writer and server a bounded grace period to finish.
                    shutdown = Some((response.id, Instant::now() + Duration::from_millis(250)));
                } else if let Some(pending) = queue.outgoing.complete(response.id) {
                    outgoing.remove(&pending.token);
                    let result = response
                        .response_result
                        .map_err(|e| anyhow!("{} (LSP {})", e.message, e.code));
                    let _ = pending.reply.try_send(result);
                }
            }
            Ok(CommandMessage::Inbound(Message::Request(request))) => {
                let id = request.id.clone();
                if incoming.contains_key(&id) {
                    reason = "Language server reused an unfinished request id".into();
                    break;
                }
                let now = Instant::now();
                queue.incoming.register(id.clone(), now);
                incoming.insert(id.clone(), now);
                let reply = Reply {
                    id: Some(id),
                    tx: tx.clone(),
                };
                match request.method.as_str() {
                    "workspace/configuration" => {
                        let configs: Vec<_> = request.params["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|item| configuration(&settings, item["section"].as_str()))
                            .collect();
                        reply.respond(json!(configs));
                    }
                    "workspace/workspaceFolders" => reply.respond(json!([root])),
                    "window/workDoneProgress/create" => reply.respond(Value::Null),
                    "workspace/applyEdit" => {
                        let _ = events.try_send(Event::ApplyEdit {
                            params: request.params,
                            reply,
                        });
                    }
                    "window/showDocument" => {
                        let _ = events.try_send(Event::ShowDocument {
                            params: request.params,
                            reply,
                        });
                    }
                    "client/registerCapability" => {
                        let _ = events.try_send(Event::RegisterCapability {
                            params: request.params,
                            reply,
                        });
                    }
                    "client/unregisterCapability" => {
                        let _ = events.try_send(Event::UnregisterCapability {
                            params: request.params,
                            reply,
                        });
                    }
                    "workspace/diagnostic/refresh"
                    | "workspace/semanticTokens/refresh"
                    | "workspace/inlayHint/refresh"
                    | "workspace/codeLens/refresh" => {
                        let _ = events.try_send(Event::Notification {
                            method: request.method,
                            params: request.params,
                        });
                        reply.respond(Value::Null);
                    }
                    _ => reply.reject(format!("Unsupported server request: {}", request.method)),
                }
            }
            Ok(CommandMessage::Reply(response)) => {
                if queue.incoming.complete(&response.id).is_some() {
                    incoming.remove(&response.id);
                    let _ = writer.send(response.into());
                }
            }
            Ok(CommandMessage::Inbound(Message::Notification(notification))) => {
                if notification.method == "$/progress" {
                    let key = notification.params["token"].to_string();
                    let mut state = state.lock().unwrap();
                    match notification.params["value"]["kind"].as_str() {
                        Some("begin") => {
                            state.progress.insert(key);
                        }
                        Some("end") => {
                            state.progress.remove(&key);
                        }
                        _ => {}
                    }
                    if state.capabilities.is_some() {
                        state.status = if state.progress.is_empty() {
                            Status::Ready
                        } else {
                            Status::PreparingIndex
                        };
                    }
                }
                let _ = events.try_send(Event::Notification {
                    method: notification.method,
                    params: notification.params,
                });
            }
            Ok(CommandMessage::PipeClosed(error)) => {
                reason = error;
                break;
            }
            Ok(CommandMessage::Terminate) => {
                shutdown = Some((
                    RequestId::from("shark-terminate".to_string()),
                    Instant::now(),
                ));
                break;
            }
            Ok(CommandMessage::Shutdown) => {
                if shutdown.is_none() {
                    let id = RequestId::from("shark-shutdown".to_string());
                    let _ = writer.send(
                        lsp_server::Request::new(id.clone(), "shutdown".into(), Value::Null).into(),
                    );
                    shutdown = Some((id, Instant::now() + SHUTDOWN_TIMEOUT));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            _ => {}
        }
        if writer.failed.get() {
            reason = "Language server stopped reading its input".into();
            break;
        }
        let now = Instant::now();
        let expired: Vec<_> = outgoing
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            if let Some((id, _)) = outgoing.remove(&token) {
                if let Some(pending) = queue.outgoing.complete(id.clone()) {
                    debug_assert!(pending.deadline <= now);
                    let _ = pending
                        .reply
                        .try_send(Err(anyhow!("Language server request timed out")));
                }
                let _ = writer
                    .send(Notification::new("$/cancelRequest".into(), json!({"id":id})).into());
            }
        }
        let expired: Vec<_> = incoming
            .iter()
            .filter(|(_, started)| now.duration_since(**started) >= REQUEST_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            incoming.remove(&id);
            queue.incoming.complete(&id);
            let _ = writer.send(
                Response::new_err(
                    id,
                    lsp_server::ErrorCode::RequestFailed as i32,
                    "Editor request timed out".into(),
                )
                .into(),
            );
        }
        if shutdown
            .as_ref()
            .is_some_and(|(_, deadline)| now >= *deadline)
        {
            break;
        }
        match child.try_wait() {
            Ok(Some(exit)) => {
                reason = format!("Language server exited: {exit}");
                break;
            }
            Err(error) => {
                reason = format!("Could not inspect language server: {error}");
                break;
            }
            _ => {}
        }
    }
    for (_, (id, _)) in outgoing {
        if let Some(pending) = queue.outgoing.complete(id) {
            let _ = pending.reply.try_send(Err(anyhow!(reason.clone())));
        }
    }
    state.lock().unwrap().status = if shutdown.is_some() {
        Status::Stopped
    } else {
        Status::SetupNeeded(reason.clone())
    };
    let _ = events.try_send(Event::Stopped(reason));
    events.close();
    // Kill the dedicated process group: children may otherwise keep the pipes and threads alive.
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: POSIX kill takes no pointers; the child was placed in its own process group above.
    unsafe {
        kill(-(child.id() as i32), 9);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn configuration(settings: &Value, section: Option<&str>) -> Value {
    match section.filter(|s| !s.is_empty()) {
        None => settings.clone(),
        Some(section) => section
            .split('.')
            .try_fold(settings, |value, key| value.get(key))
            .cloned()
            .unwrap_or(Value::Null),
    }
}

#[cfg(test)]
#[path = "client_tests/tests.rs"]
mod tests;
