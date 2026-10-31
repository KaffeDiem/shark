use super::*;
use objc2_foundation::{NSDate, NSISO8601DateFormatter, NSTimeZone};

#[test]
#[ignore = "read-only installed CLIs, no inference"]
fn live_usage() {
    let mut failures = vec![];
    for provider in Provider::ALL {
        let rows = match query(provider) {
            Ok(rows) => rows,
            Err(error) => {
                failures.push(format!("{}: {error:?}", provider.name()));
                continue;
            }
        };
        assert!(!rows.is_empty());
        assert!(
            rows.iter()
                .any(|row| row.remaining.is_some() && row.resets_at.is_some())
        );
        assert!(
            rows.iter()
                .all(|row| row.remaining.is_none_or(|n| (0. ..=100.).contains(&n)))
        );
        let labels: std::collections::HashSet<_> = rows.iter().map(|row| &row.label).collect();
        assert_eq!(labels.len(), rows.len(), "duplicate allowance labels");
        for row in rows {
            eprintln!(
                "{}: {} — {:?}% remaining, resets {}",
                provider.name(),
                row.label,
                row.remaining,
                row.resets_at.map_or("unknown".into(), |t| t.to_string())
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join(", "));
}

// Sanitized usage-only responses: Claude 2.1.309 and Codex 0.164.0, 2026-10-28.
fn fixture(provider: Provider) -> Value {
    serde_json::from_str(match provider {
        Provider::Claude => include_str!("fixtures/claude.json"),
        Provider::Codex => include_str!("fixtures/codex.json"),
    })
    .unwrap()
}

#[test]
fn verified_responses_deduplicate_and_use_reported_duration() {
    let rows = claude(&fixture(Provider::Claude)["response"]["response"]).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| (r.label.as_str(), r.remaining))
            .collect::<Vec<_>>(),
        [
            ("5 hours", Some(98.)),
            ("Weekly", Some(20.)),
            ("Fable", Some(52.))
        ]
    );
    assert!(rows.iter().all(|r| r.resets_at.is_some()));
    assert_eq!(
        rows.iter().map(|r| r.window).collect::<Vec<_>>(),
        [Some(5. * 3600.), Some(7. * 86400.), Some(7. * 86400.)]
    );
    let rows = codex(&fixture(Provider::Codex)["result"]).unwrap();
    assert_eq!(
        rows,
        [Allowance {
            label: "Weekly".into(),
            remaining: Some(68.),
            resets_at: Some(1793774588.),
            window: Some(604800.),
        }]
    );
}

#[test]
fn multiple_buckets_windows_and_unknown_fields() {
    let rows = codex(&json!({
        "rateLimits": {"primary": {"usedPercent": 1, "windowDurationMins": 300}},
        "rateLimitsByLimitId": {
            "codex": {"primary": {"usedPercent":100,"windowDurationMins":300,"resetsAt":0},
                "secondary": {"usedPercent":90,"windowDurationMins":10080}},
            "model": {"limitName":"Codex model", "primary":{"usedPercent":20,"windowDurationMins":120},
                "secondary": {"usedPercent":30,"windowDurationMins":45}},
            "future": {"newWindow": {"usedPercent":5}}
        }
    })).unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| (r.label.as_str(), r.remaining))
            .collect::<Vec<_>>(),
        [
            ("5 hours", Some(0.)),
            ("Weekly", Some(10.)),
            ("Codex model · 2 hours", Some(80.)),
            ("Codex model · 45 minutes", Some(70.))
        ]
    );
    assert_eq!(
        rows[0].resets_at,
        Some(0.),
        "a past reset never replenishes locally"
    );
}

