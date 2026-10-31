//! Optional read-only corpus audits. Never infer the user's home or inspect account credentials.
use super::*;
use std::time::Instant;
fn corpus() -> PathBuf {
    let root=std::env::var_os("SHARK_POKEMON_PARITY_CORPUS").map(PathBuf::from).expect("Set SHARK_POKEMON_PARITY_CORPUS to a sanitized log corpus with claude/ and codex/ subdirectories");
    assert!(root.is_dir());
    root
}
fn sources() -> Vec<(Provider, Report)> {
    let home = corpus();
    Provider::ALL
        .into_iter()
        .map(|provider| {
            let dir = match provider {
                Provider::Claude => "claude",
                Provider::Codex => "codex",
            };
            let roots = Roots::new(vec![home.join(dir)]);
            let report =
                super::super::scan(provider, &roots, &mut Cache::default(), crate::usage::now());
            assert!(report.error.is_none(), "{dir}: {:?}", report.error);
            (provider, report)
        })
        .collect()
}
// upstream: LocalUsageParityTests.swift::testFullRefreshPipelineWithRealProviders
#[test]
#[ignore = "Requires explicitly provided recent sanitized Claude/Codex log corpus"]
fn full_local_report_pipeline_from_explicit_corpus() {
    let reports = sources();
    let summary = crate::usage::summary::combined::Summary::from_sources(
        reports
            .iter()
            .map(|(p, r)| crate::usage::summary::combined::Source {
                id: p.source().id(),
                name: p.name(),
                report: r,
                reports_cost: true,
            }),
        crate::usage::now(),
    );
    assert_eq!(summary.providers.len(), 2);
    assert!(
        summary.today.total() > 0,
        "corpus must contain today's usage"
    );
    assert!(summary.month.total() > 0);
    assert!(summary.providers.iter().all(|p| p.error.is_none()));
    assert_eq!(
        summary.today_by_provider.values().copied().sum::<u64>(),
        summary.today.total()
    );
}
// upstream: LocalUsageParityTests.swift::testPrintRealAggregates
#[test]
#[ignore = "Requires explicitly provided sanitized Claude/Codex log corpus"]
fn print_explicit_corpus_aggregates_without_transcript_contents() {
    for (provider, report) in sources() {
        assert!(!report.entries.is_empty());
        println!(
            "{}: {} retained tokens, {} entries, cost {}",
            provider.name(),
            report.totals.total(),
            report.entries.len(),
            report.totals.cost_text(false)
        );
        for day in report.month_daily(crate::usage::now()) {
            println!("{} {} {}", provider.name(), day.date, day.bucket.total());
        }
    }
}
fn files(root: &Path, out: &mut Vec<PathBuf>) {
    assert!(out.len() < 20000, "audit corpus too large");
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            files(&entry.path(), out)
        } else if entry.path().extension().is_some_and(|s| s == "jsonl") {
            out.push(entry.path())
        }
    }
}
// upstream: LocalUsageParityTests.swift::testCodexMetadataProbeOnRealRollouts
#[test]
#[ignore = "Requires explicitly provided sanitized Codex rollout corpus"]
fn metadata_probe_matches_full_parser_on_explicit_corpus() {
    let mut paths = vec![];
    files(&corpus().join("codex"), &mut paths);
    assert!(!paths.is_empty());
    let mut sessions = 0;
    for path in paths {
        let probed = super::super::codex::probe(&path, 1 << 20).unwrap();
        let parsed = super::super::codex::parse_reader(
            std::io::BufReader::new(fs::File::open(&path).unwrap()),
            &path,
        )
        .unwrap();
        sessions += usize::from(parsed.session.is_some());
        assert_eq!(probed, parsed.session, "{}", path.display());
    }
    assert!(sessions > 0);
}
// upstream: LocalUsageParityTests.swift::testCodexOrphanedParentDoesNotRescanRealTree
#[test]
#[ignore = "Requires explicitly provided sanitized Codex rollout corpus"]
fn orphaned_fork_reuses_real_corpus_metadata_index() {
    let temp = Temp::new();
    temp.write(
        "orphan.jsonl",
        lines(&[
            meta("audit-orphan", Some("missing-audit-parent"), false),
            event(1., Some([100, 0, 0, 10, 0, 110]), [100, 0, 0, 10, 0, 110]),
        ]),
    );
    let roots = Roots::new(vec![corpus().join("codex"), temp.0.clone()]);
    let mut cache = Cache::default();
    let first = cache.entries(Provider::Codex, &roots);
    let (probes, parses) = (cache.probes, cache.parsed_files);
    let second = cache.entries(Provider::Codex, &roots);
    assert_eq!(first, second);
    assert_eq!(cache.probes, probes);
    assert_eq!(cache.parsed_files, parses);
    assert!(cache.index_count() > 1);
}
// upstream: LocalUsageParityTests.swift::testCachePerformance
#[test]
#[ignore = "Requires explicitly provided sanitized Claude/Codex log corpus"]
fn unchanged_corpus_reuses_parsed_files_and_bytes() {
    let root = corpus();
    for provider in Provider::ALL {
        let dir = match provider {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        };
        let roots = Roots::new(vec![root.join(dir)]);
        let mut cache = Cache::default();
        let start = Instant::now();
        let first = cache.entries(provider, &roots);
        let cold = start.elapsed();
        assert!(!first.is_empty());
        let (parsed, bytes) = (cache.parsed_files, cache.parsed_bytes);
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(cache.entries(provider, &roots), first);
        }
        assert_eq!(cache.parsed_files, parsed);
        assert_eq!(cache.parsed_bytes, bytes);
        println!(
            "{dir}: cold={cold:?}, three warm={:?}, files={parsed}",
            start.elapsed()
        );
    }
}
