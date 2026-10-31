use super::*;
use std::{
    io::{Read, Write},
    time::{Duration, UNIX_EPOCH},
};
fn a(id: &str, out: u64) -> Value {
    assistant(id, "claude-sonnet-5", 100, out, 0, 0, 15)
}
fn stamp(path: &Path, seconds: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap()
}
fn entries(c: &mut Cache, d: &Temp) -> Vec<Entry> {
    c.entries_since(Provider::Claude, &d.roots(), 0., 10000000.)
}
fn snapshot(path: &Path) -> Value {
    let raw = fs::read(path).unwrap();
    let mut plain = vec![];
    flate2::read::ZlibDecoder::new(raw.as_slice())
        .read_to_end(&mut plain)
        .unwrap();
    serde_json::from_slice(&plain).unwrap()
}
fn save_snapshot(path: &Path, v: &Value) {
    fs::write(path, serde_json::to_vec(v).unwrap()).unwrap()
}
fn v(i: u64, o: u64) -> [u64; 6] {
    [i, 0, 0, o, 0, i + o]
}
fn fork() -> Vec<Value> {
    vec![
        meta("fork", Some("P-123"), false),
        event(5., Some(v(100, 10)), v(100, 10)),
        event(10., Some(v(300, 30)), v(200, 20)),
        event(15., Some(v(600, 60)), v(300, 30)),
    ]
}
fn orphan_tree(d: &Temp) {
    let p = d.write("a.jsonl", lines(&[meta("alpha", None, false)]));
    stamp(&p, 1000);
    let p = d.write("b.jsonl", lines(&[meta("beta", None, false)]));
    stamp(&p, 1000);
    let p = d.write("fork.jsonl", lines(&fork()));
    stamp(&p, 3000)
}
fn codex(c: &mut Cache, d: &Temp) -> Vec<Entry> {
    c.entries_since(Provider::Codex, &d.roots(), 2000., 10000000.)
}
fn total(es: &[Entry]) -> Vec<u64> {
    let mut ns: Vec<_> = es.iter().map(Entry::total).collect();
    ns.sort();
    ns
}
// upstream: LocalUsageCacheTests.swift::testDefaultPathCacheNeitherReadsNorWritesUserFile
#[test]
fn default_cache_is_memory_only() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 42)]));
    let mut c = Cache::default();
    assert!(!c.persists_to_disk());
    assert_eq!(entries(&mut c, &d)[0].output, 42);
    assert_eq!(c.blob_count(), 1);
    assert_eq!(fs::read_dir(&d.0).unwrap().count(), 1)
}
// upstream: LocalUsageCacheTests.swift::testInjectedPathCacheStillPersists
#[test]
fn explicit_cache_persists() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 7)]));
    let p = d.0.join("cache.bin");
    let mut c = Cache::at(p.clone());
    assert!(c.persists_to_disk());
    entries(&mut c, &d);
    assert!(p.is_file())
}
// upstream: LocalUsageCacheTests.swift::testUnchangedFileIsNotReparsed
#[test]
fn unchanged_not_reparsed() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 7)]));
    let mut c = Cache::default();
    let first = entries(&mut c, &d);
    assert_eq!(entries(&mut c, &d), first);
    assert_eq!(c.parsed_files, 1)
}
// upstream: LocalUsageCacheTests.swift::testMultipleRootsAreScannedAndDedupedAcrossRoots
// upstream: LocalUsageReaderTests.swift::testMultipleRootsSumButShareGlobalDedup
#[test]
fn cross_root_global_dedup() {
    let aroot = Temp::new();
    let broot = Temp::new();
    aroot.write("a.jsonl", lines(&[a("shared", 1), a("a", 2)]));
    broot.write("b.jsonl", lines(&[a("shared", 10), a("b", 3)]));
    let roots = Roots(vec![aroot.0.clone(), broot.0.clone()]);
    let e = Cache::default().entries(Provider::Claude, &roots);
    assert_eq!(e.len(), 3);
    assert_eq!(e.iter().map(|e| e.output).sum::<u64>(), 15)
}
// upstream: LocalUsageCacheTests.swift::testChangedFileIsReparsed
#[test]
fn changed_file_reparsed() {
    let d = Temp::new();
    let p = d.write("a.jsonl", lines(&[a("a", 7)]));
    stamp(&p, 1000);
    let mut c = Cache::default();
    assert_eq!(entries(&mut c, &d)[0].output, 7);
    d.write("a.jsonl", lines(&[a("a", 8)]));
    stamp(&p, 1001);
    assert_eq!(entries(&mut c, &d)[0].output, 8);
    assert_eq!(c.parsed_files, 2)
}
// upstream: LocalUsageCacheTests.swift::testAppendedFileParsesOnlyTheNewTail
#[test]
fn append_reads_only_tail() {
    let d = Temp::new();
    let first = lines(&[a("a", 7)]);
    let next = lines(&[a("b", 8)]);
    let p = d.write("a.jsonl", &first);
    let mut c = Cache::default();
    entries(&mut c, &d);
    fs::OpenOptions::new()
        .append(true)
        .open(p)
        .unwrap()
        .write_all(&next)
        .unwrap();
    assert_eq!(entries(&mut c, &d).len(), 2);
    assert_eq!(c.parsed_bytes, (first.len() + next.len()) as u64)
}
// upstream: LocalUsageCacheTests.swift::testTruncatedFileIsReparsedFromStart
#[test]
fn truncation_restarts() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 7), a("b", 8)]));
    let mut c = Cache::default();
    entries(&mut c, &d);
    d.write("a.jsonl", lines(&[a("c", 3)]));
    let e = entries(&mut c, &d);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].output, 3);
    assert_eq!(c.parsed_files, 2)
}
// upstream: LocalUsageCacheTests.swift::testHalfWrittenTrailingLineIsRetriedOnNextRefresh
#[test]
fn half_line_retried() {
    let d = Temp::new();
    let first = lines(&[a("a", 7)]);
    let next = lines(&[a("b", 8)]);
    let mut partial = first.clone();
    partial.extend(&next[..next.len() / 2]);
    d.write("a.jsonl", partial);
    let mut c = Cache::default();
    assert_eq!(entries(&mut c, &d).len(), 1);
    let mut complete = first;
    complete.extend(next);
    d.write("a.jsonl", complete);
    assert_eq!(entries(&mut c, &d).len(), 2)
}
// upstream: LocalUsageCacheTests.swift::testBlobWithoutOffsetStillDecodes
#[test]
fn old_blob_without_offset() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 7)]));
    let p = d.0.join("cache.bin");
    entries(&mut Cache::at(p.clone()), &d);
    let mut s = snapshot(&p);
    for b in s["blobs"].as_object_mut().unwrap().values_mut() {
        b.as_object_mut().unwrap().remove("offset");
    }
    save_snapshot(&p, &s);
    let mut c = Cache::at(p);
    assert_eq!(entries(&mut c, &d)[0].output, 7);
    assert_eq!(c.parsed_files, 0)
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheDropsForkedReplayBurst
#[test]
fn codex_cache_fork_burst() {
    let d = Temp::new();
    d.write(
        "c.jsonl",
        lines(&[
            meta("c", Some("p"), false),
            event(0.01, None, v(100, 50)),
            event(0.02, None, v(100, 51)),
            event(3., None, v(100, 52)),
        ]),
    );
    let e = Cache::default().entries(Provider::Codex, &d.roots());
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].output, 52)
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheDropsSameStateRerecord
#[test]
fn codex_cache_same_state() {
    let d = Temp::new();
    d.write(
        "c.jsonl",
        lines(&[
            meta("c", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
            event(3., Some(v(100, 10)), v(100, 10)),
        ]),
    );
    assert_eq!(
        Cache::default().entries(Provider::Codex, &d.roots()).len(),
        1
    )
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheDeduplicatesRolloutAcrossActiveAndArchivedRoots
#[test]
fn active_archive_duplicate() {
    let d = Temp::new();
    let values = lines(&[
        meta("s", None, false),
        event(1., Some(v(100, 10)), v(100, 10)),
    ]);
    d.write("sessions/a.jsonl", &values);
    d.write("archived_sessions/a.jsonl", values);
    let roots = Roots(vec![d.0.join("sessions"), d.0.join("archived_sessions")]);
    assert_eq!(
        total(&Cache::default().entries(Provider::Codex, &roots)),
        [110]
    )
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheFollowsRolloutWhenMovedToArchivedRoot
#[test]
fn move_to_archive() {
    let d = Temp::new();
    let p = d.write(
        "sessions/a.jsonl",
        lines(&[
            meta("s", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
        ]),
    );
    fs::create_dir_all(d.0.join("archived_sessions")).unwrap();
    let roots = Roots(vec![d.0.join("sessions"), d.0.join("archived_sessions")]);
    let mut c = Cache::default();
    let before = c.entries(Provider::Codex, &roots);
    fs::rename(p, d.0.join("archived_sessions/a.jsonl")).unwrap();
    assert_eq!(c.entries(Provider::Codex, &roots), before);
    assert_eq!(c.blob_count(), 1)
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheKeepsSubagentFirstTurnWhenParentIsMissing
#[test]
fn orphan_subagent_first_turn() {
    let d = Temp::new();
    d.write(
        "c.jsonl",
        lines(&[
            meta("child", Some("missing"), true),
            event(1., Some(v(100, 10)), v(100, 10)),
        ]),
    );
    assert_eq!(
        total(&Cache::default().entries(Provider::Codex, &d.roots())),
        [110]
    )
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheInvalidatesOutdatedParserVersion
#[test]
fn outdated_parser_reparsed() {
    let d = Temp::new();
    d.write(
        "c.jsonl",
        lines(&[
            meta("s", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
        ]),
    );
    let p = d.0.join("cache.bin");
    Cache::at(p.clone()).entries(Provider::Codex, &d.roots());
    let mut s = snapshot(&p);
    s["version"] = json!(0);
    for b in s["blobs"].as_object_mut().unwrap().values_mut() {
        b["rollout"]["events"][0]["entry"]["output"] = json!(999);
    }
    save_snapshot(&p, &s);
    let mut c = Cache::at(p);
    assert_eq!(total(&c.entries(Provider::Codex, &d.roots())), [110]);
    assert_eq!(c.parsed_files, 1)
}
// upstream: LocalUsageCacheTests.swift::testCodexCacheLoadsParentOutsideModifiedSinceForSubagentResolution
#[test]
fn old_parent_closure_no_old_usage() {
    let d = Temp::new();
    let p = d.write(
        "parent.jsonl",
        lines(&[
            meta("parent", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
        ]),
    );
    stamp(&p, 1000);
    let c = d.write(
        "child.jsonl",
        lines(&[
            meta("child", Some("parent"), true),
            event(1., Some(v(50, 5)), v(50, 5)),
            event(3., Some(v(110, 10)), v(60, 5)),
        ]),
    );
    stamp(&c, 3000);
    let es = codex(&mut Cache::default(), &d);
    assert_eq!(total(&es), [55, 65]);
    assert!(es.iter().all(|e| e.id.starts_with("codex|child|")))
}
// upstream: LocalUsageCacheTests.swift::testCodexOrphanedParentIsNotRescannedOnEveryRefresh
#[test]
fn orphan_index_reused() {
    let d = Temp::new();
    orphan_tree(&d);
    let mut c = Cache::default();
    codex(&mut c, &d);
    assert_eq!(c.probes, 2);
    let parses = c.parsed_files;
    codex(&mut c, &d);
    assert_eq!(c.probes, 2);
    assert_eq!(c.parsed_files, parses)
}
// upstream: LocalUsageCacheTests.swift::testCodexSessionIndexPersistsAcrossInstances
#[test]
fn index_persists() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.0.join("cache.bin");
    codex(&mut Cache::at(p.clone()), &d);
    let mut c = Cache::at(p);
    codex(&mut c, &d);
    assert_eq!(c.probes, 0)
}
// upstream: LocalUsageCacheTests.swift::testCodexNegativeSessionIDProbePersistsAcrossRefreshesAndInstances
#[test]
fn negative_probes_persist() {
    let d = Temp::new();
    orphan_tree(&d);
    let path = d.write("no-meta.jsonl", b"{\"type\":\"noise\"}\n");
    stamp(&path, 1000);
    let p = d.0.join("cache.bin");
    let mut c = Cache::at(p.clone());
    codex(&mut c, &d);
    assert_eq!(c.probes, 3);
    codex(&mut c, &d);
    assert_eq!(c.probes, 3);
    let mut restored = Cache::at(p);
    codex(&mut restored, &d);
    assert_eq!(restored.probes, 0)
}
// upstream: LocalUsageCacheTests.swift::testCodexProbeReadFailureIsRetriedInsteadOfPersisted
#[test]
fn failed_probe_retried() {
    let d = Temp::new();
    orphan_tree(&d);
    let mut c = Cache::default();
    let mut calls = BTreeMap::<PathBuf, usize>::new();
    for _ in 0..3 {
        c.entries_with_probe(Provider::Codex, &d.roots(), 2000., 10000000., |p| {
            let count = calls.entry(p.to_owned()).or_default();
            *count += 1;
            if p.file_name().unwrap() == "a.jsonl" && *count == 1 {
                Err(std::io::ErrorKind::PermissionDenied.into())
            } else {
                super::super::codex::probe(p, 1 << 20)
            }
        });
    }
    assert_eq!(calls[&d.0.join("a.jsonl").canonicalize().unwrap()], 2);
    assert_eq!(calls[&d.0.join("b.jsonl").canonicalize().unwrap()], 1)
}
// upstream: LocalUsageCacheTests.swift::testCodexProbeReadFailureIsNotPersistedToSnapshot
#[test]
fn failed_probe_not_persisted() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.0.join("cache.bin");
    let mut c = Cache::at(p.clone());
    c.entries_with_probe(Provider::Codex, &d.roots(), 2000., 10000000., |p| {
        if p.file_name().unwrap() == "a.jsonl" {
            Err(std::io::ErrorKind::PermissionDenied.into())
        } else {
            super::super::codex::probe(p, 1 << 20)
        }
    });
    let mut restored = Cache::at(p);
    codex(&mut restored, &d);
    assert_eq!(restored.probes, 1)
}
// upstream: LocalUsageCacheTests.swift::testCodexSessionIndexVersionBumpRebuildsIndexWithoutReparsingBlobs
#[test]
fn index_version_preserves_blobs() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.0.join("cache.bin");
    Cache::at(p.clone()).entries_since(Provider::Codex, &d.roots(), 2000., 4000.);
    let mut s = snapshot(&p);
    s["index_version"] = json!(0);
    save_snapshot(&p, &s);
    let mut c = Cache::at(p);
    c.entries_since(Provider::Codex, &d.roots(), 2000., 4000.);
    assert_eq!(c.parsed_files, 0);
    assert_eq!(c.probes, 2)
}
// upstream: LocalUsageCacheTests.swift::testCodexSessionIndexReprobesOnlyChangedFile
#[test]
fn index_only_changed_probe() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.0.join("cache.bin");
    codex(&mut Cache::at(p.clone()), &d);
    let a = d.write("a.jsonl", lines(&[meta("changed-longer", None, false)]));
    stamp(&a, 1500);
    let mut c = Cache::at(p);
    codex(&mut c, &d);
    assert_eq!(c.probes, 1)
}
// upstream: LocalUsageCacheTests.swift::testCodexSessionIndexOutlivesBlobPrune
#[test]
fn index_survives_blob_prune() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.0.join("cache.bin");
    codex(&mut Cache::at(p.clone()), &d);
    let s = snapshot(&p);
    assert!(s["blobs"].as_object().unwrap().is_empty());
    assert_eq!(s["index"].as_object().unwrap().len(), 3);
    let mut c = Cache::at(p);
    codex(&mut c, &d);
    assert_eq!(c.probes, 0);
    assert_eq!(c.parsed_files, 1)
}
// upstream: LocalUsageCacheTests.swift::testCodexSessionIndexDropsDeletedFiles
#[test]
fn deleted_index_removed_and_saved() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.0.join("cache.bin");
    let mut c = Cache::at(p.clone());
    codex(&mut c, &d);
    assert_eq!(c.index_count(), 3);
    fs::remove_file(d.0.join("a.jsonl")).unwrap();
    c.entries_since(Provider::Codex, &d.roots(), 2000., 10000120.);
    assert_eq!(c.index_count(), 2);
    assert_eq!(Cache::at(p).index_count(), 2)
}
fn parent(d: &Temp, name: &str) {
    let p = d.write(
        name,
        lines(&[
            meta("P-123", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
            event(3., Some(v(300, 30)), v(200, 20)),
        ]),
    );
    stamp(&p, 1000)
}
// upstream: LocalUsageCacheTests.swift::testCodexStaleFilenameHintDoesNotBlockContentProbe
#[test]
fn false_filename_hint_does_not_hide_parent() {
    let d = Temp::new();
    orphan_tree(&d);
    let p = d.write("rollout-P-123.jsonl", lines(&[meta("decoy", None, false)]));
    stamp(&p, 1000);
    parent(&d, "real.jsonl");
    assert_eq!(total(&codex(&mut Cache::default(), &d)), [330])
}
// upstream: LocalUsageCacheTests.swift::testCodexRestoredParentIsFoundAfterNegativeProbe
#[test]
fn restored_parent_found() {
    let d = Temp::new();
    orphan_tree(&d);
    let mut c = Cache::default();
    assert_eq!(total(&codex(&mut c, &d)), [220, 330]);
    parent(&d, "real.jsonl");
    assert_eq!(total(&codex(&mut c, &d)), [330])
}
// upstream: LocalUsageCacheTests.swift::testCodexParsedRolloutCacheRoundTripsAcrossInstances
#[test]
fn codex_disk_roundtrip() {
    let d = Temp::new();
    let source = d.write(
        "a.jsonl",
        lines(&[
            meta("s", None, false),
            event(1., Some(v(100, 10)), v(100, 10)),
        ]),
    );
    stamp(&source, 5000);
    let p = d.0.join("cache.bin");
    let a = Cache::at(p.clone()).entries_since(Provider::Codex, &d.roots(), 0., 6000.);
    d.write(
        "a.jsonl",
        lines(&[
            meta("s", None, false),
            event(1., Some(v(100, 20)), v(100, 20)),
        ]),
    );
    stamp(&source, 5000);
    let mut c = Cache::at(p);
    assert_eq!(c.entries_since(Provider::Codex, &d.roots(), 0., 6000.), a);
    assert_eq!(c.parsed_files, 0)
}
// upstream: LocalUsageCacheTests.swift::testDiskRoundTripAcrossInstances
#[test]
fn claude_disk_roundtrip() {
    let d = Temp::new();
    let source = d.write("a.jsonl", lines(&[a("a", 42)]));
    stamp(&source, 9000000);
    let p = d.0.join("cache.bin");
    entries(&mut Cache::at(p.clone()), &d);
    d.write("a.jsonl", lines(&[a("a", 43)]));
    stamp(&source, 9000000);
    let mut c = Cache::at(p);
    assert_eq!(entries(&mut c, &d)[0].output, 42);
    assert_eq!(c.parsed_files, 0)
}
// upstream: LocalUsageCacheTests.swift::testPruneDropsBlobsOlderThan40Days
#[test]
fn prune_old_disk_blobs() {
    let d = Temp::new();
    let old = d.write("old.jsonl", lines(&[a("old", 1)]));
    stamp(&old, 1);
    d.write("new.jsonl", lines(&[a("new", 2)]));
    let p = d.0.join("cache.bin");
    let e = entries(&mut Cache::at(p.clone()), &d);
    assert_eq!(e.len(), 2);
    let s = snapshot(&p).to_string();
    assert!(!s.contains("old.jsonl"));
    assert!(s.contains("new.jsonl"))
}
// upstream: LocalUsageCacheTests.swift::testSaveThrottle60s
#[test]
fn save_throttle() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 1)]));
    let p = d.0.join("cache.bin");
    let mut c = Cache::at(p.clone());
    c.entries_since(Provider::Claude, &d.roots(), 0., 10000000.);
    let first = fs::read(&p).unwrap();
    d.write("a.jsonl", lines(&[a("a", 22)]));
    c.entries_since(Provider::Claude, &d.roots(), 0., 10000030.);
    assert_eq!(fs::read(&p).unwrap(), first);
    c.entries_since(Provider::Claude, &d.roots(), 0., 10000091.);
    assert_ne!(fs::read(&p).unwrap(), first)
}
// upstream: LocalUsageCacheTests.swift::testModifiedSinceFilters
#[test]
fn mtime_filter() {
    let d = Temp::new();
    let p = d.write("old.jsonl", lines(&[a("old", 1)]));
    stamp(&p, 1000);
    let p = d.write("new.jsonl", lines(&[a("new", 2)]));
    stamp(&p, 3000);
    let e = Cache::default().entries_since(Provider::Claude, &d.roots(), 2000., 4000.);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].output, 2)
}
// upstream: LocalUsageCacheTests.swift::testLegacyPlainJSONCacheLoads
#[test]
fn plain_snapshot_compatibility() {
    let d = Temp::new();
    d.write("a.jsonl", lines(&[a("a", 42)]));
    let p = d.0.join("cache.bin");
    entries(&mut Cache::at(p.clone()), &d);
    save_snapshot(&p, &snapshot(&p));
    let mut c = Cache::at(p);
    assert_eq!(entries(&mut c, &d)[0].output, 42);
    assert_eq!(c.parsed_files, 0)
}
// upstream: LocalUsageCacheTests.swift::testSnapshotIsCompressed
#[test]
fn compressed_snapshot() {
    let d = Temp::new();
    d.write(
        "a.jsonl",
        lines(&(0..50).map(|i| a(&format!("id{i}"), i)).collect::<Vec<_>>()),
    );
    let p = d.0.join("cache.bin");
    entries(&mut Cache::at(p.clone()), &d);
    let plain = serde_json::to_vec(&snapshot(&p)).unwrap();
    assert!(fs::read(p).unwrap().len() < plain.len())
}
// upstream: LocalUsageCacheTests.swift::testCodexEntriesParseModelAndCachedInput
#[test]
fn codex_payload_context_model() {
    let d = Temp::new();
    let mut context = ctx("gpt-5.5-codex");
    context["payload"]["type"] = json!("turn_context");
    context.as_object_mut().unwrap().remove("type");
    d.write(
        "a.jsonl",
        lines(&[context, event(1., None, [100, 80, 0, 7, 0, 107])]),
    );
    let e = Cache::default().entries(Provider::Codex, &d.roots());
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].model, "gpt-5.5-codex");
    assert_eq!(
        (e[0].input, e[0].cache_read, e[0].output, e[0].cache_write),
        (20, 80, 7, 0)
    )
}
// upstream: LocalUsageReaderTests.swift::testDegenerateParentHintIsNotUsedToNarrowCandidates
// Rust indexes complete metadata IDs instead of using filename-substring hints.
#[test]
fn degenerate_parent_ids_never_trigger_whole_tree_parse() {
    for id in [
        "-",
        "",
        "----",
        "parent",
        "00000000-0000-7000-8000-000000000001",
    ] {
        let d = Temp::new();
        for n in 0..5 {
            let p = d.write(
                &format!("rollout-{n}.jsonl"),
                lines(&[meta(&format!("other-{n}"), None, false)]),
            );
            stamp(&p, 1000)
        }
        let p = d.write(
            "child.jsonl",
            lines(&[
                meta("child", Some(id), false),
                event(1., Some(v(100, 10)), v(100, 10)),
                event(3., Some(v(300, 30)), v(200, 20)),
            ]),
        );
        stamp(&p, 3000);
        let mut c = Cache::default();
        codex(&mut c, &d);
        assert_eq!(c.parsed_files, 1);
        assert_eq!(c.probes, 5);
        codex(&mut c, &d);
        assert_eq!(c.parsed_files, 1);
        assert_eq!(c.probes, 5)
    }
}