#[test]
fn missing_values_are_unknown_and_changed_contract_is_unavailable() {
    let rows = claude(&json!({"rate_limits": {
        "five_hour": {}, "seven_day": {"utilization":100,"resets_at":"not a date"},
        "seven_day_opus": {"utilization": 40}, "seven_day_sonnet": {"utilization":0}
    }}))
    .unwrap();
    assert_eq!(rows[0].remaining, None);
    assert_eq!(rows[0].resets_at, None);
    assert_eq!(rows[1].remaining, Some(0.));
    assert_eq!(rows[1].resets_at, None);
    assert_eq!(rows[2].label, "Opus");
    assert_eq!(rows[3].remaining, Some(100.));
    let rows = codex(&json!({"rateLimits":{"primary":{},"secondary":{"usedPercent":-1}}})).unwrap();
    assert!(
        rows.iter()
            .all(|r| r.remaining.is_none() && r.resets_at.is_none() && r.label == "Window unknown")
    );
    for v in [json!({}), json!({"future": {"percent":4}})] {
        assert_eq!(claude(&v), Err(Failure::Unsupported));
        assert_eq!(codex(&v), Err(Failure::Unsupported));
    }
    assert_eq!(
        claude(&json!({"rate_limits_available":false})),
        Err(Failure::Unsupported)
    );
    assert_eq!(remaining(&json!(101)), Some(0.));
    assert_eq!(remaining(&json!("50")), None);
}

#[test]
fn supported_but_empty_claude_usage_is_temporary() {
    for limits in [Value::Null, json!({})] {
        assert_eq!(
            claude(&json!({"rate_limits_available":true,"rate_limits":limits})),
            Err(Failure::Temporary)
        );
    }
    assert_eq!(
        claude(&json!({"rate_limits_available":true,"rate_limits":{"new_contract":true}})),
        Err(Failure::Unsupported)
    );
}

#[test]
fn foundation_parses_offsets_fractional_and_whole_seconds() {
    let z = timestamp(&json!("2026-10-28T10:50:00Z")).unwrap();
    assert_eq!(timestamp(&json!("2026-10-28T11:50:00+01:00")), Some(z));
    assert!(
        (timestamp(&json!("2026-10-28T10:50:00.404499+00:00")).unwrap() - z - 0.404).abs() < 0.002
    );
}

#[test]
fn snapshot_coalesces_and_keeps_only_temporary_failure_results() {
    let mut s = Snapshot::default();
    assert!(s.begin());
    assert!(!s.begin());
    s.finish(Ok(vec![Allowance {
        label: "Weekly".into(),
        remaining: Some(0.),
        resets_at: Some(0.),
        window: None,
    }]));
    assert!(s.checked_at.is_some());
    let fetched = s.fetched_at;
    assert!(s.begin());
    s.finish(Err(Failure::Temporary));
    assert_eq!(s.allowances[0].remaining, Some(0.));
    assert_eq!(s.fetched_at, fetched);
    assert!(s.outdated);
    assert!(s.begin());
    assert!(s.outdated, "a retry does not make old results current");
    s.finish(Err(Failure::SignIn));
    assert!(s.allowances.is_empty());
    assert!(s.fetched_at.is_none());
    assert!(!s.outdated);
}

struct Fake {
    dir: std::path::PathBuf,
}

impl Fake {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "shark-usage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fake.py"), r#"
import sys, json, os, time, select
root = os.environ['FAKE_DIR']
mode = os.environ['FAKE_MODE']
provider = os.environ['FAKE_PROVIDER']
open(root+'/pid', 'w').write(str(os.getpid()))
open(root+'/argv', 'w').write(json.dumps(sys.argv[1:]))
def read():
    line = sys.stdin.readline()
    with open(root+'/requests', 'a') as f: f.write(line)
    return json.loads(line)
def send(v): print(json.dumps(v), flush=True)
if mode == 'stderr':
    print('error: unknown option --safe-mode', file=sys.stderr)
    sys.exit(1)
v = read()
if mode == 'result_error':
    send({'type':'result','is_error':True,'errors':['Invalid API key']})
    sys.exit(0)
if mode == 'timeout':
    while True: time.sleep(1)
if provider == 'Codex':
    assert v['method'] == 'initialize'
    # A premature usage response and an unrelated init response must be ignored.
    send({'id':'usage','result':{'rateLimits':{'primary':{'usedPercent':99}}}})
    send({'id':'wrong','result':{}})
    assert not select.select([sys.stdin], [], [], 0.05)[0], 'client did not wait for init'
    if mode == 'init_error':
        send({'id':'init','error':{'message':'authentication required'}})
        sys.exit(0)
    send({'id':'init','result':{}})
    assert read()['method'] == 'initialized'
    assert read()['method'] == 'account/rateLimits/read'
    send({'id':'wrong','result':{}})
    if mode.startswith('error:'):
        send({'id':'usage','error':{'message':mode[6:]}})
    else:
        send(json.load(open(root+'/response')))
