//! Shark's MCP server: the tools an Overview chat runs its section with, as streamable HTTP on
//! loopback. One listener for the app; each Overview chat gets its own URL and token, so a call
//! says which Overview made it. The listener only parses and authenticates: every tool runs on
//! the UI thread, which owns the tasks, and answers at once.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// The server's name in the agents' config: tools arrive as `mcp__shark__<tool>`.
pub const SERVER: &str = "shark";
pub const TOOL_PREFIX: &str = "mcp__shark__";
/// The agent process's environment variable holding its token, so it stays out of `ps`.
pub const TOKEN_ENV: &str = "SHARK_MCP_TOKEN";

pub const SPAWN: &str = "spawn_task";
pub const MESSAGE: &str = "message_task";
pub const READ: &str = "read_task";
pub const STOP: &str = "stop_task";

/// A request body is one JSON-RPC message; a brief is far below this.
const MAX_REQUEST: u64 = 1 << 20;
/// How long a tool call waits for the UI thread. Tools answer at once, so this only ends a call
/// the app never took; codex gives up on a tool after 60 seconds.
const ANSWER: Duration = Duration::from_secs(20);
/// How long one request may take to arrive, however slowly its bytes come.
const IO: Duration = Duration::from_secs(10);
/// Connections served at once; more are closed unanswered. An agent makes one call at a time,
/// so this only stops a local process from holding a thread per socket it opens.
const MAX_CONNECTIONS: usize = 32;

/// Protocol revisions answered as asked; any other gets the first.
const VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// One tool call, for the UI thread to run and answer.
pub struct Call {
    /// The Overview task whose chat called.
    pub overview: String,
    pub tool: String,
    pub args: Value,
    /// The tool's text; `Err` is shown to the agent as a failed call.
    pub reply: mpsc::SyncSender<Result<String, String>>,
    /// The agent was told the call failed, because the app took too long: it must not run now.
    pub expired: Arc<AtomicBool>,
}

struct Listening {
    port: u16,
    secret: [u8; 32],
}

static LISTENING: OnceLock<Listening> = OnceLock::new();

/// Starts the listener, once for the app. The receiver yields every authenticated tool call.
pub fn start() -> std::io::Result<async_channel::Receiver<Call>> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let mut secret = [0; 32];
    getrandom::fill(&mut secret).map_err(std::io::Error::other)?;
    let port = listener.local_addr()?.port();
    LISTENING
        .set(Listening { port, secret })
        .map_err(|_| std::io::Error::other("the MCP server is already running"))?;
    let (tx, rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("shark-mcp".into())
        .spawn(move || serve(&listener, &secret, &tx))?;
    Ok(rx)
}

/// Where the Overview task `overview`'s chats reach the server, and their token. `None` until
/// `start` ran: such a chat simply has no tools.
pub fn endpoint(overview: &str) -> Option<(String, String)> {
    let l = LISTENING.get()?;
    Some((
        format!("http://127.0.0.1:{}/mcp/{overview}", l.port),
        token(&l.secret, overview),
    ))
}

/// `endpoint` for a task that is a section's Overview; no other task's chats get the tools, so
/// tasks can't start tasks.
pub fn for_task(ws: &crate::state::Task) -> Option<(String, String)> {
    ws.overview.then(|| endpoint(&ws.name)).flatten()
}

/// claude's `--mcp-config` value. The token is read from the process's environment.
pub fn claude_config(url: &str) -> String {
    json!({"mcpServers": {SERVER: {
        "type": "http",
        "url": url,
        "headers": {"Authorization": format!("Bearer ${{{TOKEN_ENV}}}")},
    }}})
    .to_string()
}

/// codex's `-c` overrides, which reach only the process they are passed to.
pub fn codex_config(url: &str) -> [String; 2] {
    [
        format!("mcp_servers.{SERVER}.url={}", Value::from(url)),
        format!("mcp_servers.{SERVER}.bearer_token_env_var=\"{TOKEN_ENV}\""),
    ]
}

/// What plan mode refuses of this server: everything that acts on a task.
pub fn acts(tool: &str) -> bool {
    tool.strip_prefix(TOOL_PREFIX).is_some_and(|t| t != READ)
}

