use super::super::codex::*;
use super::*;
fn v(input: u64, output: u64) -> [u64; 6] {
    [input, 0, 0, output, 0, input + output]
}
fn totals(es: &[Entry]) -> Vec<u64> {
    let mut v: Vec<_> = es.iter().map(Entry::total).collect();
    v.sort();
    v
}
// upstream: LocalUsageReaderTests.swift::testCodexParsing
#[test]
fn codex_disjoint_buckets() {
    let r = rollout(
        "a",
        &[ctx("gpt-5.5"), event(1., None, [100, 30, 0, 10, 5, 110])],
    );
    let e = resolve(vec![r]);
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].input, e[0].cache_read, e[0].output), (70, 30, 10));
    assert_eq!(e[0].total(), 110);
    assert_eq!(e[0].model, "gpt-5.5")
}
// upstream: LocalUsageReaderTests.swift::testCodexNonForkResolverPreservesParsedEntriesExceptCanonicalIDs
#[test]
fn nonfork_preserves_records() {
    let r = rollout(
        "a",
        &[
            meta("s", None, false),
            ctx("gpt-5.5"),
            event(1., Some(v(100, 10)), v(100, 10)),
            event(2., Some(v(300, 30)), v(200, 20)),
        ],
    );
    let mut expected: Vec<_> = r.events.iter().map(|e| e.entry.clone()).collect();
    let mut actual = resolve(vec![r]);
    for e in expected.iter_mut().chain(actual.iter_mut()) {
        e.id.clear()
    }
    assert_eq!(actual, expected)
}
// upstream: LocalUsageReaderTests.swift::testCodexDropsConsecutiveSameStateRerecordsAndMatchesCumulativeTotal
#[test]
fn repeated_full_state_suppressed() {
    let r = rollout(
        "a",
        &[
            meta("s", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
            event(2., Some(v(100, 10)), v(100, 10)),
            event(3., Some(v(300, 30)), v(200, 20)),
        ],
    );
    let e = resolve(vec![r]);
    assert_eq!(totals(&e), [110, 220]);
    assert_eq!(bucket(&e).total(), 330)
}
// upstream: LocalUsageReaderTests.swift::testCodexSameScalarTotalsWithDifferentFullVectorsArePreserved
#[test]
fn full_vector_identity_not_scalar() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("s", None, false),
            event(1., Some([100, 0, 0, 10, 0, 110]), v(100, 10)),
            event(2., Some([100, 1, 0, 10, 0, 110]), v(100, 10)),
        ],
    )]);
    assert_eq!(e.len(), 2);
    assert_ne!(e[0].id, e[1].id)
}
// upstream: LocalUsageReaderTests.swift::testCodexUnchangedCumulativeWithDifferentLastVectorIsPreserved
#[test]
fn last_vector_is_part_of_identity() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("s", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
            event(2., Some(v(100, 10)), v(50, 5)),
        ],
    )]);
    assert_eq!(totals(&e), [55, 110])
}
// upstream: LocalUsageReaderTests.swift::testCodexTotalOnlyLastUsageCountsWhenItMatchesSessionTotal
#[test]
fn total_only_matches_cumulative() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("s", None, false),
            event(1., Some(v(100, 10)), [0, 0, 0, 0, 0, 110]),
        ],
    )]);
    assert_eq!(totals(&e), [110]);
    assert_eq!(e[0].cost_unavailable, Some(true))
}
// upstream: LocalUsageReaderTests.swift::testCodexTotalOnlyLastUsageCountsWhenCumulativeIsAbsent
#[test]
fn total_only_without_cumulative() {
    let e = resolve(vec![rollout("a", &[event(1., None, [0, 0, 0, 0, 0, 110])])]);
    assert_eq!(totals(&e), [110]);
    assert!(bucket(&e).coverage.unknown)
}
// upstream: LocalUsageReaderTests.swift::testCodexTotalOnlyLastUsageCountsWhenCumulativeGrew
#[test]
fn total_only_cumulative_grew() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("s", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
            event(2., Some(v(300, 30)), [0, 0, 0, 0, 0, 220]),
        ],
    )]);
    assert_eq!(totals(&e), [110, 220]);
    assert!(
        e.iter()
            .find(|e| e.total() == 220)
            .unwrap()
            .cost_unavailable
            .unwrap()
    )
}
// upstream: LocalUsageReaderTests.swift::testCodexSessionChangeResetsSameStateComparison
#[test]
fn session_change_resets_rerecord() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("a", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
            meta("b", None, false),
            event(2., Some(v(100, 10)), v(100, 10)),
        ],
    )]);
    assert_eq!(totals(&e), [110, 110]);
    assert_ne!(e[0].id, e[1].id)
}
// upstream: LocalUsageReaderTests.swift::testCodexMissingCumulativeUsagePreservesRepeatedRecords
#[test]
fn missing_cumulative_keeps_repeats() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("s", None, false),
            event(1., None, v(100, 10)),
            event(2., None, v(100, 10)),
        ],
    )]);
    assert_eq!(totals(&e), [110, 110])
}
// upstream: LocalUsageReaderTests.swift::testCodexManualForkFallsBackWhenParentUsageStateIsUnavailable
#[test]
fn fork_old_logs_fallback() {
    let parent = rollout(
        "p",
        &[
            meta("p", None, false),
            event(1., None, v(100, 10)),
            event(2., None, v(200, 20)),
        ],
    );
    let child = rollout(
        "c",
        &[
            meta("c", Some("p"), false),
            event(3., None, v(100, 10)),
            event(3.01, None, v(200, 20)),
            event(6., None, v(300, 30)),
        ],
    );
    assert_eq!(totals(&resolve(vec![parent, child])), [110, 220, 330])
}
// upstream: LocalUsageReaderTests.swift::testCodexManualForkFallsBackWhenFoundParentPrefixDoesNotMatch
#[test]
fn fork_mismatch_uses_timing_fallback() {
    let parent = rollout(
        "p",
        &[
            meta("p", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
        ],
    );
    let child = rollout(
        "c",
        &[
            meta("c", Some("p"), false),
            event(3., Some([100, 0, 7, 10, 0, 110]), v(100, 10)),
            event(6., Some(v(300, 30)), v(200, 20)),
        ],
    );
    assert_eq!(totals(&resolve(vec![parent, child])), [110, 220])
}
// upstream: LocalUsageReaderTests.swift::testCodexCumulativeUsageClampsOutOfRangeNumber
#[test]
fn huge_cumulative_clamped() {
    let mut e = event(1., Some(v(100, 10)), v(100, 10));
    e["payload"]["info"]["total_token_usage"]["input_tokens"] = json!(1e30);
    assert_eq!(
        totals(&resolve(vec![rollout("a", &[meta("s", None, false), e])])),
        [110]
    )
}
// upstream: LocalUsageReaderTests.swift::testCodexForkTrimsReplayBeforeDroppingActualSameStateRerecord
#[test]
fn fork_replay_then_rerecord() {
    let e = resolve(vec![rollout(
        "c",
        &[
            meta("c", Some("p"), false),
            event(0.01, Some(v(100, 10)), v(100, 10)),
            event(3., Some(v(300, 30)), v(200, 20)),
            event(4., Some(v(300, 30)), v(200, 20)),
        ],
    )]);
    assert_eq!(totals(&e), [220])
}
// upstream: LocalUsageReaderTests.swift::testCodexForkedRolloutDropsLeadingReplayBurst
#[test]
fn fork_leading_burst() {
    let e = resolve(vec![rollout(
        "c",
        &[
            meta("c", Some("p"), false),
            event(0.01, None, v(100, 50)),
            event(0.02, None, v(100, 51)),
            event(3., None, v(100, 52)),
        ],
    )]);
    assert_eq!(e.iter().map(|e| e.output).collect::<Vec<_>>(), [52])
}
// upstream: LocalUsageReaderTests.swift::testCodexForkDropsReplayBurstThatStartsAfterMetadataDelay
#[test]
fn fork_metadata_delay_not_usage() {
    let e = resolve(vec![rollout(
        "c",
        &[
            meta("c", Some("p"), false),
            event(3., None, v(100, 1)),
            event(3.01, None, v(100, 2)),
            event(3.02, None, v(100, 3)),
            event(43., None, v(100, 99)),
        ],
    )]);
    assert_eq!(e.iter().map(|e| e.output).collect::<Vec<_>>(), [99])
}
// upstream: LocalUsageReaderTests.swift::testCodexForkKeepsRealTurnsAfterReplayBurstWhenTheyAreLessThanTwoSecondsApart
#[test]
fn fork_real_turns_less_than_two_seconds() {
    let e = resolve(vec![rollout(
        "c",
        &[
            meta("c", Some("p"), false),
            event(0.01, None, v(100, 1)),
            event(0.02, None, v(100, 2)),
            event(0.03, None, v(100, 3)),
            event(1.53, None, v(100, 11)),
            event(3.03, None, v(100, 22)),
            event(4.53, None, v(100, 33)),
            event(59., None, v(100, 44)),
        ],
    )]);
    let mut out: Vec<_> = e.iter().map(|e| e.output).collect();
    out.sort();
    assert_eq!(out, [11, 22, 33, 44])
}
// upstream: LocalUsageReaderTests.swift::testCodexForkDetectsMetadataAfterLeadingNonTokenRecord
#[test]
fn metadata_after_context() {
    let e = resolve(vec![rollout(
        "c",
        &[
            ctx("gpt-5.5"),
            meta("c", Some("p"), false),
            event(0.01, None, v(100, 1)),
            event(3., None, v(100, 99)),
        ],
    )]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].output, 99)
}
fn padded(id: &str, n: usize) -> Value {
    let mut m = meta(id, None, false);
    m["payload"]["base_instructions"] = json!("a".repeat(n));
    m
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeReadsSessionIDWhenMetadataLineExceedsChunk
#[test]
fn probe_large_metadata() {
    let d = Temp::new();
    let p = d.write("p.jsonl", lines(&[padded("large", 200000)]));
    assert_eq!(probe(&p, 1 << 20).unwrap(), Some("large".into()))
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeDecodesMultibyteStraddlingChunkBoundary
#[test]
fn probe_multibyte_chunk_boundary() {
    let d = Temp::new();
    let head = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"utf8\",\"pad\":\"";
    let mut bytes = head.to_vec();
    bytes.extend(vec![b'a'; 65535 - head.len()]);
    bytes.extend("가\"}}".as_bytes());
    assert!(std::str::from_utf8(&bytes[..65536]).is_err());
    let p = d.write("p.jsonl", bytes);
    assert_eq!(probe(&p, 1 << 20).unwrap(), Some("utf8".into()))
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeScansManyLinesAcrossChunks
#[test]
fn probe_many_lines() {
    let d = Temp::new();
    let mut vs = vec![json!({"type":"noise","pad":"x".repeat(1000)}); 100];
    vs.push(meta("late", None, false));
    let p = d.write("p.jsonl", lines(&vs));
    assert_eq!(probe(&p, 1 << 20).unwrap(), Some("late".into()))
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeStopsAtByteLimit
#[test]
fn probe_budget() {
    let d = Temp::new();
    let p = d.write("p.jsonl", lines(&[padded("large", 200000)]));
    assert_eq!(probe(&p, 65536).unwrap(), None)
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeStopsAtInvalidUTF8BeforeSessionMeta
#[test]
fn probe_invalid_utf8_stops() {
    let d = Temp::new();
    let mut bytes = vec![255, b'\n'];
    bytes.extend(lines(&[meta("wrong", None, false)]));
    let p = d.write("p.jsonl", bytes);
    assert_eq!(probe(&p, 1 << 20).unwrap(), None)
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeStopsAtTokenCountBeforeSessionMeta
#[test]
fn probe_token_before_metadata_stops() {
    let d = Temp::new();
    let p = d.write(
        "p.jsonl",
        lines(&[event(1., None, v(100, 10)), meta("wrong", None, false)]),
    );
    assert_eq!(probe(&p, 1 << 20).unwrap(), None)
}
// upstream: LocalUsageReaderTests.swift::testCodexProbeFindsMetadataAfterLeadingNonTokenRecord
#[test]
fn probe_context_before_metadata() {
    let d = Temp::new();
    let p = d.write(
        "p.jsonl",
        lines(&[ctx("gpt-5.5"), meta("correct", None, false)]),
    );
    assert_eq!(probe(&p, 1 << 20).unwrap(), Some("correct".into()))
}
fn fixture(group: &str, name: &str) -> Rollout {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/poketokenbar")
        .join(group)
        .join(format!("{name}.jsonl"));
    parse(&fs::read(&path).unwrap(), &path)
}
// upstream: LocalUsageReaderTests.swift::testCodexManualForkFixtureKeepsOnlyPostReplayUsage
#[test]
fn real_manual_fork_child() {
    assert_eq!(
        totals(&resolve(vec![fixture("CodexFork", "child")])),
        [0, 28138]
    )
}
// upstream: LocalUsageReaderTests.swift::testCodexManualForkFixtureKeepsParentAndChildUsageOnTheirOwnDays
#[test]
fn real_manual_fork_days() {
    let e = resolve(vec![
        fixture("CodexFork", "parent"),
        fixture("CodexFork", "child"),
    ]);
    assert_eq!(bucket(&e).total(), 340952);
    let r = Report::from_entries(e.clone(), epoch(15));
    let mut totals: Vec<_> = r.daily_buckets().values().map(Bucket::total).collect();
    totals.sort();
    assert_eq!(totals, [28138, 312814]);
    assert_eq!(
        e.iter().map(|e| &e.id).collect::<BTreeSet<_>>().len(),
        e.len()
    )
}
// upstream: LocalUsageReaderTests.swift::testCodexSiblingForkFixturesKeepIndependentPostReplayUsage
#[test]
fn real_sibling_forks() {
    let e = resolve(vec![
        fixture("CodexFork", "parent"),
        fixture("CodexFork", "child"),
        fixture("CodexFork", "sibling"),
    ]);
    assert_eq!(bucket(&e).total(), 369215);
    assert!(totals(&e).contains(&28138));
    assert!(totals(&e).contains(&28263))
}
// upstream: LocalUsageReaderTests.swift::testCodexSubagentFixturesKeepAllOwnUsageWithoutReplayPrefix
#[test]
fn real_subagents() {
    for (p, c, expected, total) in [
        (
            "parent",
            "child",
            vec![22992, 23043, 23062, 23219, 23291],
            115607,
        ),
        (
            "parent-v145",
            "child-v145",
            vec![20863, 21175, 21365, 21458, 21722],
            106583,
        ),
    ] {
        let child = fixture("CodexSubagent", c);
        let id = child.session.clone().unwrap();
        let e = resolve(vec![fixture("CodexSubagent", p), child]);
        assert_eq!(totals(&e), expected);
        assert_eq!(bucket(&e).total(), total);
        assert_eq!(
            e.iter()
                .filter(|e| e.id.starts_with(&format!("codex|{id}|")))
                .count(),
            2
        )
    }
}
// upstream: LocalUsageReaderTests.swift::testCodexSubagentChildFixturesKeepFirstTurnWhenParentIsMissing
#[test]
fn real_subagent_missing_parent() {
    for (c, expected) in [
        ("child", vec![23062, 23291]),
        ("child-v145", vec![21458, 21722]),
    ] {
        let child = fixture("CodexSubagent", c);
        assert!(child.subagent);
        assert!(child.parent.is_some());
        let id = child.session.clone().unwrap();
        let e = resolve(vec![child]);
        assert_eq!(totals(&e), expected);
        assert!(e.iter().all(|e| e.id.starts_with(&format!("codex|{id}|"))))
    }
}
// upstream: LocalUsageReaderTests.swift::testCodexForkOfForkReusesResolvedAncestorHistory
#[test]
fn nested_fork_history() {
    let a = event(1., Some(v(100, 10)), v(100, 10));
    let b = event(3., Some(v(300, 30)), v(200, 20));
    let c = event(6., Some(v(600, 60)), v(300, 30));
    let e = resolve(vec![
        rollout("p", &[meta("p", None, false), a.clone()]),
        rollout("c", &[meta("c", Some("p"), false), a.clone(), b.clone()]),
        rollout("g", &[meta("g", Some("c"), false), a, b, c]),
    ]);
    assert_eq!(totals(&e), [110, 220, 330])
}
// upstream: LocalUsageReaderTests.swift::testCodexSiblingForksWithIdenticalOwnUsageKeepDistinctIDs
#[test]
fn sibling_equal_usage_distinct_owner() {
    let a = event(1., Some(v(100, 10)), v(100, 10));
    let b = event(3., Some(v(300, 30)), v(200, 20));
    let e = resolve(vec![
        rollout("p", &[meta("p", None, false), a.clone()]),
        rollout("a", &[meta("a", Some("p"), false), a.clone(), b.clone()]),
        rollout("b", &[meta("b", Some("p"), false), a, b]),
    ]);
    assert_eq!(totals(&e), [110, 220, 220])
}
// upstream: LocalUsageReaderTests.swift::testCodexCumulativeResetStartsNewCanonicalEpoch
#[test]
fn reset_epoch_changes_identity() {
    let a = event(1., Some(v(100, 10)), v(100, 10));
    let b = event(3., Some(v(300, 30)), v(200, 20));
    let mut later = a.clone();
    later["timestamp"] = event(6., None, v(0, 0))["timestamp"].clone();
    let e = resolve(vec![rollout("a", &[meta("s", None, false), a, b, later])]);
    assert_eq!(totals(&e), [110, 110, 220]);
    assert!(e.iter().any(|e| e.id.starts_with("codex|s|1|")))
}
// upstream: LocalUsageReaderTests.swift::testCodexCanonicalIDCollapsesSameSessionStateAcrossFilesKeepingEarliestDate
#[test]
fn canonical_cross_file_earliest() {
    let early = event(1., Some(v(100, 10)), v(100, 10));
    let late = event(6., Some(v(100, 10)), v(100, 10));
    let e = resolve(vec![
        rollout("a", &[meta("s", None, false), late]),
        rollout("z", &[meta("s", None, false), early.clone()]),
    ]);
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0].date,
        crate::usage::timestamp(&early["timestamp"]).unwrap()
    )
}
// upstream: LocalUsageReaderTests.swift::testDegenerateParentHintStillResolvesUsageCorrectly
#[test]
fn degenerate_parent_id_fallback() {
    let e = resolve(vec![rollout(
        "a",
        &[
            meta("child", Some("-"), false),
            event(0.01, Some(v(100, 10)), v(100, 10)),
            event(5., Some(v(300, 30)), v(200, 20)),
        ],
    )]);
    assert_eq!(totals(&e), [220])
}
// upstream: LocalUsageReaderTests.swift::testForkReplayIsTrimmedAgainstTheParentRollout
#[test]
fn parent_state_trims_replay() {
    let a = event(1., Some(v(100, 10)), v(100, 10));
    let b = event(1.1, Some(v(300, 30)), v(200, 20));
    let e = resolve(vec![
        rollout("p", &[meta("p", None, false), a.clone()]),
        rollout("c", &[meta("c", Some("p"), false), a, b]),
    ]);
    assert_eq!(totals(&e), [110, 220])
}
