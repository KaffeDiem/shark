use super::combined::*;
use super::*;
fn now() -> f64 {
    Local
        .with_ymd_and_hms(2026, 9, 18, 12, 0, 0)
        .unwrap()
        .timestamp() as f64
}
fn report(rows: &[(f64, u64)]) -> Report {
    Report::from_entries(
        rows.iter()
            .enumerate()
            .map(|(i, (date, tokens))| Entry {
                id: i.to_string(),
                date: *date,
                input: *tokens,
                model: "unknown".into(),
                ..Default::default()
            })
            .collect(),
        now(),
    )
}
fn combine(reports: &[(&str, Report)]) -> Summary {
    Summary::from_sources(
        reports.iter().map(|(id, r)| Source {
            id,
            name: id,
            report: r,
            reports_cost: true,
        }),
        now(),
    )
}
// upstream: UsageStoreTests.swift::testAggregatesTodayTokensAcrossProviders
#[test]
fn today_across_sources() {
    let s = combine(&[
        ("claude", report(&[(now(), 100000000)])),
        ("codex", report(&[(now(), 50000000)])),
    ]);
    assert_eq!(s.today.total(), 150000000);
    assert_eq!(
        s.today_by_provider,
        BTreeMap::from([("claude".into(), 100000000), ("codex".into(), 50000000)])
    );
    assert_eq!(s.today_by_provider.values().sum::<u64>(), s.today.total())
}
// upstream: UsageStoreTests.swift::testCodexOnlyWhenClaudeHasNoData
#[test]
fn absent_claude_keeps_codex() {
    let s = combine(&[
        ("claude", Report::default()),
        ("codex", report(&[(now(), 50000000)])),
    ]);
    assert_eq!(s.today.total(), 50000000);
    assert_eq!(s.providers.len(), 1);
    assert_eq!(s.providers[0].id, "codex");
    assert_eq!(s.today_by_provider.len(), 1)
}
// upstream: UsageStoreTests.swift::testStaleDatedSnapshotExcludedFromTodayTotal
#[test]
fn stale_day_excluded_today() {
    let s = combine(&[
        ("claude", report(&[(now(), 100000000)])),
        ("codex", report(&[(946684800., 999)])),
    ]);
    assert_eq!(s.today.total(), 100000000);
    assert_eq!(
        s.today_by_provider,
        BTreeMap::from([("claude".into(), 100000000)])
    )
}
// upstream: UsageStoreTests.swift::testBurnTierThresholds
#[test]
fn burn_boundaries() {
    for (rate, tier) in [
        (500., BurnTier::Idle),
        (1000., BurnTier::Idle),
        (50000., BurnTier::Normal),
        (200000., BurnTier::Fast),
        (500000., BurnTier::Blazing),
    ] {
        let r = report(&[(now() - 60., rate as u64)]);
        assert_eq!(combine(&[("claude", r)]).burn, tier)
    }
}
// upstream: UsageStoreTests.swift::testBurnTierFromNonClaudeProvider
#[test]
fn codex_burn() {
    assert_eq!(
        combine(&[("codex", report(&[(now() - 60., 200000)]))]).burn,
        BurnTier::Fast
    )
}
// upstream: UsageStoreTests.swift::testBurnTierCombinesProviders
#[test]
fn combined_burn() {
    let s = combine(&[
        ("claude", report(&[(now() - 60., 60000)])),
        ("codex", report(&[(now() - 60., 60000)])),
    ]);
    assert_eq!(s.rate, 120000.);
    assert_eq!(s.burn, BurnTier::Fast)
}
// upstream: UsageStoreTests.swift::testCarrierForWeekMonthOnlyWithoutActiveBlock
#[test]
fn history_visible_without_today_or_burn() {
    let s = combine(&[(
        "codex",
        report(&[(now() - 86400., 50000000), (now() - 10. * 86400., 30000000)]),
    )]);
    assert_eq!(s.providers.len(), 1);
    assert_eq!(s.today.total(), 0);
    assert!(s.today_by_provider.is_empty());
    assert_eq!(s.week.total(), 50000000);
    assert_eq!(s.month.total(), 80000000);
    assert_eq!(s.burn, BurnTier::Idle)
}
// upstream: UsageStoreTests.swift::testNoCarrierForZeroTokenActiveBlock
#[test]
fn zero_synthetic_no_provider() {
    let s = combine(&[("claude", report(&[(now() - 60., 0)]))]);
    assert!(s.providers.is_empty());
    assert_eq!(s.rate, 0.)
}
// upstream: UsageStoreTests.swift::testCarrierForProviderUsedWithinRecentWindowAcrossCalendarBoundaries
#[test]
fn recent_use_crosses_calendar_boundary() {
    let monday = Local
        .with_ymd_and_hms(2026, 6, 1, 12, 0, 0)
        .unwrap()
        .timestamp() as f64;
    let r = Report::from_entries(
        vec![Entry {
            date: monday - 3. * 86400.,
            input: 100,
            ..Default::default()
        }],
        monday,
    );
    let s = Summary::from_sources(
        [Source {
            id: "codex",
            name: "Codex",
            report: &r,
            reports_cost: true,
        }],
        monday,
    );
    assert_eq!(s.providers.len(), 1);
    assert_eq!(s.today.total(), 0);
    assert_eq!(s.week.total(), 0);
    assert_eq!(s.month.total(), 0);
    assert!(s.today_by_provider.is_empty());
    assert_eq!(s.burn, BurnTier::Idle)
}
// upstream: UsageStoreTests.swift::testNoCarrierForProviderIdleBeyondRecentWindow
#[test]
fn older_than_week_without_current_period_hidden() {
    let day = Local
        .with_ymd_and_hms(2026, 6, 1, 12, 0, 0)
        .unwrap()
        .timestamp() as f64;
    let r = Report::from_entries(
        vec![Entry {
            date: day - 7. * 86400. - 3600.,
            input: 100,
            ..Default::default()
        }],
        day,
    );
    assert!(
        Summary::from_sources(
            [Source {
                id: "codex",
                name: "Codex",
                report: &r,
                reports_cost: true
            }],
            day
        )
        .providers
        .is_empty()
    )
}
// upstream: UsageStoreTests.swift::testSnapshotOrderFollowsProviderRegistrationOrder
#[test]
fn provider_order_registration() {
    let s = combine(&[
        ("claude", report(&[(now() - 86400., 10)])),
        ("codex", report(&[(now(), 5000)])),
    ]);
    assert_eq!(
        s.providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["claude", "codex"]
    )
}
// upstream: UsageStoreTests.swift::testUnknownFutureProviderFlowsThroughAllAggregation
#[test]
fn arbitrary_future_source_all_aggregates() {
    let r = report(&[
        (now() - 60., 200000),
        (now() - 6. * 3600., 41800000),
        (now() - 86400., 48000000),
        (now() - 10. * 86400., 210000000),
    ]);
    let s = combine(&[("future_tool_xyz", r)]);
    assert_eq!(s.today.total(), 42000000);
    assert_eq!(s.week.total(), 90000000);
    assert_eq!(s.month.total(), 300000000);
    assert_eq!(s.burn, BurnTier::Fast);
    assert_eq!(
        s.preferring(Some("future_tool_xyz")).unwrap().id,
        "future_tool_xyz"
    )
}
// upstream: UsageStoreTests.swift::testSnapshotPreferringSelection
#[test]
fn preferred_source_or_stable_first() {
    let s = combine(&[
        ("claude", report(&[(now(), 1000)])),
        ("future-tool", report(&[(now(), 2000)])),
    ]);
    assert_eq!(s.preferring(Some("future-tool")).unwrap().id, "future-tool");
    assert_eq!(s.preferring(None).unwrap().id, "claude");
    assert_eq!(s.preferring(Some("missing")).unwrap().id, "claude")
}
// upstream: UsageStoreTests.swift::testMidnightCarrierSnapshotFromActiveBlock
#[test]
fn midnight_preserves_active_usage_and_periods() {
    let now = Local
        .with_ymd_and_hms(2026, 9, 18, 0, 30, 0)
        .unwrap()
        .timestamp() as f64;
    let r = Report::from_entries(
        [
            (now - 3600., 12000000),
            (now - 86400., 78000000),
            (now - 10. * 86400., 210000000),
        ]
        .into_iter()
        .map(|(date, input)| Entry {
            date,
            input,
            ..Default::default()
        })
        .collect(),
        now,
    );
    let s = Summary::from_sources(
        [Source {
            id: "claude",
            name: "Claude",
            report: &r,
            reports_cost: true,
        }],
        now,
    );
    assert_eq!(s.providers.len(), 1);
    assert_eq!(s.preferring(Some("claude")).unwrap().id, "claude");
    assert_eq!(s.today.total(), 0);
    assert_eq!(s.week.total(), 90000000);
    assert_eq!(s.month.total(), 300000000);
    assert_eq!(s.burn, BurnTier::Fast)
}
// upstream: UsageStoreTests.swift::testIdleCarrierRequiresPositiveHistoricalValue
#[test]
fn historical_visibility_requires_positive_usage() {
    for source in ["week", "month", "series", "empty", "failed"] {
        let r = match source {
            "week" => report(&[(now() - 86400., 12000)]),
            "month" => report(&[(now() - 10. * 86400., 12000)]),
            "series" => Report::from_daily(crate::usage::activity::Daily {
                first_day: crate::usage::activity::local_day(now()) - 1,
                tokens: vec![12000, 0],
            }),
            "failed" => Report {
                error: Some("failed".into()),
                ..Default::default()
            },
            _ => report(&[(now() - 86400., 0)]),
        };
        let s = combine(&[("future", r)]);
        assert_eq!(
            s.providers.len(),
            usize::from(!matches!(source, "empty" | "failed")),
            "{source}"
        );
        assert_eq!(s.today.total(), 0)
    }
}