/// A token only this run's secret can make, so a URL alone opens nothing.
fn token(secret: &[u8; 32], overview: &str) -> String {
    URL_SAFE_NO_PAD.encode(
        Sha256::new()
            .chain_update(secret)
            .chain_update(overview)
            .finalize(),
    )
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

static LIVE: AtomicUsize = AtomicUsize::new(0);

fn serve(listener: &TcpListener, secret: &[u8; 32], calls: &async_channel::Sender<Call>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            // Out of descriptors, say: the connection stays queued, so don't spin on it.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        if LIVE.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            LIVE.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let (secret, calls) = (*secret, calls.clone());
        // A call blocks on the UI thread's answer, so each connection has its own thread.
        let spawned = std::thread::Builder::new()
            .name("shark-mcp-call".into())
            .spawn(move || {
                let _ = connection(&stream, &secret, &calls);
                LIVE.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

struct Request {
    method: String,
    path: String,
    bearer: Option<String>,
    body: Vec<u8>,
}

/// A socket read that gives up at `until`, however the bytes trickle in.
struct Timed<'a>(&'a TcpStream, Instant);

impl Read for Timed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.1.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.0.set_read_timeout(Some(left))?;
        (&mut &*self.0).read(buf)
    }
}

/// The request, or the status line that refuses it. What the client claims about its body is
/// checked before a byte of it is read or allocated.
fn read_request(stream: &TcpStream) -> Result<Request, &'static str> {
    const BAD: &str = "400 Bad Request";
    let mut r = BufReader::new(Timed(stream, Instant::now() + IO).take(MAX_REQUEST));
    let mut line = String::new();
    r.read_line(&mut line).map_err(|_| BAD)?;
    let mut parts = line.split_ascii_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(BAD);
    };
    let (method, path) = (method.to_string(), path.to_string());
    let (mut len, mut bearer) = (0u64, None);
    loop {
        line.clear();
        if r.read_line(&mut line).map_err(|_| BAD)? == 0 || line.trim_end().is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if key.eq_ignore_ascii_case("content-length") {
            len = value.parse().map_err(|_| BAD)?;
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err("411 Length Required");
        } else if key.eq_ignore_ascii_case("authorization") {
            bearer = value.strip_prefix("Bearer ").map(str::to_string);
        }
    }
    if len > MAX_REQUEST {
        return Err("413 Payload Too Large");
    }
    let mut body = vec![0; len as usize];
    r.read_exact(&mut body).map_err(|_| BAD)?;
    Ok(Request {
        method,
        path,
        bearer,
        body,
    })
}

fn respond(mut stream: &TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    stream.write_all(&out)
}

/// One request per connection: the clients are local and calls are rare.
fn connection(
    stream: &TcpStream,
    secret: &[u8; 32],
    calls: &async_channel::Sender<Call>,
) -> std::io::Result<()> {
    stream.set_write_timeout(Some(IO))?;
    let req = match read_request(stream) {
        Ok(req) => req,
        Err(status) => return respond(stream, status, b""),
    };
    let overview = req.path.strip_prefix("/mcp/").unwrap_or_default();
    let known = !overview.is_empty()
        && (req.bearer.as_deref()).is_some_and(|b| same(b, &token(secret, overview)));
    if !known {
        return respond(stream, "401 Unauthorized", b"");
    }
    // claude also opens a GET stream for server messages; there are none.
    if req.method != "POST" {
        return respond(stream, "405 Method Not Allowed", b"");
    }
    // One message per request: no revision these clients speak batches them.
    let message = serde_json::from_slice::<Value>(&req.body).ok();
    let Some(message) = message.filter(Value::is_object) else {
        return respond(stream, "400 Bad Request", b"");
    };
    match rpc(&message, overview, calls) {
        Some(answer) => respond(stream, "200 OK", answer.to_string().as_bytes()),
        None => respond(stream, "202 Accepted", b""),
    }
}