#[test]
fn compressed_snapshot_expansion_is_bounded() {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&vec![b' '; 4096]).unwrap();
    let bytes = encoder.finish().unwrap();
    assert!(bytes.len() < 256);
    assert!(super::super::cache::inflate_limited(&bytes, 256).is_err());
    assert_eq!(
        super::super::cache::inflate_limited(&bytes, 4096)
            .unwrap()
            .len(),
        4096
    )
}
#[test]
fn oversized_snapshot_is_ignored() {
    let d = Temp::new();
    let p = d.0.join("cache.bin");
    fs::File::create(&p)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(Cache::at(p).blob_count(), 0)
}
#[test]
fn scan_io_error_is_distinct_from_successful_empty() {
    let d = Temp::new();
    let loop_path = d.0.join("loop");
    std::os::unix::fs::symlink(&loop_path, &loop_path).unwrap();
    let mut c = Cache::default();
    let report = super::super::scan(
        Provider::Claude,
        &Roots(vec![loop_path]),
        &mut c,
        crate::usage::now(),
    );
    assert!(report.error.is_some());
    let empty = Temp::new();
    let report = super::super::scan(
        Provider::Claude,
        &empty.roots(),
        &mut c,
        crate::usage::now(),
    );
    assert!(report.error.is_none());
    assert_eq!(report.today(), 0)
}

