use super::super::claude::*;
use super::*;
fn parse_lines(v: &[Value]) -> Vec<Entry> {
    let p = parse(&lines(v), Path::new("session.jsonl"), 0);
    let mut e = p.entries;
    apply_ledger(&mut e, &p.ledger);
    e
}
fn ledger(v: Value) -> Value {
    json!({"type":"cost-state","modelUsage":v})
}
fn a(id: &str, model: &str, tokens: u64, day: u8) -> Value {
    assistant(id, model, tokens, 0, 0, 0, day)
}
// upstream: ClaudeReportedCostTests.swift::testUnpricedModelUsesReportedCost
#[test]
fn unpriced_reported_cost() {
    let m = "claude-future-unpriced-model";
    assert_eq!(super::super::pricing::estimate(m, 10, 10, 0, 0), None);
    let e = parse_lines(&[
        a("a", m, 150, 15),
        ledger(json!({format!("{m}[1m]"):{"costUSD":12.5}})),
    ]);
    let b = bucket(&e);
    close(b.cost, 12.5);
    assert!(b.coverage.reported);
    assert!(!b.coverage.unknown)
}
// upstream: ClaudeReportedCostTests.swift::testContextWindowVariantMatchesBaseModel
#[test]
fn context_variants_pool() {
    close(
        bucket(&parse_lines(&[
            a("a", "claude-opus-5", 20, 15),
            ledger(json!({"claude-opus-5[1m]":{"costUSD":4},"claude-opus-5":{"costUSD":1}})),
        ]))
        .cost,
        5.,
    )
}
// upstream: ClaudeReportedCostTests.swift::testReportedCostOverridesTableEstimate
#[test]
fn reported_overrides_estimate() {
    let m = "claude-haiku-4-5-20251001";
    close(
        super::super::pricing::estimate(m, 1000000, 0, 0, 0).unwrap(),
        1.,
    );
    close(
        bucket(&parse_lines(&[
            a("a", m, 1000000, 15),
            ledger(json!({m:{"costUSD":7.25}})),
        ]))
        .cost,
        7.25,
    )
}
// upstream: ClaudeReportedCostTests.swift::testLastCostStateWins
#[test]
fn final_ledger_wins() {
    close(
        bucket(&parse_lines(&[
            a("a", "claude-opus-5", 10, 15),
            ledger(json!({"claude-opus-5":{"costUSD":1}})),
            a("b", "claude-opus-5", 10, 15),
            ledger(json!({"claude-opus-5":{"costUSD":9}})),
        ]))
        .cost,
        9.,
    )
}
// upstream: ClaudeReportedCostTests.swift::testCostSplitsAcrossDaysAndSumsExactly
#[test]
fn attribution_splits_days_exactly() {
    let e = parse_lines(&[
        a("a", "claude-opus-5", 300, 15),
        a("b", "claude-opus-5", 100, 16),
        ledger(json!({"claude-opus-5":{"costUSD":10}})),
    ]);
    assert_eq!(e.len(), 2);
    assert_ne!(
        super::super::local_day(e[0].date),
        super::super::local_day(e[1].date)
    );
    close(e[0].explicit_cost.unwrap(), 7.5);
    close(e[1].explicit_cost.unwrap(), 2.5);
    close(bucket(&e).cost, 10.)
}
// upstream: ClaudeReportedCostTests.swift::testUnattributableModelCostIsNotReassigned
#[test]
fn missing_models_not_reassigned() {
    close(
        bucket(&parse_lines(&[
            a("a", "claude-opus-5", 100, 15),
            ledger(
                json!({"claude-opus-5":{"costUSD":2},"claude-haiku-4-5-20251001":{"costUSD":98}}),
            ),
        ]))
        .cost,
        2.,
    )
}
// upstream: ClaudeReportedCostTests.swift::testNoCostStateFallsBackToTable
#[test]
fn absent_ledger_estimates_or_unknown() {
    let priced = bucket(&parse_lines(&[a(
        "a",
        "claude-haiku-4-5-20251001",
        1000000,
        15,
    )]));
    close(priced.cost, 1.);
    assert!(priced.coverage.estimated);
    let unknown = bucket(&parse_lines(&[a("a", "future-unpriced", 1000000, 15)]));
    assert_eq!(unknown.cost, 0.);
    assert!(unknown.coverage.unknown)
}
// upstream: ClaudeReportedCostTests.swift::testZeroTokenEntriesDoNotAbsorbCost
#[test]
fn zero_tokens_no_share() {
    let e = parse_lines(&[
        a("a", "claude-opus-5", 0, 15),
        a("b", "claude-opus-5", 500, 15),
        ledger(json!({"claude-opus-5":{"costUSD":6}})),
    ]);
    assert_eq!(e[0].explicit_cost, None);
    close(e[1].explicit_cost.unwrap(), 6.);
    close(bucket(&e).cost, 6.)
}
// upstream: ClaudeReportedCostTests.swift::testIncrementalAppendReattributesTheWholeSessionToLatestLedger
#[test]
fn appended_ledger_reattributes_whole_session() {
    let d = Temp::new();
    let mut vs = vec![
        a("a", "future", 10, 15),
        ledger(json!({"future":{"costUSD":1}})),
    ];
    d.write("session.jsonl", lines(&vs));
    let mut c = Cache::default();
    close(bucket(&c.entries(Provider::Claude, &d.roots())).cost, 1.);
    vs.extend([
        a("b", "future", 10, 16),
        ledger(json!({"future":{"costUSD":9}})),
    ]);
    d.write("session.jsonl", lines(&vs));
    let e = c.entries(Provider::Claude, &d.roots());
    assert_eq!(e.len(), 2);
    assert!(
        e.iter()
            .all(|e| e.explicit_cost == Some(4.5) && e.session_ids.contains("session"))
    );
    close(bucket(&c.entries(Provider::Claude, &d.roots())).cost, 9.);
    assert_eq!(c.parsed_files, 2)
}
// upstream: ClaudeReportedCostTests.swift::testPersistedLedgerIsRetainedWhenTheAppendedTailHasNoLedger
#[test]
fn persisted_ledger_retained_on_plain_tail() {
    let d = Temp::new();
    let cache = d.0.join("cache.bin");
    let mut vs = vec![
        a("a", "future", 10, 15),
        ledger(json!({"future":{"costUSD":4}})),
    ];
    d.write("session.jsonl", lines(&vs));
    Cache::at(cache.clone()).entries(Provider::Claude, &d.roots());
    let mut c = Cache::at(cache);
    vs.push(a("b", "future", 10, 16));
    d.write("session.jsonl", lines(&vs));
    let e = c.entries(Provider::Claude, &d.roots());
    assert_eq!(e.len(), 2);
    assert!(e.iter().all(|e| e.explicit_cost == Some(2.)));
    close(bucket(&e).cost, 4.)
}
// upstream: ClaudeReportedCostTests.swift::testTruncationDiscardsTheCachedLedger
#[test]
fn truncation_discards_ledger() {
    let d = Temp::new();
    let a = a("a", "future", 10, 15);
    d.write(
        "s.jsonl",
        lines(&[a.clone(), ledger(json!({"future":{"costUSD":9}}))]),
    );
    let mut c = Cache::default();
    c.entries(Provider::Claude, &d.roots());
    d.write("s.jsonl", lines(&[a]));
    let e = c.entries(Provider::Claude, &d.roots());
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].explicit_cost, None);
    assert!(bucket(&e).coverage.unknown)
}
// upstream: ClaudeReportedCostTests.swift::testHalfWrittenCostStateIsRetriedOnAppend
#[test]
fn partial_ledger_retried() {
    let d = Temp::new();
    let mut prefix = lines(&[a("a", "future", 10, 15)]);
    let ledger = lines(&[ledger(json!({"future":{"costUSD":9}}))]);
    let mut partial = prefix.clone();
    partial.extend(&ledger[..ledger.len() / 2]);
    d.write("s.jsonl", partial);
    let mut c = Cache::default();
    assert_eq!(
        c.entries(Provider::Claude, &d.roots())[0].explicit_cost,
        None
    );
    prefix.extend(ledger);
    d.write("s.jsonl", prefix);
    let e = c.entries(Provider::Claude, &d.roots());
    assert_eq!(e.len(), 1);
    close(bucket(&e).cost, 9.)
}
// upstream: LocalUsageReaderTests.swift::testClaudeDedupKeepsMaxOutput
#[test]
fn streamed_keep_largest_and_earliest() {
    let e = parse_lines(&[
        assistant("a", "claude-opus-5", 10, 20, 30, 40, 15),
        assistant("a", "claude-opus-5", 10, 100, 30, 40, 16),
    ]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].total(), 180);
    assert_eq!(e[0].date, epoch(15))
}
// upstream: LocalUsageReaderTests.swift::testClaudeDailyAndCost
#[test]
fn claude_daily_cost() {
    let e = parse_lines(&[assistant("a", "claude-sonnet-4", 1000, 100, 200, 300, 15)]);
    let r = Report::from_entries(e, epoch(15));
    assert_eq!(r.today(), 1600);
    close(r.totals.cost, 0.00534);
    assert!(r.totals.coverage.estimated)
}
#[test]
fn subagent_provenance_parent_session() {
    assert_eq!(
        session(Path::new("projects/session-1/subagents/agent-x.jsonl")),
        "session-1"
    );
    let a = Entry {
        id: "a".into(),
        date: 2.,
        input: 10,
        session_ids: ["parent".into()].into(),
        ..Entry::default()
    };
    let mut b = a.clone();
    b.date = 1.;
    b.output = 20;
    b.session_ids = ["child".into()].into();
    let e = dedup_claude(vec![a, b]);
    assert_eq!(e[0].session_ids.len(), 2);
    assert_eq!(e[0].date, 1.);
    assert_eq!(e[0].total(), 30)
}
