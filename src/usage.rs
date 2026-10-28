//! Short-lived, prompt-free account queries. No chat state or snapshots go to disk.
use objc2_foundation::{
    NSDate, NSDateFormatter, NSDateFormatterStyle, NSISO8601DateFormatOptions,
    NSISO8601DateFormatter, NSString, NSTimeZone,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Claude => 0,
            Self::Codex => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Allowance {
    pub label: String,
    pub remaining: Option<f64>,
    pub resets_at: Option<f64>,
    /// Length of the reset window in seconds.
    pub window: Option<f64>,
}

const HOUR: f64 = 3600.;
const WEEK: f64 = 7. * 24. * HOUR;

impl Allowance {
    /// The share that would be left `now` if the window were used evenly: 50 halfway to the reset.
    pub fn pace(&self, now: f64) -> Option<f64> {
        let window = self.window.filter(|w| *w > 0.)?;
        Some(((self.resets_at? - now) / window * 100.).clamp(0., 100.))
    }
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Failure {
    MissingCli,
    SignIn,
    Unsupported,
    Temporary,
}

impl Failure {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingCli => "CLI not installed",
            Self::SignIn => "Sign in through the CLI",
            Self::Unsupported => "Usage reporting unavailable",
            Self::Temporary => "Could not refresh usage",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed(Failure),
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub allowances: Vec<Allowance>,
    pub status: Status,
    pub checked_at: Option<f64>,
    pub fetched_at: Option<f64>,
    pub outdated: bool,
}

impl Snapshot {
    /// The tightest known allowance; unknown windows cannot imply available capacity.
    pub fn limiting(&self) -> Option<&Allowance> {
        self.allowances
            .iter()
            .filter(|row| row.remaining.is_some())
            .min_by(|a, b| a.remaining.unwrap().total_cmp(&b.remaining.unwrap()))
    }

    /// One in-flight query per provider, shared by opening, refresh and polling.
    pub fn begin(&mut self) -> bool {
        if self.status == Status::Loading {
            return false;
        }
        self.status = Status::Loading;
        true
    }

    pub fn finish(&mut self, result: Result<Vec<Allowance>, Failure>) {
        let now = now();
        self.checked_at = Some(now);
        match result {
            Ok(rows) => {
                self.allowances = rows;
                self.fetched_at = Some(now);
                self.status = Status::Ready;
                self.outdated = false;
            }
            Err(error) => {
                if error != Failure::Temporary {
                    self.allowances.clear();
                    self.fetched_at = None;
                }
                self.outdated = error == Failure::Temporary && !self.allowances.is_empty();
                self.status = Status::Failed(error);
            }
        }
    }
}

pub fn local_time(seconds: f64) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setDateStyle(NSDateFormatterStyle::MediumStyle);
    formatter.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    formatter.setTimeZone(Some(&NSTimeZone::localTimeZone()));
    formatter
        .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(seconds))
        .to_string()
}

fn timestamp(value: &Value) -> Option<f64> {
    let formatter = NSISO8601DateFormatter::new();
    let text = NSString::from_str(value.as_str()?);
    for options in [
        NSISO8601DateFormatOptions::WithInternetDateTime
            | NSISO8601DateFormatOptions::WithFractionalSeconds,
        NSISO8601DateFormatOptions::WithInternetDateTime,
    ] {
        formatter.setFormatOptions(options);
        if let Some(date) = formatter.dateFromString(&text) {
            return Some(date.timeIntervalSince1970());
        }
    }
    None
}

fn remaining(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.)
        .map(|n| (100. - n).clamp(0., 100.))
}