// upstream: CustomScanRootsTests.swift::testCodexCacheKeepsSessionIndexAcrossMultipleRoots
// upstream: CustomScanRootsTests.swift::testCodexCacheFindsParentInASiblingRoot
#[test]
fn codex_parent_index_spans_default_and_custom_roots_without_reprobing() {
    let a = Temp::new();
    let b = Temp::new();
    let parent = a.write(
        "parent.jsonl",
        lines(&[
            meta("P-123", None, false),
            event(5., Some(v(100, 10)), v(100, 10)),
            event(10., Some(v(300, 30)), v(200, 20)),
        ]),
    );
    stamp(&parent, 1000);
    let child = b.write("child.jsonl", lines(&fork()));
    stamp(&child, 3000);
    let roots = Roots::new(vec![a.0.clone(), b.0.clone()]);
    let mut c = Cache::default();
    let first = c.entries_since(Provider::Codex, &roots, 2000., 10000000.);
    assert_eq!(total(&first), [330]);
    assert_eq!(c.index_count(), 2);
    let probes = c.probes;
    let parses = c.parsed_files;
    assert_eq!(
        c.entries_since(Provider::Codex, &roots, 2000., 10000000.),
        first
    );
    assert_eq!(c.probes, probes);
    assert_eq!(c.parsed_files, parses);
}

