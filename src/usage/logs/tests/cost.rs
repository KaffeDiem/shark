use super::*;
use crate::pokemon::format::{Language, compact_cost, cost, percent};
fn e(model: &str) -> Entry {
    Entry {
        id: model.into(),
        date: epoch(15),
        model: model.into(),
        input: 1000,
        output: 100,
        cache_read: 500,
        ..Entry::default()
    }
}
// upstream: UsageCostTests.swift::testPlainDollarFormattingAcrossCoverageAndLanguages
#[test]
fn plain_dollar_all_languages() {
    for l in Language::ALL {
        for coverage in [
            CostCoverage {
                reported: true,
                ..Default::default()
            },
            CostCoverage {
                estimated: true,
                ..Default::default()
            },
            CostCoverage {
                reported: true,
                unknown: true,
                ..Default::default()
            },
            CostCoverage {
                estimated: true,
                unknown: true,
                ..Default::default()
            },
        ] {
            let b = Bucket {
                cost: 12.5,
                coverage,
                ..Default::default()
            };
            assert_eq!(b.localized_cost_text(l, false), "$12.50");
            assert_eq!(b.localized_cost_text(l, true), "$12.5")
        }
    }
}
// upstream: UsageCostTests.swift::testExplicitZeroIsNotMissingAndNeverFallsBackToModelRate
#[test]
fn explicit_zero_is_known() {
    let mut zero = e("gpt-5.5");
    zero.explicit_cost = Some(0.);
    let b = bucket(&[zero]);
    assert_eq!(b.cost, 0.);
    assert!(b.coverage.reported);
    assert!(!b.coverage.estimated);
    assert_eq!(b.cost_text(false), "$0.00");
    let b = bucket(&[e("gpt-5.5")]);
    close(b.cost, 0.00825);
    assert!(b.coverage.estimated);
    assert_eq!(b.cost_text(false), "$0.01")
}
// upstream: UsageCostTests.swift::testUnavailableAndPartialCostsSurviveEveryAggregation
#[test]
fn partial_coverage_survives_all_reports() {
    let unknown = e("not-priced");
    let b = bucket(&[unknown.clone()]);
    assert_eq!(b.total(), 1600);
    assert_eq!(b.localized_cost_text(Language::Ko, false), "계산 불가");
    assert_eq!(b.cost_text(true), "$—");
    let mut reported = e("gpt-5.5");
    reported.explicit_cost = Some(0.);
    let report = Report::from_entries(vec![e("gpt-5.5"), unknown, reported], epoch(15));
    let expected = CostCoverage {
        reported: true,
        estimated: true,
        unknown: true,
    };
    assert_eq!(report.totals.coverage, expected);
    assert_eq!(
        report.bucket_between(epoch(14), epoch(16)).coverage,
        expected
    );
    assert_eq!(
        report
            .active_block(epoch(15) + 60.)
            .unwrap()
            .bucket
            .coverage,
        expected
    );
    assert!(
        report
            .daily_buckets()
            .values()
            .all(|b| b.coverage == expected)
    );
    assert_eq!(report.totals.total(), 4800);
    assert_eq!(report.totals.cost_text(false), "$0.01");
    assert!(
        report
            .totals
            .cost_explanation(Language::En)
            .contains("Some usage")
    );
    let mut merged = Bucket::default();
    for b in report.daily_buckets().values() {
        merged.merge(b)
    }
    assert_eq!(merged.coverage, expected)
}
// upstream: UsageCostTests.swift::testZeroUsageDoesNotTurnUnknownCostIntoPartialZero
#[test]
fn empty_usage_does_not_claim_estimate() {
    let b = bucket(&[
        e("unknown"),
        Entry {
            model: "gpt-5.5".into(),
            ..Default::default()
        },
    ]);
    assert!(b.coverage.unknown);
    assert!(!b.coverage.has_known());
    assert_eq!(b.cost_text(false), "Unavailable")
}
// upstream: UsageCostTests.swift::testUnrecoverableBucketSplitAndInvalidSourceCost
#[test]
fn invalid_source_cost_fallback() {
    for bad in [f64::NAN, f64::INFINITY, -1.] {
        let mut entry = e("gpt-5.5");
        entry.explicit_cost = Some(bad);
        let b = bucket(&[entry]);
        assert!(b.coverage.estimated);
        assert!(b.cost.is_finite())
    }
    let mut unavailable = e("gpt-5.5");
    unavailable.cost_unavailable = Some(true);
    assert!(bucket(&[unavailable.clone()]).coverage.unknown);
    unavailable.explicit_cost = Some(0.5);
    assert!(bucket(&[unavailable.clone()]).coverage.reported);
    unavailable.explicit_cost = Some(0.);
    unavailable.cost_is_estimate = Some(true);
    assert!(bucket(&[unavailable]).coverage.estimated)
}
// upstream: UsageCostTests.swift::testLegacyDecodingAndEntryMetadataRoundTrip
#[test]
fn legacy_cost_coverage_and_metadata() {
    for raw in [
        json!({"date":"2026-09-11","totalTokens":500,"totalCost":0}),
        json!({"totalTokens":500,"costUSD":0}),
    ] {
        let b: Bucket = serde_json::from_value(raw).unwrap();
        assert!(b.coverage.unknown);
        assert_eq!(b.total(), 500)
    }
    let b: Bucket =
        serde_json::from_value(json!({"totalTokens":500,"totalCost":1,"costUSD":1})).unwrap();
    assert!(b.coverage.estimated);
    let b: Bucket = serde_json::from_value(json!({})).unwrap();
    assert_eq!(b.coverage, CostCoverage::default());
    let mut entry = e("gpt-5.5");
    entry.explicit_cost = Some(0.);
    entry.cost_is_estimate = Some(true);
    entry.cost_unavailable = Some(true);
    let mut encoded = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        serde_json::from_value::<Entry>(encoded.clone()).unwrap(),
        entry
    );
    encoded.as_object_mut().unwrap().remove("cost_is_estimate");
    encoded.as_object_mut().unwrap().remove("cost_unavailable");
    let legacy: Entry = serde_json::from_value(encoded).unwrap();
    assert_eq!(legacy.cost_is_estimate, None);
    assert_eq!(legacy.cost_unavailable, None)
}
// upstream: UsageCostTests.swift::testSonnet55ClaudeLogPricesColdWarmAndPersistedTokenCaches
#[test]
fn claude_cost_cold_warm_persisted() {
    let d = Temp::new();
    let bytes = lines(&[assistant("a", "claude-sonnet-5-5", 100, 10, 40, 1000, 15)]);
    let source = d.write("s.jsonl", &bytes);
    let time = fs::metadata(&source).unwrap().modified().unwrap();
    let path = d.0.join("cache.bin");
    let mut c = Cache::at(path.clone());
    let check = |es: Vec<Entry>| {
        assert_eq!(es.len(), 1);
        assert_eq!(es[0].explicit_cost, None);
        let r = Report::from_entries(es, epoch(15) + 60.);
        assert_eq!(r.today(), 1150);
        for b in [
            r.totals.clone(),
            r.bucket_between(epoch(14), epoch(16)),
            r.active_block(epoch(15) + 60.).unwrap().bucket,
        ] {
            close(b.cost, 0.0005);
            assert!(b.coverage.estimated)
        }
    };
    check(c.entries(Provider::Claude, &d.roots()));
    d.write("s.jsonl", vec![b' '; bytes.len()]);
    fs::File::options()
        .write(true)
        .open(source)
        .unwrap()
        .set_modified(time)
        .unwrap();
    check(c.entries(Provider::Claude, &d.roots()));
    check(Cache::at(path).entries(Provider::Claude, &d.roots()));
    assert_eq!(c.parsed_files, 1)
}
// upstream: UsageCostTests.swift::testCodexProviderPricesColdAndWarmCacheAndMarksTotalOnlyUnknown
#[test]
fn codex_cost_cache_preserves_partial_coverage() {
    let d = Temp::new();
    d.write(
        "a.jsonl",
        lines(&[
            ctx("gpt-5.5"),
            event(1., None, [1500, 500, 0, 100, 0, 1600]),
            event(2., None, [0, 0, 0, 0, 0, 70]),
        ]),
    );
    let path = d.0.join("cache.bin");
    for _ in 0..2 {
        let es = Cache::at(path.clone()).entries(Provider::Codex, &d.roots());
        let b = bucket(&es);
        assert_eq!(b.total(), 1670);
        close(b.cost, 0.00825);
        assert_eq!(
            b.coverage,
            CostCoverage {
                estimated: true,
                unknown: true,
                reported: false
            }
        );
        let r = Report::from_entries(es, epoch(15) + 60.);
        assert_eq!(r.totals.coverage, b.coverage)
    }
}
// upstream: LocalUsageReaderTests.swift::testPricingExactAndFallbackAndZero
#[test]
fn legacy_price_identities() {
    use super::super::pricing::*;
    close(estimate("claude-opus-4-8", 1000000, 0, 0, 0).unwrap(), 5.);
    close(estimate("claude-opus-4-8", 0, 1000000, 0, 0).unwrap(), 25.);
    close(
        estimate("claude-haiku-4-5-20251001", 1000000, 0, 0, 0).unwrap(),
        1.,
    );
    close(
        estimate("claude-fable-5", 1000000, 1000000, 1000000, 1000000).unwrap(),
        73.5,
    );
    close(
        estimate("claude-fable-5-1", 0, 0, 0, 1000000).unwrap(),
        0.25,
    );
    for m in [
        "claude-opus-4-99",
        "claude-fable-6",
        "claude-opus-6",
        "totally-unknown",
    ] {
        assert!(rate(m).is_none())
    }
}
// upstream: LocalUsageReaderTests.swift::testPeriodAndActiveBlock
#[test]
fn period_and_active_window() {
    let now = epoch(15);
    let recent = Entry {
        date: now - 1800.,
        input: 600000,
        ..e("claude-opus-4-8")
    };
    let old = Entry {
        date: now - 36000.,
        input: 999,
        output: 0,
        cache_read: 0,
        ..e("claude-opus-4-8")
    };
    let mut recent = recent;
    recent.output = 0;
    recent.cache_read = 0;
    let r = Report::from_entries(vec![recent, old], now);
    let b = r.active_block(now).unwrap();
    assert_eq!(b.bucket.total(), 600000);
    assert!(b.tokens_per_minute > 0.);
    assert_eq!(r.bucket_between(now - 43200., now + 43200.).total(), 600999)
}
// upstream: LocalUsageReaderTests.swift::testActiveBlockIgnoresZeroTokenSyntheticEntries
#[test]
fn synthetic_zero_not_active() {
    let now = epoch(15);
    let synthetic = Entry {
        date: now - 120.,
        model: "<synthetic>".into(),
        ..Default::default()
    };
    assert!(
        Report::from_entries(vec![synthetic.clone()], now)
            .active_block(now)
            .is_none()
    );
    let actual = Entry {
        date: now - 60.,
        input: 10,
        output: 5,
        ..Default::default()
    };
    let block = Report::from_entries(vec![synthetic, actual], now)
        .active_block(now)
        .unwrap();
    assert_eq!(block.start, now - 60.);
    assert_eq!(block.bucket.total(), 15)
}
// upstream: LocalUsageReaderTests.swift::testClaudeParsingClampsAbsurdTokenCountsInsteadOfTrapping
#[test]
fn claude_absurd_tokens_bounded() {
    let mut v = assistant("a", "claude", 0, 0, 0, 0, 15);
    v["message"]["usage"]["input_tokens"] = json!(1e30);
    v["message"]["usage"]["output_tokens"] = json!(1e30);
    let p = super::super::claude::parse(&lines(&[v]), Path::new("a"), 0);
    assert_eq!(p.entries[0].input, 1000000000000000);
    assert_eq!(p.entries[0].output, 1000000000000000)
}
// upstream: LocalUsageReaderTests.swift::testCodexLastUsageClampsAbsurdTokenCountsInsteadOfTrapping
#[test]
fn codex_absurd_tokens_bounded() {
    let mut v = event(1., Some([10, 0, 0, 5, 0, 15]), [0; 6]);
    v["payload"]["info"]["last_token_usage"]["input_tokens"] = json!(1e30);
    v["payload"]["info"]["last_token_usage"]["output_tokens"] = json!(1e30);
    let p = rollout("a", &[v]);
    assert_eq!(p.events[0].entry.input, 1000000000000000);
    assert_eq!(p.events[0].entry.output, 1000000000000000)
}
// upstream: LocalUsageReaderTests.swift::testParsingStillFoldsMissingAndNegativeToZero
#[test]
fn malformed_token_fields_zero() {
    let mut v = assistant("a", "claude", 0, 0, 0, 0, 15);
    v["message"]["usage"] =
        json!({"input_tokens":-5,"output_tokens":null,"cache_read_input_tokens":"nope"});
    let p = super::super::claude::parse(&lines(&[v]), Path::new("a"), 0);
    assert_eq!(p.entries[0].total(), 0)
}
// upstream: LocalUsageCacheTests.swift::testFormatterRoundingBoundaries
#[test]
fn rounded_cost_percent_boundaries() {
    for (n, w) in [
        (79.96, "80%"),
        (99.97, "100%"),
        (0.04, "0%"),
        (79.94, "79.9%"),
    ] {
        assert_eq!(percent(n), w)
    }
    for (n, w) in [
        (99.96, "$100"),
        (99.94, "$99.9"),
        (9999.6, "$10.0K"),
        (9999.4, "$9999"),
        (100., "$100"),
    ] {
        assert_eq!(compact_cost(n), w)
    }
}
#[test]
fn finite_cost_saturates() {
    let b = bucket(&[
        Entry {
            explicit_cost: Some(f64::MAX),
            ..e("unknown")
        },
        Entry {
            explicit_cost: Some(f64::MAX),
            ..e("unknown")
        },
    ]);
    assert_eq!(b.cost, f64::MAX);
    let mut merged = b.clone();
    merged.merge(&b);
    assert!(merged.cost.is_finite())
}
#[test]
fn all_localized_cost_states() {
    for l in Language::ALL {
        for coverage in [
            CostCoverage {
                estimated: true,
                ..Default::default()
            },
            CostCoverage {
                reported: true,
                ..Default::default()
            },
            CostCoverage {
                unknown: true,
                ..Default::default()
            },
            CostCoverage {
                estimated: true,
                unknown: true,
                ..Default::default()
            },
        ] {
            let b = Bucket {
                cost: 12.5,
                coverage,
                ..Default::default()
            };
            assert!(!b.localized_cost_text(l, false).is_empty());
            assert!(!b.cost_explanation(l).is_empty())
        }
    }
}
// upstream: LocalUsageCacheTests.swift::testDateHelpers
#[test]
fn calendar_boundaries() {
    use super::super::timing::*;
    use chrono::{Datelike, Local, TimeZone};
    let d = Local.with_ymd_and_hms(2026, 7, 2, 15, 0, 0).unwrap();
    let now = d.timestamp() as f64;
    let month = Local.timestamp_opt(month_start(now) as i64, 0).unwrap();
    assert_eq!((month.year(), month.month(), month.day()), (2026, 7, 1));
    let week = week_start(now);
    assert!(week <= now);
    assert!(now - week < 7. * 86400.);
    assert_eq!(month_key(now), "2026-07")
}
// upstream: LocalUsageReaderTests.swift::testEnrichmentScanStartCoversAllWindows
#[test]
fn scan_floor_covers_calendar_windows() {
    use super::super::timing::*;
    use chrono::{Local, TimeZone};
    let mut straddles = 0;
    for month in 1..=12 {
        let now = Local
            .with_ymd_and_hms(2026, month, 1, 2, 0, 0)
            .unwrap()
            .timestamp() as f64;
        let start = enrichment_start(now);
        assert!(start <= month_start(now));
        assert!(start <= week_start(now));
        assert!(start <= now - 5. * 3600.);
        assert!(start <= now - 7. * 86400.);
        if week_start(now) < month_start(now) {
            straddles += 1;
            assert!(start < month_start(now))
        }
    }
    assert!(straddles > 0);
    let mid = Local
        .with_ymd_and_hms(2026, 7, 20, 12, 0, 0)
        .unwrap()
        .timestamp() as f64;
    assert_eq!(enrichment_start(mid), month_start(mid))
}
// upstream: LocalUsageCacheTests.swift::testEnrichmentScanStartCatchesWeekStraddlingMonthBoundary
#[test]
fn scan_floor_keeps_previous_month_week_usage() {
    use super::super::timing::*;
    use chrono::{DateTime, Local, TimeZone, Utc};
    let now = Local
        .with_ymd_and_hms(2026, 7, 1, 12, 0, 0)
        .unwrap()
        .timestamp() as f64;
    let week = week_start(now);
    let month = month_start(now);
    assert!(week < month);
    let timestamp = week + 3600.;
    let mut row = assistant("straddle", "claude-sonnet-5", 100, 500, 0, 0, 15);
    row["timestamp"] = json!(
        DateTime::<Utc>::from_timestamp(timestamp as i64, 0)
            .unwrap()
            .to_rfc3339()
    );
    let d = Temp::new();
    let p = d.write("straddle.jsonl", lines(&[row]));
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(timestamp as u64))
        .unwrap();
    let mut c = Cache::default();
    assert!(
        c.entries_since(Provider::Claude, &d.roots(), month, now)
            .is_empty()
    );
    let es = c.entries_since(Provider::Claude, &d.roots(), enrichment_start(now), now);
    assert_eq!(es.len(), 1);
    assert_eq!(
        Report::from_entries(es, now)
            .bucket_between(week, now + 1.)
            .total(),
        600
    )
}
// upstream: UsageCostTests.swift::testOptionalLocalCodexCostAudit
// The upstream opt-in diagnostic is adapted to immutable fixtures: CI never reads user logs.
#[test]
fn fixture_codex_cost_audit() {
    let d = Temp::new();
    d.write(
        "a.jsonl",
        lines(&[
            ctx("gpt-5.5"),
            event(1., None, [1500, 500, 0, 100, 0, 1600]),
            event(2., None, [0, 0, 0, 0, 0, 70]),
        ]),
    );
    let es = Cache::default().entries(Provider::Codex, &d.roots());
    let b = bucket(&es);
    assert_eq!(b.total(), es.iter().map(Entry::total).sum::<u64>());
    assert!(b.cost > 0.);
    assert!(b.coverage.estimated);
    let unknown = es
        .iter()
        .filter(|e| {
            e.cost_unavailable == Some(true)
                || super::super::pricing::estimate(
                    &e.model,
                    e.input,
                    e.output,
                    e.cache_write,
                    e.cache_read,
                )
                .is_none()
        })
        .map(Entry::total)
        .sum::<u64>();
    assert_eq!(unknown, 70);
    assert_eq!(b.cost_text(false), "$0.01")
}

// upstream: LocalUsageCacheTests.swift::testFormatterEdges
#[test]
fn grouped_and_cost_edges() {
    use crate::pokemon::format::grouped;
    assert_eq!(grouped(253412890, ','), "253,412,890");
    assert_eq!(grouped(253412890, '.'), "253.412.890");
    assert_eq!(cost(48.104), "$48.10");
    assert_eq!(compact_cost(9.54), "$9.5");
    assert_eq!(compact_cost(311.4), "$311");
    assert_eq!(compact_cost(12340.), "$12.3K");
    assert_eq!(percent(88.), "88%");
    assert_eq!(percent(88.35), "88.3%")
}
