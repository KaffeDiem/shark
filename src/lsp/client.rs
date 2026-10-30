//! Ordered JSON-RPC over stdio. No pipe I/O or process waits run on the UI thread.
use anyhow::{Context as _, Result, anyhow, bail};
use lsp_server::{Message, Notification, ReqQueue, RequestId, Response};
use lsp_types::{InitializeResult, PositionEncodingKind, ServerCapabilities};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read as _},
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
/// Larger frames are treated as a corrupt header instead of being allocated.
const MAX_FRAME: usize = 64 << 20;
/// Enough of a crashed server's stderr to say why, without retaining project output.
const STDERR_TAIL: usize = 2048;
/// A single frame write blocked this long means the server stopped reading, not that it is busy.
const WRITE_STALL: Duration = if cfg!(test) {
    Duration::from_secs(3)
} else {
    REQUEST_TIMEOUT
};

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
    capabilities: Mutex<Option<ServerCapabilities>>,
    root: Value,
    settings: Value,
    next_token: AtomicU64,
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
        // Unbounded: a server busy on its main thread is not dead, and request timeouts still apply.
        let (write_tx, write_rx) = mpsc::channel::<Message>();
        let (events_tx, events) = async_channel::unbounded();
        let (stopped_tx, stopped) = async_channel::bounded::<()>(1);
        let reader_tx = tx.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader) {
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
        let writing = Arc::new(Mutex::new(None::<Instant>));
        let writer_writing = writing.clone();
        thread::spawn(move || {
            let mut stdin = stdin;
            while let Ok(message) = write_rx.recv() {
                let exiting = matches!(&message, Message::Notification(n) if n.method == "exit");
                *writer_writing.lock().unwrap() = Some(Instant::now());
                let written = message.write(&mut stdin);
                *writer_writing.lock().unwrap() = None;
                if let Err(error) = written {
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
        // Draining stderr avoids deadlock; only a bounded tail is kept for the failure reason.
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let (stderr_done_tx, stderr_done) = mpsc::channel::<()>();
        let tail = stderr_tail.clone();
        thread::spawn(move || {
            let _done = stderr_done_tx;
            let mut chunk = [0; 4096];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let mut tail = tail.lock().unwrap();
                        tail.extend_from_slice(&chunk[..read]);
                        let excess = tail.len().saturating_sub(STDERR_TAIL);
                        tail.drain(..excess);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
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
                worker_settings,
                worker_root,
                Pipes {
                    stderr: stderr_tail,
                    stderr_done,
                    writing,
                },
            );
            drop(stopped_tx);
        });
        let client = Self(Arc::new(Handle {
            tx,
            events,
            stopped,
            capabilities: Mutex::new(None),
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

    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.0.capabilities.lock().unwrap().clone()
    }

    pub fn encoding(&self) -> PositionEncodingKind {
        self.capabilities()
            .and_then(|c| c.position_encoding)
            .unwrap_or(PositionEncodingKind::UTF16)
    }

    #[cfg(test)]
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
        let result: InitializeResult =
            serde_json::from_value(self.request("initialize", params).await?)?;
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
        *self.0.capabilities.lock().unwrap() = Some(result.capabilities.clone());
        self.notify("initialized", json!({}))?;
        self.notify(
            "workspace/didChangeConfiguration",
            json!({"settings":self.0.settings}),
        )?;
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
    #[cfg(test)]
    pub fn shutdown(&self) {
        let _ = self.0.tx.send(CommandMessage::Shutdown);
    }
}

/// `Message::read`, except that a corrupt length cannot make Shark allocate it.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Message>> {
    let invalid = |error: String| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
    let mut size = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.by_ref().take(4096).read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line
            .strip_suffix("\r\n")
            .ok_or_else(|| invalid(format!("malformed header: {line:?}")))?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(": ")
            .ok_or_else(|| invalid(format!("malformed header: {header:?}")))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            size = Some(value.parse::<usize>().map_err(|e| invalid(e.to_string()))?);
        }
    }
    let size = size.ok_or_else(|| invalid("no Content-Length".into()))?;
    if size > MAX_FRAME {
        return Err(invalid(format!("{size}-byte message exceeds the limit")));
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| invalid(format!("malformed LSP payload: {e}")))
}

/// What the coordinator observes of the pipe threads.
struct Pipes {
    stderr: Arc<Mutex<Vec<u8>>>,
    /// Disconnects at stderr EOF.
    stderr_done: mpsc::Receiver<()>,
    /// When the frame write in progress started.
    writing: Arc<Mutex<Option<Instant>>>,
}

fn coordinate(
    mut child: Child,
    rx: mpsc::Receiver<CommandMessage>,
    tx: mpsc::Sender<CommandMessage>,
    writer: mpsc::Sender<Message>,
    events: async_channel::Sender<Event>,
    settings: Value,
    root: Value,
    Pipes {
        stderr,
        stderr_done,
        writing,
    }: Pipes,
) {
    let mut queue: ReqQueue<(), Pending> = ReqQueue::default();
    let mut outgoing: HashMap<u64, (RequestId, Instant)> = HashMap::new();
    let mut incoming: HashMap<RequestId, Instant> = HashMap::new();
    let mut shutdown: Option<(RequestId, Instant)> = None;
    let mut terminated = false;
    let mut reason = "Language server stopped".to_string();
    loop {
        // Sleep until the next deadline. The idle cap only bounds how late an exit is noticed
        // when another process keeps the server's stdout open.
        let wake = outgoing
            .values()
            .map(|(_, deadline)| *deadline)
            .chain(incoming.values().map(|started| *started + REQUEST_TIMEOUT))
            .chain(shutdown.as_ref().map(|(_, deadline)| *deadline))
            .fold(Instant::now() + Duration::from_secs(1), Instant::min);
        let command = rx.recv_timeout(wake.saturating_duration_since(Instant::now()));
        match command {
            Ok(CommandMessage::Request {
                token,
                method,
                params,
                reply,
                timeout,
            }) if shutdown.is_none() => {
                let deadline = Instant::now() + timeout;
                let request = queue
                    .outgoing
                    .register(method, params, Pending { token, reply });
                outgoing.insert(token, (request.id.clone(), deadline));
                // A writer that stopped has already queued PipeClosed with its reason.
                let _ = writer.send(request.into());
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
                incoming.insert(id.clone(), Instant::now());
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
                    // Forwarded to the editor as notifications; the server gets a null reply ("dismissed").
                    "window/showMessageRequest"
                    | "workspace/diagnostic/refresh"
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
                // A request that timed out was already answered; drop the editor's late reply.
                if incoming.remove(&response.id).is_some() {
                    let _ = writer.send(response.into());
                }
            }
            Ok(CommandMessage::Inbound(Message::Notification(notification))) => {
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
                terminated = true;
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
        let now = Instant::now();
        let expired: Vec<_> = outgoing
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            if let Some((id, _)) = outgoing.remove(&token) {
                if let Some(pending) = queue.outgoing.complete(id.clone()) {
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
        // The idle wakeup cap bounds how late this is noticed.
        if writing
            .lock()
            .unwrap()
            .is_some_and(|since| now.duration_since(since) >= WRITE_STALL)
        {
            reason = "Language server stopped reading its input".into();
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
    if shutdown.is_none() && !terminated {
        // The last stderr lines usually say why the server failed.
        let _ = stderr_done.recv_timeout(Duration::from_millis(100));
        let tail = String::from_utf8_lossy(&stderr.lock().unwrap())
            .trim()
            .to_string();
        if !tail.is_empty() {
            reason = format!("{reason}\n{tail}");
        }
    }
    for (_, (id, _)) in outgoing {
        if let Some(pending) = queue.outgoing.complete(id) {
            let _ = pending.reply.try_send(Err(anyhow!(reason.clone())));
        }
    }
    let _ = events.try_send(Event::Stopped(reason));
    events.close();
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