else:
    assert v == {'type':'control_request','request_id':'usage','request':{'subtype':'get_usage','skip_behaviors':True}}
    send({'type':'control_response','response':{'request_id':'wrong','subtype':'success','response':{}}})
    if mode.startswith('error:'):
        send({'type':'control_response','response':{'request_id':'usage','subtype':'error','error':mode[6:]}})
    else:
        send(json.load(open(root+'/response')))
while True: time.sleep(1)
"#).unwrap();
        Self { dir }
    }

    fn run(&self, provider: Provider, mode: &str) -> Result<Vec<Allowance>, Failure> {
        std::fs::write(self.dir.join("response"), fixture(provider).to_string()).unwrap();
        let mut command = Command::new("/usr/bin/python3");
        command
            .arg(self.dir.join("fake.py"))
            .env("FAKE_DIR", &self.dir)
            .env("FAKE_MODE", mode)
            .env("FAKE_PROVIDER", provider.name());
        query_with(
            provider,
            command,
            if mode == "timeout" {
                Duration::from_millis(300)
            } else {
                Duration::from_secs(5)
            },
        )
    }

    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("requests"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    fn reaped(&self) {
        let pid = std::fs::read_to_string(self.dir.join("pid")).unwrap();
        assert!(
            !Command::new("/bin/kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success(),
            "child {pid} still exists"
        );
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn fake_handshake_matching_flags_and_no_inference() {
    for provider in Provider::ALL {
        let fake = Fake::new();
        let result = fake.run(provider, "success").unwrap();
        fake.reaped();
        let requests = fake.requests();
        match provider {
            Provider::Claude => {
                assert_eq!(requests.len(), 1);
                assert_eq!(result.len(), 3);
                let args: Vec<String> =
                    serde_json::from_str(&std::fs::read_to_string(fake.dir.join("argv")).unwrap())
                        .unwrap();
                for flag in [
                    "--safe-mode",
                    "--strict-mcp-config",
                    "--no-session-persistence",
                ] {
                    assert!(args.iter().any(|a| a == flag));
                }
                assert!(args.windows(2).any(|a| a == ["--tools", ""]));
            }
            Provider::Codex => {
                assert_eq!(
                    requests
                        .iter()
                        .map(|v| v["method"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["initialize", "initialized", "account/rateLimits/read"]
                );
                assert_eq!(result[0].remaining, Some(68.));
            }
        }
        assert!(requests.iter().all(|v| v["type"] != "user"
            && v["method"] != "turn/start"
            && v["method"] != "thread/start"));
    }
}

#[test]
fn fake_errors_missing_cli_and_timeout_cleanup() {
    for provider in Provider::ALL {
        for (mode, expected) in [
            ("error:Please log in", Failure::SignIn),
            ("error:Not logged in", Failure::SignIn),
            (
                "error:Only supported for ChatGPT accounts",
                Failure::Unsupported,
            ),
            ("error:Method not found", Failure::Unsupported),
            ("error:service unavailable", Failure::Temporary),
            ("timeout", Failure::Temporary),
            ("stderr", Failure::Unsupported),
        ] {
            let fake = Fake::new();
            assert_eq!(
                fake.run(provider, mode),
                Err(expected),
                "{provider:?}: {mode}"
            );
            fake.reaped();
        }
        assert_eq!(
            query_with(
                provider,
                Command::new("/does-not-exist/shark-usage"),
                Duration::from_secs(1)
            ),
            Err(Failure::MissingCli)
        );
    }
    let fake = Fake::new();
    assert_eq!(
        fake.run(Provider::Claude, "result_error"),
        Err(Failure::SignIn)
    );
    fake.reaped();
    let fake = Fake::new();
    assert_eq!(
        fake.run(Provider::Codex, "init_error"),
        Err(Failure::SignIn)
    );
    assert_eq!(fake.requests().len(), 1);
    fake.reaped();
}

fn midnight(day: i64) -> f64 {
    let utc = day as f64 * 86400.;
    let local = utc
        - NSTimeZone::localTimeZone()
            .secondsFromGMTForDate(&NSDate::dateWithTimeIntervalSince1970(utc)) as f64;
    assert_eq!(
        (activity::local_day(local - 1.), activity::local_day(local)),
        (day - 1, day)
    );
    local
}

/// A log whose `@A` is noon on `day`, `@B` the last second of it and `@C` the next midnight.
fn log(provider: Provider, day: i64) -> String {
    let iso = |s: f64| {
        NSISO8601DateFormatter::new()
            .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(s))
            .to_string()
    };
    let next = midnight(day + 1);
    match provider {
        Provider::Claude => include_str!("fixtures/claude-log.jsonl"),
        Provider::Codex => include_str!("fixtures/codex-log.jsonl"),
    }
    .replace("@A", &iso(midnight(day) + 12. * HOUR))
    .replace("@B", &iso(next - 1.))
    .replace("@C", &iso(next))
}

fn heatmap(
    provider: Provider,
    root: &std::path::Path,
    cache: &mut logs::Cache,
    now: f64,
) -> activity::Daily {
    let dir = root.join(match provider {
        Provider::Claude => "projects",
        Provider::Codex => "sessions",
    });
    logs::scan(provider, &logs::Roots::new(vec![dir]), cache, now).heatmap()
}

#[test]
fn heatmap_deduplicates_skips_noise_splits_at_local_midnight_and_excludes_cache_reads() {
    let day = activity::local_day(now()) - 5;
    // Claude: 2+100+50 once despite the repeat (the later line's output_tokens is the real one,
    // counted on the first line's day), plus 1+9 before midnight; cache reads excluded.
    // Codex: 100-40+10 once despite the repeated total, plus 200-50+20; 10-20 saturates.
    for (provider, expected) in [(Provider::Claude, [162, 1000]), (Provider::Codex, [240, 5])] {
        let root = std::env::temp_dir().join(format!(
            "shark-heatmap-{}-{}",
            std::process::id(),
            provider as usize
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(match provider {
            Provider::Claude => "projects/p",
            Provider::Codex => "sessions/2026",
        });
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jsonl"), log(provider, day)).unwrap();
        let daily = heatmap(provider, &root, &mut Default::default(), now());
        let i = (day - daily.first_day) as usize;
        assert_eq!(daily.tokens[i..i + 2], expected, "{}", provider.name());
        assert_eq!(
            daily.tokens.iter().sum::<u64>(),
            expected.iter().sum::<u64>()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn heatmap_windows_skips_stale_files_and_reuses_cache() {
    let root = std::env::temp_dir().join(format!("shark-activity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (now, today) = (now(), activity::local_day(now()));
    let write = |path: std::path::PathBuf, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    };
    let session = write(
        root.join("projects/p/s.jsonl"),
        &log(Provider::Claude, today - 1),
    );
    // Subagent logs count; a line from before the window does not.
    let agent = log(Provider::Claude, today - 1).replace(r#""msg_"#, r#""agent_"#)
        + &log(Provider::Claude, today - 200).replace(r#""msg_"#, r#""old_"#);
    write(root.join("projects/p/s/subagents/agent-1.jsonl"), &agent);
    let stale = write(
        root.join("projects/q/old.jsonl"),
        &log(Provider::Claude, today - 1).replace(r#""msg_"#, r#""stale_"#),
    );
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(86400))
        .unwrap();
    let codex_dir = |day| root.join(format!("sessions/{day}"));
    write(
        codex_dir(today).join("rollout-a.jsonl"),
        &log(Provider::Codex, today - 1),
    );
    // A session started before the window and resumed inside it still counts.
    write(
        codex_dir(today - 140).join("rollout-b.jsonl"),
        &log(Provider::Codex, today - 1).replace(r#""s1""#, r#""s2""#),
    );

    let mut caches = [logs::Cache::default(), logs::Cache::default()];
    let mut scan = |p: Provider| heatmap(p, &root, &mut caches[p as usize], now);
    let daily = scan(Provider::Claude);
    assert_eq!(daily.first_day, today - 139);
    assert_eq!(daily.tokens.len(), activity::DAYS);
    assert_eq!(daily.tokens[138..], [324, 2000]);
    assert_eq!(daily.tokens[..138].iter().sum::<u64>(), 0);
    assert_eq!(scan(Provider::Codex).tokens[138..], [480, 10]);

    // Same length and mtime: the cached result stands even though the bytes changed.
    let mtime = std::fs::metadata(&session).unwrap().modified().unwrap();
    std::fs::write(
        &session,
        log(Provider::Claude, today - 1).replace("1000", "3000"),
    )
    .unwrap();
    let file = std::fs::File::options().write(true).open(&session).unwrap();
    file.set_modified(mtime).unwrap();
    assert_eq!(scan(Provider::Claude).tokens[139], 2000);
    file.set_modified(mtime + Duration::from_secs(1)).unwrap();
    assert_eq!(scan(Provider::Claude).tokens[139], 4000);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn heatmap_extreme_counts_saturate() {
    let entry = |id: &str| logs::Entry {
        id: id.into(),
        date: now(),
        input: u64::MAX,
        output: 1,
        cache_write: 1,
        ..Default::default()
    };
    let report = logs::Report::from_entries(vec![entry("a"), entry("b")], now());
    assert_eq!(report.heatmap().tokens.last(), Some(&u64::MAX));
}

#[test]
fn failures_read_message_fields_and_401_as_a_word() {
    let kind = |v: Value| failure(&message(&v));
    // Ids, durations and timestamps around a transient error say nothing about auth.
    assert_eq!(
        kind(
            json!({"type":"result","is_error":true,"result":"overloaded","session_id":"s-401-login","duration_ms":1401,"timestamp":"2025-10-04T01:40:12Z"})
        ),
        Failure::Temporary
    );
    assert_eq!(failure("took 1401ms"), Failure::Temporary);
    assert_eq!(failure("HTTP 401: nope"), Failure::SignIn);
    assert_eq!(kind(json!({"code":401,"message":"nope"})), Failure::SignIn);
    assert_eq!(kind(json!({"errors":["Invalid API key"]})), Failure::SignIn);
    assert_eq!(kind(json!({"code":-32601})), Failure::Unsupported);
}

#[test]
fn window_label_is_singular_for_one() {
    for (mins, label) in [
        (1440, "1 day"),
        (2880, "2 days"),
        (60, "1 hour"),
        (10080, "Weekly"),
    ] {
        assert_eq!(window_label(&json!(mins)), label);
    }
}

#[test]
fn exchange_reads_each_providers_protocol_from_pipes() {
    let reply = |provider, lines: &[String]| {
        let mut sent = vec![];
        let out = std::io::Cursor::new(lines.join("\n") + "\n");
        let result = exchange(provider, &mut sent, out);
        let sent: Vec<Value> = String::from_utf8(sent)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (result, sent)
    };
    let (rows, sent) = reply(
        Provider::Claude,
        &[
            json!({"type":"control_response","response":{"request_id":"wrong","subtype":"success","response":{}}}).to_string(),
            fixture(Provider::Claude).to_string(),
        ],
    );
    assert_eq!(rows.unwrap().len(), 3);
    assert_eq!(sent[0]["request"]["subtype"], "get_usage");
    assert_eq!(sent.len(), 1);

    // A usage answer before the handshake finishes is ignored.
    let usage = fixture(Provider::Codex).to_string();
    let (rows, sent) = reply(
        Provider::Codex,
        &[
            usage.clone(),
            json!({"id":"init","result":{}}).to_string(),
            usage,
        ],
    );
    assert_eq!(rows.unwrap()[0].remaining, Some(68.));
    let methods: Vec<_> = sent.iter().map(|v| v["method"].as_str().unwrap()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "account/rateLimits/read"]
    );

    let (rows, _) = reply(
        Provider::Codex,
        &[json!({"id":"init","error":{"message":"not logged in"}}).to_string()],
    );
    assert_eq!(rows, Err(Failure::SignIn));
    assert_eq!(
        exchange(Provider::Claude, Vec::new(), std::io::empty()),
        Err(Failure::Temporary)
    );
    assert_eq!(
        exchange(Provider::Claude, Vec::new(), &b"{}"[..]),
        Err(Failure::Unsupported)
    );
}
