//! Fake-server helpers shared by the LSP, watcher and editor tests.
use serde_json::Value;
use std::{
    future::Future,
    path::Path,
    pin::pin,
    process::Command,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::Duration,
};

struct Unpark(thread::Thread);
impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls on an ordinary thread waker. GPUI's deterministic scheduler rejects wakers invoked by
/// real OS threads, so waits on a real peer go through this even inside GPUI tests.
pub(crate) fn block_on<T>(future: impl Future<Output = T>) -> T {
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

/// `client_tests/server.py`, logging each message it receives to `log` as one JSON line.
pub(crate) fn fake_server(log: &Path) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/lsp/client_tests/server.py"
        ))
        .arg(log);
    command
}

/// The messages `fake_server` has logged so far.
pub(crate) fn logged(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