/// The JSON-RPC answer to one message; none for a notification, or for a client's own response.
fn rpc(message: &Value, overview: &str, calls: &async_channel::Sender<Call>) -> Option<Value> {
    let id = message.get("id")?;
    let method = message.get("method")?.as_str().unwrap_or_default();
    let params = &message["params"];
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => Ok(call_within(params, overview, calls, ANSWER)),
        method => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

/// Hands a tool call to the UI thread and waits `wait` for its answer. A call given up on is
/// marked, so the app doesn't run it after the agent was told it failed.
fn call_within(
    params: &Value,
    overview: &str,
    calls: &async_channel::Sender<Call>,
    wait: Duration,
) -> Value {
    const CLOSING: &str = "Shark is closing.";
    let (reply, answer) = mpsc::sync_channel(1);
    let expired = Arc::new(AtomicBool::new(false));
    let sent = calls.try_send(Call {
        overview: overview.into(),
        tool: params["name"].as_str().unwrap_or_default().into(),
        args: params["arguments"].clone(),
        reply,
        expired: expired.clone(),
    });
    let answer = match sent.map(|()| answer.recv_timeout(wait)) {
        Ok(Ok(answer)) => answer,
        Ok(Err(mpsc::RecvTimeoutError::Timeout)) => {
            expired.store(true, Ordering::SeqCst);
            Err("Shark didn't answer in time; nothing was done.".into())
        }
        Ok(Err(mpsc::RecvTimeoutError::Disconnected)) | Err(_) => Err(CLOSING.into()),
    };
    let (text, is_error) = match answer {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

/// The four tools. Only `read_task` is marked read-only, which is what lets it run in plan mode
/// (claude and codex both go by the mark); none asks for a person.
fn tools() -> Value {
    let task = json!({"type": "string", "description": "The task's name, as read_task lists it."});
    json!([
        {
            "name": SPAWN,
            "description": "Creates a task in this section and starts its agent on the brief in the \
                background, at once. Nothing queues and nothing orders the work for you: start a \
                task that depends on another when that one is ready, or pass `base` to build on \
                its branch. The brief is all the new agent knows, so make it complete. Returns the \
                new task's name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "brief": {"type": "string", "description": "The agent's first message: what to do and what done means."},
                    "repos": {"type": "array", "items": {"type": "string"}, "description": "Names of the repositories the task works in; read_task lists them."},
                    "agent": {"type": "string", "description": "Name of the agent that runs it; read_task lists them."},
                    "base": {"type": "string", "description": "A task of this section whose branch the new task starts from. Omit to start from each repository's default base."},
                },
                "required": ["brief", "repos", "agent"],
            },
        },
        {
            "name": MESSAGE,
            "description": "Sends a follow-up message to a task's agent, in the task's first chat. \
                It arrives marked as coming from the Overview and is never the user's approval. A \
                working agent reads it when its current step ends.",
            "inputSchema": {
                "type": "object",
                "properties": {"task": task, "message": {"type": "string"}},
                "required": ["task", "message"],
            },
        },
        {
            "name": READ,
            "description": "Reads this section's tasks: status, whether the agent is running, its \
                last reply, the diff stat and the worktree paths. Without `task` it lists every \
                task, plus the repositories and agents spawn_task accepts. You have no repositories \
                of your own: read and edit code through these worktree paths. Edit a task's \
                worktree only while that task is idle, commit what you changed there, and tell the \
                task with message_task.",
            "inputSchema": {"type": "object", "properties": {"task": task}},
            "annotations": {"readOnlyHint": true},
        },
        {
            "name": STOP,
            "description": "Stops a task's running agent. The task, its branch and its worktrees \
                stay: only the user deletes a task.",
            "inputSchema": {"type": "object", "properties": {"task": task}, "required": ["task"]},
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7; 32];

    /// A listener on a free port, answering every tool call with its own name and arguments.
    fn serving() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = async_channel::unbounded::<Call>();
        std::thread::spawn(move || serve(&listener, &SECRET, &tx));
        std::thread::spawn(move || {
            while let Ok(c) = rx.recv_blocking() {
                let _ = c.reply.send(match c.tool.as_str() {
                    "fails" => Err("no such task".into()),
                    tool => Ok(format!("{} ran {tool} with {}", c.overview, c.args)),
                });
            }
        });
        port
    }

    /// The status line and the body of the answer to one raw request.
    fn send(port: u16, raw: &str) -> (String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(raw.as_bytes()).unwrap();
        let mut answer = String::new();
        s.read_to_string(&mut answer).unwrap();
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();
        (head.lines().next().unwrap().into(), body.into())
    }

    fn post(port: u16, overview: &str, bearer: &str, message: &Value) -> (String, Value) {
        let body = message.to_string();
        let (status, answer) = send(
            port,
            &format!(
                "POST /mcp/{overview} HTTP/1.1\r\nHost: x\r\nauthorization: Bearer {bearer}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        (status, serde_json::from_str(&answer).unwrap_or(Value::Null))
    }

    #[test]
    fn a_call_reaches_the_app_as_the_overview_its_token_names() {
        let port = serving();
        let message = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": SPAWN, "arguments": {"brief": "do it"}}});
        let (status, answer) = post(port, "mobile", &token(&SECRET, "mobile"), &message);
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(answer["id"], 4);
        assert_eq!(answer["result"]["isError"], false);
        assert_eq!(
            answer["result"]["content"][0]["text"],
            r#"mobile ran spawn_task with {"brief":"do it"}"#
        );
        // A tool's failure is the call's result, not a protocol error: the agent reads why.
        let fails = json!({"id": "a", "method": "tools/call", "params": {"name": "fails"}});
        let (_, answer) = post(port, "mobile", &token(&SECRET, "mobile"), &fails);
        assert_eq!(answer["result"]["isError"], true);
        assert_eq!(answer["result"]["content"][0]["text"], "no such task");
    }

    #[test]
    fn only_the_overviews_own_token_opens_its_url() {
        let port = serving();
        let ping = json!({"id": 1, "method": "ping"});
        let ok = token(&SECRET, "mobile");
        assert_eq!(post(port, "mobile", &ok, &ping).0, "HTTP/1.1 200 OK");
        // Another Overview's token, a wrong one, none, and no task in the path.
        for (overview, bearer) in [("web", ok.as_str()), ("mobile", "nope"), ("", ok.as_str())] {
            let (status, _) = post(port, overview, bearer, &ping);
            assert_eq!(status, "HTTP/1.1 401 Unauthorized", "{overview} {bearer}");
        }
        let (status, _) = send(
            port,
            "POST /mcp/mobile HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
        );
        assert_eq!(status, "HTTP/1.1 401 Unauthorized");
        assert_ne!(
            token(&[8; 32], "mobile"),
            ok,
            "the token depends on the run's secret"
        );
    }

    #[test]
    fn handshake_as_claude_and_codex_send_it() {
        let port = serving();
        let ok = token(&SECRET, "mobile");
        let ask = |m: Value| post(port, "mobile", &ok, &m);
        for version in ["2025-11-25", "2025-06-18"] {
            let (_, a) = ask(
                json!({"id": 0, "method": "initialize", "params": {"protocolVersion": version}}),
            );
            assert_eq!(a["result"]["protocolVersion"], version);
            assert_eq!(a["result"]["serverInfo"]["name"], SERVER);
        }
        // A revision this server doesn't know is answered with one it does.
        let (_, a) = ask(
            json!({"id": 0, "method": "initialize", "params": {"protocolVersion": "2099-01-01"}}),
        );
        assert_eq!(a["result"]["protocolVersion"], VERSIONS[0]);
        // A notification has no id and gets no body.
        let (status, a) = ask(json!({"method": "notifications/initialized"}));
        assert_eq!((status.as_str(), a), ("HTTP/1.1 202 Accepted", Value::Null));
        // claude probes `server/discover` and opens a GET stream; both are refused politely.
        let (status, a) = ask(json!({"id": 9, "method": "server/discover"}));
        assert_eq!(
            (status.as_str(), &a["error"]["code"]),
            ("HTTP/1.1 200 OK", &json!(-32601))
        );
        let get = format!("GET /mcp/mobile HTTP/1.1\r\nAuthorization: Bearer {ok}\r\n\r\n");
        assert_eq!(send(port, &get).0, "HTTP/1.1 405 Method Not Allowed");
        let (status, _) = send(
            port,
            &format!(
                "POST /mcp/mobile HTTP/1.1\r\nAuthorization: Bearer {ok}\r\nContent-Length: 3\r\n\r\n{{x}}"
            ),
        );
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
    }

    /// What a client claims about its body is checked before anything is allocated or read,
    /// token or no token.
    #[test]
    fn a_body_past_the_limit_is_refused_unread() {
        let port = serving();
        let ok = token(&SECRET, "mobile");
        for auth in [format!("Authorization: Bearer {ok}\r\n"), String::new()] {
            let (status, _) = send(
                port,
                &format!("POST /mcp/mobile HTTP/1.1\r\n{auth}Content-Length: 2000000\r\n\r\n"),
            );
            assert_eq!(status, "HTTP/1.1 413 Payload Too Large");
        }
        // A length that could never be allocated doesn't take the app down.
        let huge = "POST /mcp/mobile HTTP/1.1\r\nContent-Length: 400000000000\r\n\r\n";
        assert_eq!(send(port, huge).0, "HTTP/1.1 413 Payload Too Large");
        let chunked = format!(
            "POST /mcp/mobile HTTP/1.1\r\nAuthorization: Bearer {ok}\r\nTransfer-Encoding: chunked\r\n\r\n"
        );
        assert_eq!(send(port, &chunked).0, "HTTP/1.1 411 Length Required");
        let ping = json!({"id": 1, "method": "ping"});
        assert_eq!(
            post(port, "mobile", &ok, &ping).0,
            "HTTP/1.1 200 OK",
            "still serving"
        );
    }

    /// A request may arrive in pieces; a client's own response, or a batch, is not a call.
    #[test]
    fn requests_in_pieces_and_messages_that_are_not_calls() {
        let port = serving();
        let ok = token(&SECRET, "mobile");
        let body = json!({"id": 3, "method": "ping"}).to_string();
        let head = format!(
            "POST /mcp/mobile HTTP/1.1\r\nAuthorization: Bearer {ok}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        for piece in [&head[..20], &head[20..], &body[..5], &body[5..]] {
            s.write_all(piece.as_bytes()).unwrap();
            s.flush().unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut answer = String::new();
        s.read_to_string(&mut answer).unwrap();
        assert!(
            answer.starts_with("HTTP/1.1 200 OK") && answer.ends_with(r#""result":{}}"#),
            "{answer}"
        );
        // A response to something this server never asked has no answer.
        let (status, a) = post(port, "mobile", &ok, &json!({"id": 5, "result": {}}));
        assert_eq!((status.as_str(), a), ("HTTP/1.1 202 Accepted", Value::Null));
        let (status, _) = post(port, "mobile", &ok, &json!([{"id": 1, "method": "ping"}]));
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "no batches");
    }

    /// A call the app took too long over is not run late: the agent was told it failed, and
    /// may have asked again.
    #[test]
    fn a_call_that_timed_out_is_marked_expired() {
        let (tx, rx) = async_channel::unbounded::<Call>();
        let params = json!({"name": SPAWN, "arguments": {}});
        let answer = call_within(&params, "mobile", &tx, Duration::from_millis(30));
        assert_eq!(answer["isError"], true);
        assert_eq!(
            answer["content"][0]["text"],
            "Shark didn't answer in time; nothing was done."
        );
        let late = rx.try_recv().unwrap();
        assert!(late.expired.load(Ordering::SeqCst));
        // The app going away is said as such.
        drop(late);
        let (tx, rx) = async_channel::unbounded::<Call>();
        std::thread::spawn(move || drop(rx.recv_blocking()));
        let answer = call_within(&params, "mobile", &tx, Duration::from_secs(5));
        assert_eq!(answer["content"][0]["text"], "Shark is closing.");
    }

    #[test]
    fn only_read_task_is_marked_read_only() {
        let port = serving();
        let (_, a) = post(
            port,
            "mobile",
            &token(&SECRET, "mobile"),
            &json!({"id": 1, "method": "tools/list"}),
        );
        let tools = a["result"]["tools"].as_array().unwrap();
        let marked: Vec<_> = (tools.iter())
            .map(|t| {
                (
                    t["name"].as_str().unwrap(),
                    t["annotations"]["readOnlyHint"] == true,
                )
            })
            .collect();
        assert_eq!(
            marked,
            [
                (SPAWN, false),
                (MESSAGE, false),
                (READ, true),
                (STOP, false)
            ]
        );
        // No tool asks for a person: plan mode would then stop on it.
        assert!(tools.iter().all(|t| t.get("_meta").is_none()));
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
        // Plan mode refuses the three that act, by the names the agents use.
        assert!(acts("mcp__shark__spawn_task") && acts("mcp__shark__stop_task"));
        assert!(!acts("mcp__shark__read_task") && !acts("mcp__other__spawn_task"));
    }

    #[test]
    fn agent_configs_keep_the_token_out_of_the_command_line() {
        let url = "http://127.0.0.1:4/mcp/mobile";
        let claude: Value = serde_json::from_str(&claude_config(url)).unwrap();
        let server = &claude["mcpServers"]["shark"];
        assert_eq!(
            (&server["type"], &server["url"]),
            (&json!("http"), &json!(url))
        );
        assert_eq!(
            server["headers"]["Authorization"],
            "Bearer ${SHARK_MCP_TOKEN}"
        );
        assert_eq!(
            codex_config(url),
            [
                r#"mcp_servers.shark.url="http://127.0.0.1:4/mcp/mobile""#,
                r#"mcp_servers.shark.bearer_token_env_var="SHARK_MCP_TOKEN""#,
            ]
        );
    }
}
