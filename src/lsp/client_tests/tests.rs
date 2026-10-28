use super::*;
use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Wake, Waker},
};

struct Unpark(thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}
fn block_on<T>(future: impl Future<Output = T>) -> T {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => return result,
            Poll::Pending => thread::park_timeout(Duration::from_secs(1)),
        }
    }
}

struct Peer {
    client: Client,
    log: std::path::PathBuf,
}
impl Peer {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let log = std::env::temp_dir().join(format!(
            "shark-lsp-peer-{}-{}.jsonl",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new("/usr/bin/python3");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/lsp/client_tests/server.py"
            ))
            .arg(&log);
        let client = Client::start(
            command,
            &std::env::temp_dir(),
            json!({"python":{"analysis":{"typeCheckingMode":"strict"}}}),
        )
        .unwrap();
        Self { client, log }
    }
    fn messages(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn wait_for(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(message) = self.messages().into_iter().find(&predicate) {
                return message;
            }
            assert!(
                Instant::now() < deadline,
                "fake peer did not receive expected message: {:?}",
                self.messages()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.client.shutdown();
        let _ = std::fs::remove_file(&self.log);
    }
}

#[test]
fn lifecycle_orders_unsaved_text_requests_configuration_and_diagnostics() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    assert_eq!(peer.client.encoding(), PositionEncodingKind::UTF16);
    let initialize = peer.wait_for(|v| v["method"] == "initialize");
    // Pyrefly returns no outline unless the client advertises hierarchy support.
    assert_eq!(
        initialize["params"]["capabilities"]["textDocument"]["documentSymbol"]["hierarchicalDocumentSymbolSupport"],
        true
    );
    let uri = "file:///tmp/example.py";
    peer.client
        .notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"python","version":1,"text":"old"}}),
        )
        .unwrap();
    peer.client.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"emoji 🦈\r\nλ"}]})).unwrap();
    let completion = block_on(peer.client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":0,"character":0}}),
    ))
    .unwrap();
    assert_eq!(completion, "emoji 🦈\r\nλ");
    let event = block_on(peer.client.events().recv()).unwrap();
    assert!(
        matches!(event, Event::Notification { method, params } if method == "textDocument/publishDiagnostics" && params["version"] == 2)
    );
    let config = peer.wait_for(|v| v["id"] == "config");
    assert_eq!(config["result"][0]["typeCheckingMode"], "strict");
    assert!(config["result"][1]["python"].is_object());
    peer.client
        .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
        .unwrap();
    peer.client.shutdown();
    peer.wait_for(|v| v["method"] == "exit");
    let methods: Vec<_> = peer
        .messages()
        .iter()
        .filter_map(|v| v["method"].as_str().map(str::to_string))
        .collect();
    let position = |method| methods.iter().position(|m| m == method).unwrap();
    assert!(position("initialized") < position("textDocument/didOpen"));
    assert!(position("textDocument/didChange") < position("textDocument/completion"));
    assert!(position("textDocument/didClose") < position("shutdown"));
}

#[test]
fn shutdown_flushes_exit_before_closing_the_write_pipe() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    block_on(peer.client.request("test/exitNeedsEof", Value::Null)).unwrap();
    peer.client
        .notify("test/finalNotification", json!({"value":42}))
        .unwrap();
    peer.client.shutdown();
    peer.wait_for(|v| v["exit_followed_by_eof"] == true);
    let messages = peer.messages();
    let at = |method| messages.iter().position(|v| v["method"] == method).unwrap();
    assert!(at("test/finalNotification") < at("shutdown"));
    assert!(at("shutdown") < at("exit"));
    assert_eq!(messages.last().unwrap()["exit_followed_by_eof"], true);
}

#[test]
fn timeout_cancels_request_without_blocking_following_requests() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    let error = block_on(peer.client.request_timeout(
        "test/wait",
        Value::Null,
        Duration::from_millis(50),
    ))
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    peer.wait_for(|v| v["method"] == "$/cancelRequest");
    assert_eq!(
        block_on(peer.client.request("test/echo", json!(42))).unwrap(),
        42
    );
}

#[test]
fn dropped_request_cancels_and_late_response_cannot_complete_another_request() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut request = Box::pin(peer.client.request("test/wait", Value::Null));
    assert!(request.as_mut().poll(&mut context).is_pending());
    drop(request);
    let wait = peer.wait_for(|v| v["method"] == "test/wait");
    let cancel = peer.wait_for(|v| v["method"] == "$/cancelRequest");
    assert_eq!(wait["id"], cancel["params"]["id"]);
    assert_eq!(
        block_on(peer.client.request("test/echo", json!("next"))).unwrap(),
        "next"
    );
}

#[test]
fn server_edits_require_host_acknowledgement() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    block_on(peer.client.request("test/applyEdit", Value::Null)).unwrap();
    let Event::ApplyEdit { params, reply } = block_on(peer.client.events().recv()).unwrap() else {
        panic!("expected apply edit");
    };
    assert!(params["edit"]["changes"].is_object());
    reply.respond(json!({"applied":false,"failureReason":"dirty diff buffer"}));
    let response = peer.wait_for(|v| v["id"] == "apply");
    assert_eq!(response["result"]["applied"], false);
}

