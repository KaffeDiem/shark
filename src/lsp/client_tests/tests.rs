use super::*;
use crate::lsp::test_support::{block_on, fake_server, logged};
use std::task::{Context, Waker};

/// The pid the fake server logged, once it has seen `test/ignoreShutdown`.
fn server_pid(log: &std::path::Path) -> i32 {
    logged(log)
        .iter()
        .find_map(|v| v["pid"].as_i64())
        .expect("fake server did not log its pid") as i32
}

/// Whether the process is gone, polling briefly in case the reaping is still in flight.
fn reaped(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while crate::proc::signal(pid, 0) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    !crate::proc::signal(pid, 0)
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
        let client = Client::start(
            fake_server(&log),
            &std::env::temp_dir(),
            json!({"python":{"analysis":{"typeCheckingMode":"strict"}}}),
        )
        .unwrap();
        Self { client, log }
    }
    fn wait_for(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(message) = logged(&self.log).into_iter().find(&predicate) {
                return message;
            }
            assert!(
                Instant::now() < deadline,
                "fake peer did not receive expected message: {:?}",
                logged(&self.log)
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
    assert_eq!(peer.client.encoding(), Encoding::Utf16);
    let initialize = peer.wait_for(|v| v["method"] == "initialize");
    // Pyrefly returns no outline unless the client advertises hierarchy support.
    assert_eq!(
        initialize["params"]["capabilities"]["textDocument"]["documentSymbol"]["hierarchicalDocumentSymbolSupport"],
        true
    );
    // Servers send workspace/diagnostic/refresh only when the client declares it.
    assert_eq!(
        initialize["params"]["capabilities"]["workspace"]["diagnostics"]["refreshSupport"],
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
    let methods: Vec<_> = logged(&peer.log)
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
    let messages = logged(&peer.log);
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
    let mut context = Context::from_waker(Waker::noop());
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
fn message_requests_are_forwarded_and_dismissed() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    let events = peer.client.events();
    block_on(peer.client.request("test/ask", Value::Null)).unwrap();
    // The peer asks before it answers test/ask, and the coordinator handles both in order.
    let params = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            Event::Notification { method, params } if method == "window/showMessageRequest" => {
                Some(params)
            }
            _ => None,
        })
        .expect("message request was not forwarded");
    assert_eq!(params["message"], "Index failed");
    // No action chosen, rather than an error the server may treat as a broken client.
    let response = peer.wait_for(|v| v["id"] == "ask");
    assert!(response.get("error").is_none(), "{response}");
    assert!(response["result"].is_null());
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
fn a_server_that_stops_reading_its_input_is_stopped() {
    let peer = Peer::new();
    block_on(peer.client.initialize()).unwrap();
    let events = peer.client.events();
    peer.client.notify("test/sleep", json!(60)).unwrap();
    let text = "x".repeat(64 * 1024);
    for _ in 0..64 {
        peer.client
            .notify("test/large", json!({ "text": text }))
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let stopped = loop {
        match events.try_recv() {
            Ok(Event::Stopped(reason)) => break reason,
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "a stalled server was kept");
                thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert!(stopped.contains("stopped reading its input"), "{stopped}");
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
fn a_frame_with_an_invalid_payload_is_skipped_not_fatal() {
    // lsp-server cannot deserialize a response whose id is null.
    let frame = |body: &[u8]| {
        let mut f = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        f.extend_from_slice(body);
        f
    };
    let mut bytes =
        frame(br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse"}}"#);
    bytes.extend(frame(b"not json"));
    bytes.extend(frame(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
    ));
    let mut reader = std::io::Cursor::new(bytes);
    assert!(matches!(
        read_message(&mut reader).unwrap(),
        Some(Message::Notification(n)) if n.method == "initialized"
    ));
    assert!(read_message(&mut reader).unwrap().is_none());
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
    let client = Client::start(fake_server(&log), &std::env::temp_dir(), json!({})).unwrap();
    block_on(client.initialize()).unwrap();
    block_on(client.request("test/ignoreShutdown", Value::Null)).unwrap();
    let pid = server_pid(&log);
    let events = client.events();
    let started = Instant::now();
    drop(client);
    while let Ok(event) = block_on(events.recv()) {
        if matches!(event, Event::Stopped(_)) {
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(reaped(pid), "server was not reaped");
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
    let pid = server_pid(&peer.log);
    let started = Instant::now();
    peer.client.terminate();
    // Closing the channel wakes every waiter, including clones from different editors.
    let _ = block_on(peer.client.0.stopped.recv());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(reaped(pid));
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
    let pid = server_pid(&peer.log);
    drop(peer);
    let started = Instant::now();
    cleanup.tx.send(CommandMessage::Terminate).unwrap();
    let _ = block_on(cleanup.stopped.recv());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(reaped(pid));
}
