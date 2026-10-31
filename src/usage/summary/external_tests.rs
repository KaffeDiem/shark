use super::external::*;
use super::*;

// upstream: PerformanceTests.swift::testLargeDailyReportDecode
#[test]
fn thousand_row_daily_reports_decode_with_counts_and_costs_preserved() {
    let json=serde_json::to_vec(&serde_json::json!({"daily":(0..1000).map(|i|serde_json::json!({"date":format!("2026-06-{}",i%28+1),"inputTokens":i,"outputTokens":1,"cacheCreationTokens":2,"cacheReadTokens":3,"totalTokens":i,"totalCost":0.1})).collect::<Vec<_>>()})).unwrap();
    let started = std::time::Instant::now();
    for _ in 0..20 {
        let report = ExternalReport::parse(&json).unwrap();
        assert_eq!(report.daily.len(), 1000);
        assert_eq!(report.daily[999].total_tokens, 999);
        assert_eq!(report.daily[999].bucket.input, 999);
        assert_eq!(report.daily[999].bucket.cache_read, 3);
        assert_eq!(report.daily[999].bucket.cost, 0.1);
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    eprintln!(
        "1000-row native usage report decoder:20passes in {:?}",
        started.elapsed()
    );
}
use serde_json::json;
fn parse(v: serde_json::Value) -> ExternalReport {
    ExternalReport::parse(&serde_json::to_vec(&v).unwrap()).unwrap()
}
// upstream: PokeTokenBarTests.swift::testDailyReport
#[test]
fn daily_fixture() {
    let r = parse(
        json!({"daily":[{"date":"2026-06-10","inputTokens":907436,"outputTokens":1334526,"cacheCreationTokens":18905002,"cacheReadTokens":169465976,"totalTokens":190612940,"totalCost":311.30462895,"modelsUsed":["claude-fable-5"],"modelBreakdowns":[]}],"totals":null}),
    );
    assert_eq!(r.daily.len(), 1);
    assert_eq!(r.daily[0].total_tokens, 190612940);
    assert_eq!(r.daily[0].period, "2026-06-10")
}
// upstream: PokeTokenBarTests.swift::testDailyReportV20Period
#[test]
fn daily_v20_period() {
    let r = parse(
        json!({"daily":[{"agent":"all","period":"2026-06-15","metadata":{"agents":["claude"]},"inputTokens":372486,"outputTokens":167660,"cacheCreationTokens":2050363,"cacheReadTokens":18225182,"totalTokens":20815691,"totalCost":27.98}],"totals":null}),
    );
    assert_eq!(r.daily[0].period, "2026-06-15");
    assert_eq!(r.daily[0].total_tokens, 20815691)
}
// upstream: PokeTokenBarTests.swift::testCodexDailyReportMapsCachedInputTokens
#[test]
fn codex_daily_aliases() {
    let r = parse(
        json!({"daily":[{"date":"2026-06-17","inputTokens":10,"outputTokens":20,"cachedInputTokens":30,"totalTokens":60,"costUSD":0.25}]}),
    );
    assert_eq!(r.daily[0].bucket.cache_read, 30);
    assert_eq!(r.daily[0].bucket.cost, 0.25)
}
// upstream: PokeTokenBarTests.swift::testDailyReportEmpty
#[test]
fn empty_daily() {
    assert!(parse(json!({"daily":[],"totals":null})).daily.is_empty())
}
// upstream: PokeTokenBarTests.swift::testTotalTokensFallback
#[test]
fn total_fallback() {
    let r = parse(
        json!({"daily":[{"date":"2026-06-10","inputTokens":10,"outputTokens":20,"cacheCreationTokens":30,"cacheReadTokens":40,"costUSD":1.5}]}),
    );
    assert_eq!(r.daily[0].total_tokens, 100);
    assert_eq!(r.daily[0].bucket.cost, 1.5)
}
// upstream: PokeTokenBarTests.swift::testBlocksReport
#[test]
fn blocks_fixture() {
    let r = parse(
        json!({"blocks":[{"id":"2026-06-10T06:00:00.000Z","startTime":"2026-06-10T06:00:00.000Z","endTime":"2026-06-10T11:00:00.000Z","isActive":true,"isGap":false,"entries":399,"totalTokens":26910731,"costUSD":51.8358331}]}),
    );
    assert_eq!(r.blocks.len(), 1);
    assert!(r.blocks[0].active);
    assert!(r.blocks[0].end.is_some());
    assert_eq!(r.blocks[0].total_tokens, 26910731)
}
// upstream: PokeTokenBarTests.swift::testWeeklyMonthlyReport
#[test]
fn weekly_monthly_fixture() {
    let r = parse(
        json!({"weekly":[{"week":"2026-05-31","inputTokens":79280,"outputTokens":634270,"cacheCreationTokens":4355252,"cacheReadTokens":141260644,"totalTokens":146329446,"totalCost":107.0425086}],"monthly":[{"month":"2026-06","totalTokens":671185849,"totalCost":589.255}]}),
    );
    assert_eq!(r.weekly[0].period, "2026-05-31");
    assert_eq!(r.weekly[0].total_tokens, 146329446);
    assert_eq!(r.monthly[0].period, "2026-06");
    assert_eq!(r.monthly[0].total_tokens, 671185849)
}
// upstream: PokeTokenBarTests.swift::testCodexMonthlyReportCostUSD
#[test]
fn codex_monthly_cost_alias() {
    let r = parse(
        json!({"monthly":[{"month":"2026-06","inputTokens":10,"outputTokens":20,"cachedInputTokens":30,"totalTokens":60,"costUSD":0.25}]}),
    );
    assert_eq!(r.monthly[0].total_tokens, 60);
    assert_eq!(r.monthly[0].bucket.cost, 0.25)
}
// upstream: PokeTokenBarTests.swift::testPeriodUsageSumsDailyRows
#[test]
fn period_sums_disjoint_rows() {
    let a = Bucket {
        input: 1,
        output: 2,
        cache_write: 3,
        cache_read: 4,
        cost: 0.1,
        ..Default::default()
    };
    let b = Bucket {
        input: 5,
        output: 6,
        cache_write: 7,
        cache_read: 8,
        cost: 0.2,
        ..Default::default()
    };
    let mut period = a;
    period.merge(&b);
    assert_eq!(period.total(), 36);
    assert!((period.cost - 0.3).abs() < 1e-9)
}
// upstream: PokeTokenBarTests.swift::testBlockBurnRateDecoding
#[test]
fn nested_burn_rate() {
    let r = parse(
        json!({"blocks":[{"id":"b","startTime":"2026-06-11T01:00:00.000Z","endTime":"2026-06-11T06:00:00.000Z","isActive":true,"totalTokens":26910731,"costUSD":51.8,"burnRate":{"tokensPerMinute":457194.05,"costPerHour":1.18}}]}),
    );
    assert_eq!(r.blocks[0].tokens_per_minute, Some(457194.05))
}
// upstream: PokeTokenBarTests.swift::testForecastDepletion
#[test]
fn forecast_guardrails() {
    let now = 100000.;
    assert_eq!(
        forecast_depletion(23000000, 1000000., 23., now),
        Some(now + 77. * 60.)
    );
    assert_eq!(forecast_depletion(1000000, 1000000., 3., now), None);
    assert_eq!(forecast_depletion(23000000, 5000., 23., now), None);
    assert_eq!(forecast_depletion(100000000, 10000., 10., now), None);
    assert_eq!(forecast_depletion(23000000, f64::INFINITY, 23., now), None)
}
#[test]
fn external_provider_uses_generic_report() {
    let r = parse(
        json!({"daily":[{"date":"2026-06-17","inputTokens":10,"outputTokens":20,"cachedInputTokens":30,"costUSD":0.25}]}),
    );
    let now = crate::usage::timestamp(&json!("2026-06-17T12:00:00Z")).unwrap();
    let r = r.into_report("future-tool", now);
    assert_eq!(r.today(), 60);
    assert_eq!(r.totals.cost, 0.25);
    assert!(r.entries[0].id.starts_with("future-tool|"))
}