fn claude(value: &Value) -> Result<Vec<Allowance>, Failure> {
    if value["rate_limits_available"] == false {
        return Err(Failure::Unsupported);
    }
    let limits = &value["rate_limits"];
    // Live CLI can report support but temporarily return no usage data.
    if value["rate_limits_available"] == true
        && (limits.is_null() || limits.as_object().is_some_and(|v| v.is_empty()))
    {
        return Err(Failure::Temporary);
    }
    let mut rows = vec![];
    let mut add = |label: String, value: &Value, percent: &str, window: f64| {
        if value.is_object() && !rows.iter().any(|r: &Allowance| r.label == label) {
            rows.push(Allowance {
                label,
                remaining: remaining(&value[percent]),
                resets_at: timestamp(&value["resets_at"]),
                window: Some(window),
            });
        }
    };
    // The structured list and legacy fields overlap. Prefer the structured list by identity.
    for limit in limits["limits"].as_array().into_iter().flatten() {
        let label = match limit["kind"].as_str() {
            Some("session") => Some(("5 hours", 5. * HOUR)),
            Some("weekly_all") => Some(("Weekly", WEEK)),
            Some("weekly_scoped") => limit["scope"]["model"]["display_name"]
                .as_str()
                .or_else(|| limit["scope"]["model"]["id"].as_str())
                .map(|label| (label, WEEK)),
            _ => None,
        };
        if let Some((label, window)) = label {
            add(label.into(), limit, "percent", window);
        }
    }
    for (key, label, window) in [
        ("five_hour", "5 hours", 5. * HOUR),
        ("seven_day", "Weekly", WEEK),
    ] {
        add(label.into(), &limits[key], "utilization", window);
    }
    for model in limits["model_scoped"].as_array().into_iter().flatten() {
        if let Some(label) = model["display_name"].as_str() {
            add(label.into(), model, "utilization", WEEK);
        }
    }
    for (key, label) in [("seven_day_opus", "Opus"), ("seven_day_sonnet", "Sonnet")] {
        add(label.into(), &limits[key], "utilization", WEEK);
    }
    if rows.is_empty() {
        Err(Failure::Unsupported)
    } else {
        Ok(rows)
    }
}

fn window_label(value: &Value) -> String {
    match value.as_u64().filter(|n| *n > 0) {
        Some(10080) => "Weekly".into(),
        Some(60) => "1 hour".into(),
        Some(n) if n % 1440 == 0 => format!("{} days", n / 1440),
        Some(n) if n % 60 == 0 => format!("{} hours", n / 60),
        Some(n) => format!("{n} minutes"),
        None => "Window unknown".into(),
    }
}

fn codex(value: &Value) -> Result<Vec<Allowance>, Failure> {
    let buckets: Vec<(&str, &Value)> = match value["rateLimitsByLimitId"]
        .as_object()
        .filter(|m| !m.is_empty())
    {
        Some(buckets) => buckets.iter().map(|(id, v)| (id.as_str(), v)).collect(),
        None => vec![("codex", &value["rateLimits"])],
    };
    let mut rows = vec![];
    for (id, bucket) in buckets {
        let model = bucket["limitName"]
            .as_str()
            .or_else(|| bucket["normalModelSlug"].as_str())
            .or_else(|| (id != "codex").then_some(id));
        for key in ["primary", "secondary"] {
            let window = &bucket[key];
            if !window.is_object() {
                continue;
            }
            let duration = window_label(&window["windowDurationMins"]);
            rows.push(Allowance {
                label: model
                    .map_or_else(|| duration.clone(), |model| format!("{model} · {duration}")),
                remaining: remaining(&window["usedPercent"]),
                resets_at: window["resetsAt"].as_f64().filter(|n| n.is_finite()),
                window: window["windowDurationMins"].as_f64().map(|m| m * 60.),
            });
        }
    }
    if rows.is_empty() {
        Err(Failure::Unsupported)
    } else {
        Ok(rows)
    }
}

fn failure(value: &str) -> Failure {
    let text = value.to_lowercase();
    if [
        "unauthorized",
        "not authenticated",
        "unauthenticated",
        "not logged in",
        "not signed in",
        "authentication",
        "sign in",
        "sign-in",
        "login",
        "log in",
        "401",
        "expired token",
        "invalid token",
        "invalid api key",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        Failure::SignIn
    } else if [
        "unsupported",
        "not supported",
        "unknown option",
        "unknown request",
        "unrecognized request",
        "unknown control",
        "method not found",
        "-32601",
        "not available",
        "only available",
        "only supported",
        "requires a chatgpt",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        Failure::Unsupported
    } else {
        Failure::Temporary
    }
}