#[test]
fn larger_claude_replacement_restarts_instead_of_reusing_an_append_offset() {
    for atomic in [false, true] {
        let d = Temp::new();
        let path = d.write("session.jsonl", lines(&[a("old", 7)]));
        let mut cache = Cache::default();
        assert_eq!(entries(&mut cache, &d)[0].output, 7);
        let replacement = lines(&[
            a("new-record-with-a-longer-id", 80),
            a("other-new-record", 90),
        ]);
        if atomic {
            let new = d.write("replacement.tmp", replacement);
            fs::rename(new, &path).unwrap();
        } else {
            fs::write(&path, replacement).unwrap();
        }
        let refreshed = entries(&mut cache, &d);
        assert_eq!(refreshed.len(), 2);
        assert_eq!(refreshed.iter().map(|e| e.output).sum::<u64>(), 170);
        assert!(refreshed.iter().all(|e| !e.id.starts_with("old|")));
    }
}

#[test]
fn inaccessible_parent_retained_in_index_does_not_panic_or_duplicate_fork_replay() {
    let d = Temp::new();
    let parent = d.write(
        "parents/p.jsonl",
        lines(&[
            meta("parent", None, false),
            event(1., Some([100, 0, 0, 10, 0, 110]), [100, 0, 0, 10, 0, 110]),
        ]),
    );
    d.write(
        "children/c.jsonl",
        lines(&[
            meta("child", Some("parent"), false),
            event(1., Some([100, 0, 0, 10, 0, 110]), [100, 0, 0, 10, 0, 110]),
            event(5., Some([200, 0, 0, 20, 0, 220]), [100, 0, 0, 10, 0, 110]),
        ]),
    );
    let mut cache = Cache::default();
    let all = cache.entries(Provider::Codex, &d.roots());
    assert_eq!(all.iter().map(Entry::total).sum::<u64>(), 220);
    fs::remove_file(parent).unwrap();
    let broken = d.0.join("unreadable");
    std::os::unix::fs::symlink(&broken, &broken).unwrap();
    let roots = Roots(vec![d.0.join("children"), broken]);
    let child = cache.entries(Provider::Codex, &roots);
    assert!(cache.last_error.is_some());
    assert_eq!(child.iter().map(Entry::total).sum::<u64>(), 110);
    assert_eq!(cache.entries(Provider::Codex, &roots), child);
}