#[test]
fn crash_resolves_pending_requests_and_reports_the_stderr_tail() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    let events = peer.client.events();
    let error = block_on(peer.client.request("test/crash", Value::Null)).unwrap_err();
    assert!(
        error.to_string().contains("panicked: index corrupt"),
        "{error}"
    );
    let stopped = loop {
        if let Event::Stopped(reason) = block_on(events.recv()).unwrap() {
            break reason;
        }
    };
    assert!(stopped.contains("panicked: index corrupt"), "{stopped}");
    assert!(!stopped.contains("early noise"), "{stopped}");
}

#[test]
fn a_busy_server_is_not_killed_while_its_input_backs_up() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    peer.client.notify("test/sleep", json!(1)).unwrap();
    // Far more than the pipe buffer, queued while the server is not reading.
    let text = "x".repeat(64 * 1024);
    for _ in 0..64 {
        peer.client
            .notify("test/large", json!({ "text": text }))
            .unwrap();
    }
    assert_eq!(
        block_on(peer.client.request("test/echo", json!("after"))).unwrap(),
        "after"
    );
}

#[test]
fn oversized_or_malformed_frames_are_rejected_before_allocation() {
    let read = |bytes: &[u8]| read_message(&mut std::io::Cursor::new(bytes.to_vec()));
    let error = read(b"Content-Length: 99999999999999\r\n\r\n").unwrap_err();
    assert!(error.to_string().contains("exceeds the limit"));
    assert!(read(b"Content-Length: 2\n\n{}").is_err());
    assert!(read(b"").unwrap().is_none());
    let body = br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    assert!(matches!(
        read(&frame).unwrap(),
        Some(Message::Notification(n)) if n.method == "initialized"
    ));
}

#[test]
fn nested_configuration_is_resolved_without_rewriting_settings() {
    let config = json!({"python":{"analysis":{"strict":true}}});
    assert_eq!(
        configuration(&config, Some("python.analysis")),
        json!({"strict":true})
    );
    assert_eq!(configuration(&config, Some("missing")), Value::Null);
    assert_eq!(configuration(&config, None), config);
}

#[test]
fn last_handle_drop_reaps_a_server_that_ignores_shutdown() {
    let log =
        std::env::temp_dir().join(format!("shark-lsp-last-drop-{}.jsonl", std::process::id()));
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/lsp/client_tests/server.py"
        ))
        .arg(&log);
    let client = Client::start(command, &std::env::temp_dir(), json!({})).unwrap();
    block_on(client.initialize()).unwrap();
    block_on(client.request("test/ignoreShutdown", Value::Null)).unwrap();
    let pid = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .find_map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v["pid"].as_i64())
        })
        .unwrap() as i32;
    let events = client.events();
    let started = Instant::now();
    drop(client);
    while let Ok(event) = block_on(events.recv()) {
        if matches!(event, Event::Stopped(_)) {
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    // SAFETY: signal zero checks the process's existence and does not send a signal.
    while unsafe { kill(pid, 0) } == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(unsafe { kill(pid, 0) }, 0, "server was not reaped");
    let _ = std::fs::remove_file(log);
}

#[test]
fn completion_resolve_retains_item_identity_and_supplies_auto_import_edits() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    let uri = "file:///tmp/resolve.py";
    peer.client.notify("textDocument/didOpen", json!({"textDocument":{"uri":uri,"languageId":"python","version":1,"text":"# resolve-completion\nsq"}})).unwrap();
    let items = block_on(peer.client.request(
        "textDocument/completion",
        json!({"textDocument":{"uri":uri},"position":{"line":1,"character":2}}),
    ))
    .unwrap();
    let resolved = block_on(
        peer.client
            .request("completionItem/resolve", items[0].clone()),
    )
    .unwrap();
    assert_eq!(resolved["data"]["token"], "math.sqrt");
    assert_eq!(resolved["textEdit"]["newText"], "sqrt");
    assert_eq!(
        resolved["additionalTextEdits"][0]["newText"],
        "from math import sqrt\n"
    );
}

#[test]
fn immediate_termination_reaps_without_the_graceful_shutdown_timeout() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    block_on(peer.client.request("test/ignoreShutdown", Value::Null)).unwrap();
    let pid = peer
        .messages()
        .iter()
        .find_map(|v| v["pid"].as_i64())
        .unwrap() as i32;
    let started = Instant::now();
    peer.client.terminate();
    block_on(peer.client.stopped());
    assert!(started.elapsed() < Duration::from_secs(1));
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: signal zero only checks existence; stopped() waits until wait() has reaped the child.
    assert_ne!(unsafe { kill(pid, 0) }, 0);
}

#[test]
fn app_quit_can_reap_a_coordinator_after_its_last_editor_closed() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    block_on(peer.client.request("test/ignoreShutdown", Value::Null)).unwrap();
    let cleanup = CLIENTS
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .iter()
        .find(|entry| entry.stopped.same_channel(&peer.client.0.stopped))
        .unwrap()
        .clone();
    let pid = peer
        .messages()
        .iter()
        .find_map(|v| v["pid"].as_i64())
        .unwrap() as i32;
    drop(peer);
    let started = Instant::now();
    cleanup.tx.send(CommandMessage::Terminate).unwrap();
    let _ = block_on(cleanup.stopped.recv());
    assert!(started.elapsed() < Duration::from_secs(1));
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: signal zero checks existence without sending a signal.
    assert_ne!(unsafe { kill(pid, 0) }, 0);
}