struct QueryChild(Child);

impl Drop for QueryChild {
    fn drop(&mut self) {
        // Descendants can hold pipes open even after the CLI exits.
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn query(provider: Provider) -> Result<Vec<Allowance>, Failure> {
    let (override_name, default) = match provider {
        Provider::Claude => ("SHARK_CLAUDE", "claude"),
        Provider::Codex => ("SHARK_CODEX", "codex"),
    };
    query_with(
        provider,
        Command::new(std::env::var_os(override_name).unwrap_or(default.into())),
        Duration::from_secs(20),
    )
}

fn query_with(
    provider: Provider,
    mut cmd: Command,
    timeout: Duration,
) -> Result<Vec<Allowance>, Failure> {
    match provider {
        Provider::Claude => {
            cmd.args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--safe-mode",
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
            ]);
        }
        Provider::Codex => {
            cmd.arg("app-server");
        }
    }
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let mut child = QueryChild(
        cmd.current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Failure::MissingCli
                } else {
                    Failure::Temporary
                }
            })?,
    );
    let mut stdin = child.0.stdin.take().ok_or(Failure::Temporary)?;
    let stdout = child.0.stdout.take().ok_or(Failure::Temporary)?;
    let stderr = child.0.stderr.take().ok_or(Failure::Temporary)?;
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.take(8192).read_to_string(&mut text);
        text
    });
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let result = (|| {
            let mut send = |v: Value| writeln!(stdin, "{v}").map_err(|_| Failure::Temporary);
            match provider {
                Provider::Claude => send(
                    json!({"type":"control_request","request_id":"usage","request":{"subtype":"get_usage","skip_behaviors":true}}),
                )?,
                Provider::Codex => send(
                    json!({"id":"init","method":"initialize","params":{"clientInfo":{"name":"shark_usage","version":env!("CARGO_PKG_VERSION")}}}),
                )?,
            }
            let mut initialized = false;
            let mut reader = BufReader::new(stdout);
            loop {
                // Bound individual protocol messages, including malformed CLI output.
                let mut line = String::new();
                let n = reader
                    .by_ref()
                    .take(1024 * 1024)
                    .read_line(&mut line)
                    .map_err(|_| Failure::Temporary)?;
                if n == 0 {
                    return Err(Failure::Temporary);
                }
                if !line.ends_with('\n') {
                    return Err(Failure::Unsupported);
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                match provider {
                    Provider::Claude if v["type"] == "result" && v["is_error"] == true => {
                        return Err(failure(&v.to_string()));
                    }
                    Provider::Claude
                        if v["type"] == "control_response"
                            && v["response"]["request_id"] == "usage" =>
                    {
                        return if v["response"]["subtype"] == "success" {
                            claude(&v["response"]["response"])
                        } else {
                            Err(failure(&v["response"].to_string()))
                        };
                    }
                    Provider::Codex if !initialized && v["id"] == "init" => {
                        if !v["error"].is_null() {
                            return Err(failure(&v["error"].to_string()));
                        }
                        if !v["result"].is_object() {
                            return Err(Failure::Unsupported);
                        }
                        send(json!({"method":"initialized"}))?;
                        send(json!({"id":"usage","method":"account/rateLimits/read"}))?;
                        initialized = true;
                    }
                    Provider::Codex if initialized && v["id"] == "usage" => {
                        return if v["error"].is_null() {
                            codex(&v["result"])
                        } else {
                            Err(failure(&v["error"].to_string()))
                        };
                    }
                    _ => {}
                }
            }
        })();
        let _ = tx.send(result);
    });
    let received = rx.recv_timeout(timeout);
    let timed_out = matches!(received, Err(mpsc::RecvTimeoutError::Timeout));
    let result = received.unwrap_or(Err(Failure::Temporary));
    drop(child); // Kill and reap on success, EOF, protocol errors and timeout alike.
    let _ = reader.join();
    let stderr = errors.join().unwrap_or_default();
    match result {
        Err(Failure::Temporary) if !timed_out && !stderr.is_empty() => Err(failure(&stderr)),
        other => other,
    }
}

#[cfg(test)]
mod tests;
