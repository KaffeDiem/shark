use super::*;
use serde_json::json;
fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}
fn day(date: NaiveDate, tokens: u64) -> Day {
    Day {
        date,
        bucket: Bucket {
            input: tokens,
            ..Default::default()
        },
    }
}
fn ledger(since: NaiveDate, rows: &[(NaiveDate, u64)]) -> UsageLedger {
    let mut l = UsageLedger::default();
    l.merge(&[day(since, 0)]);
    l.merge(&rows.iter().map(|(d, n)| day(*d, *n)).collect::<Vec<_>>());
    l
}
fn now() -> NaiveDate {
    date(2026, 9, 18)
}
fn recap(l: &UsageLedger, scope: Scope, offset: i32) -> Recap {
    Recap::make(
        l,
        &[],
        Period::containing(scope, now(), 0).shifted(offset),
        now(),
    )
}
#[test]
fn day_highwater() {
    let mut l = ledger(
        date(2026, 9, 10),
        &[(date(2026, 9, 10), 500), (date(2026, 9, 11), 300)],
    );
    l.merge(&[day(date(2026, 9, 10), 120), day(date(2026, 9, 11), 900)]);
    assert_eq!(l.tokens(date(2026, 9, 10)), 500);
    assert_eq!(l.tokens(date(2026, 9, 11)), 900)
}
#[test]
fn coverage_includes_zero_days() {
    let mut l = ledger(date(2026, 9, 1), &[(date(2026, 9, 2), 40)]);
    assert_eq!(l.tokens_by_day.len(), 1);
    assert_eq!(l.covered_since.as_deref(), Some("2026-09-01"));
    assert!(l.covers(date(2026, 9, 1)));
    assert!(!l.covers(date(2026, 8, 31)));
    l.merge(&[day(date(2026, 9, 5), 10)]);
    assert_eq!(l.covered_since.as_deref(), Some("2026-09-01"))
}
#[test]
fn pruning_forgets_coverage() {
    let mut l = ledger(
        date(2025, 6, 1),
        &[(date(2025, 6, 10), 100), (date(2026, 2, 1), 200)],
    );
    l.prune(date(2026, 1, 1));
    assert_eq!(l.tokens_by_day.len(), 1);
    assert_eq!(l.tokens(date(2026, 2, 1)), 200);
    assert_eq!(l.covered_since.as_deref(), Some("2026-01-01"))
}
#[test]
fn junk_ledger_dropped() {
    let l:UsageLedger=serde_json::from_value(json!({"tokens_by_day":{"2026-09-10":400,"nope":10,"0000-00-00":10,"2026-09-11":0,"2026-09-12":-20},"covered_since":"0000-00-00"})).unwrap();
    assert_eq!(l.tokens_by_day.len(), 1);
    assert_eq!(l.tokens(date(2026, 9, 10)), 400);
    assert_eq!(l.covered_since, None)
}
#[test]
fn ledger_roundtrip() {
    let l = ledger(date(2026, 9, 1), &[(date(2026, 9, 10), 700)]);
    assert_eq!(
        serde_json::from_slice::<UsageLedger>(&serde_json::to_vec(&l).unwrap()).unwrap(),
        l
    )
}
#[test]
fn regional_week_start() {
    let mon = Period::containing(Scope::Week, now(), 0);
    assert_eq!(mon.start, date(2026, 9, 14));
    assert_eq!(mon.end, date(2026, 9, 21));
    assert_eq!(
        Period::containing(Scope::Week, now(), 6).start,
        date(2026, 9, 13)
    )
}
#[test]
fn shift_whole_periods() {
    let feb = Period::containing(Scope::Month, date(2026, 3, 31), 0).shifted(-1);
    assert_eq!((feb.start, feb.end), (date(2026, 2, 1), date(2026, 3, 1)));
    let year = Period::containing(Scope::Year, now(), 0).shifted(-1);
    assert_eq!((year.start, year.end), (date(2025, 1, 1), date(2026, 1, 1)))
}
#[test]
fn scope_meters() {
    let l = ledger(
        date(2026, 1, 1),
        &[
            (date(2026, 9, 2), 10),
            (date(2026, 9, 17), 20),
            (date(2026, 8, 30), 5),
        ],
    );
    let week = recap(&l, Scope::Week, 0);
    assert_eq!(week.buckets[0].date, date(2026, 9, 14));
    assert_eq!(week.buckets.len(), 7);
    assert_eq!(
        week.buckets
            .iter()
            .filter(|b| b.current)
            .map(|b| b.date)
            .collect::<Vec<_>>(),
        [now()]
    );
    assert_eq!(recap(&l, Scope::Month, 0).buckets.len(), 30);
    assert_eq!(recap(&l, Scope::Month, -7).buckets.len(), 28);
    let year = recap(&l, Scope::Year, 0);
    assert_eq!(year.buckets.len(), 12);
    assert_eq!(year.buckets[8].tokens, 30);
    assert_eq!(year.buckets[7].tokens, 5);
    assert!(year.buckets[8].current);
    assert_eq!(year.total, 35)
}
#[test]
fn missing_not_zero() {
    let l = ledger(date(2026, 9, 16), &[(date(2026, 9, 17), 50)]);
    let week = recap(&l, Scope::Week, 0);
    assert_eq!(
        week.buckets.iter().map(|b| b.has_data).collect::<Vec<_>>(),
        [false, false, true, true, true, false, false]
    );
    assert_eq!(week.active_days, 1);
    assert_eq!(week.counted_days, 3);
    let year = recap(&ledger(date(2026, 9, 1), &[]), Scope::Year, 0);
    assert_eq!(
        year.buckets.iter().map(|b| b.has_data).collect::<Vec<_>>(),
        [
            false, false, false, false, false, false, false, false, true, false, false, false
        ]
    )
}
#[test]
fn running_comparison_elapsed_days() {
    let l = ledger(
        date(2026, 8, 1),
        &[
            (date(2026, 9, 10), 100),
            (date(2026, 8, 18), 40),
            (date(2026, 8, 19), 1000),
        ],
    );
    let month = recap(&l, Scope::Month, 0);
    assert!(month.in_progress);
    assert_eq!(month.previous_total, Some(40));
    assert_eq!(month.delta(), Some(1.5))
}
#[test]
fn finished_comparison_whole() {
    let l = ledger(
        date(2026, 7, 1),
        &[(date(2026, 8, 5), 300), (date(2026, 7, 31), 100)],
    );
    let august = recap(&l, Scope::Month, -1);
    assert!(!august.in_progress);
    assert_eq!(august.previous_total, Some(100));
    assert_eq!(august.delta(), Some(2.))
}
#[test]
fn partial_previous_no_comparison() {
    let l = ledger(
        date(2026, 9, 9),
        &[(date(2026, 9, 15), 150), (date(2026, 9, 10), 50)],
    );
    let week = recap(&l, Scope::Week, 0);
    assert_eq!(week.previous_total, None);
    assert_eq!(week.delta(), None)
}
#[test]
fn streak_stays_in_period() {
    let l = ledger(
        date(2026, 9, 1),
        &[11, 12, 13, 14, 15, 16, 18].map(|d| (date(2026, 9, d), 5)),
    );
    assert_eq!(recap(&l, Scope::Week, 0).best_streak, 3);
    assert_eq!(recap(&l, Scope::Month, 0).best_streak, 6)
}
#[test]
fn graduations_exclude_released_outside() {
    let graduates = [
        Graduation {
            id: "this-week".into(),
            date: date(2026, 9, 17),
            released: false,
        },
        Graduation {
            id: "monday".into(),
            date: date(2026, 9, 14),
            released: false,
        },
        Graduation {
            id: "let-go".into(),
            date: date(2026, 9, 16),
            released: true,
        },
        Graduation {
            id: "last-week".into(),
            date: date(2026, 9, 13),
            released: false,
        },
    ];
    let l = ledger(date(2026, 9, 1), &[]);
    let week = Recap::make(
        &l,
        &graduates,
        Period::containing(Scope::Week, now(), 0),
        now(),
    );
    assert_eq!(
        week.graduated
            .iter()
            .map(|g| g.id.as_str())
            .collect::<Vec<_>>(),
        ["this-week", "monday"]
    );
    assert_eq!(
        Recap::make(
            &l,
            &graduates,
            Period::containing(Scope::Month, now(), 0),
            now()
        )
        .graduated
        .len(),
        3
    )
}
#[test]
fn navigation_stops_before_coverage() {
    let l = ledger(date(2026, 9, 1), &[(date(2026, 9, 2), 10)]);
    assert!(!recap(&l, Scope::Month, 0).can_go_back);
    assert!(recap(&l, Scope::Week, -1).can_go_back);
    assert!(!recap(&l, Scope::Week, -2).can_go_back);
    assert!(!recap(&l, Scope::Year, 0).can_go_back)
}
#[test]
fn empty_ledger_has_unknown_meters() {
    let week = recap(&UsageLedger::default(), Scope::Week, 0);
    assert_eq!(week.buckets.len(), 7);
    assert!(!week.buckets.iter().any(|b| b.has_data));
    assert_eq!(week.total, 0);
    assert_eq!(week.best_day, None);
    assert_eq!(week.previous_total, None);
    assert!(!week.can_go_back)
}
fn entry(day: NaiveDate, output: u64) -> Entry {
    Entry {
        id: day.to_string(),
        date: Local
            .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
            .unwrap()
            .timestamp() as f64,
        model: "claude-opus-4-8".into(),
        output,
        ..Default::default()
    }
}
fn report(entries: Vec<Entry>, now: NaiveDate) -> Report {
    Report::from_entries(entries, entry(now, 0).date)
}
#[test]
fn month_series_matches_scalar() {
    let now = date(2026, 7, 10);
    let r = report(
        vec![
            entry(date(2026, 6, 29), 700),
            entry(date(2026, 7, 3), 100),
            entry(now, 40),
        ],
        now,
    );
    let series = r.month_daily(entry(now, 0).date);
    assert!(series.iter().all(|d| d.date.month() == 7));
    let month = r.period(date(2026, 7, 1), date(2026, 7, 11));
    assert_eq!(
        series.iter().map(|d| d.bucket.total()).sum::<u64>(),
        month.total()
    );
    let cost = series.iter().map(|d| d.bucket.cost).sum::<f64>();
    assert!((cost - month.cost).abs() < 1e-9)
}
#[test]
fn last_usage_excludes_synthetic() {
    let real = entry(date(2026, 7, 6), 300);
    let synthetic = entry(date(2026, 7, 9), 0);
    assert_eq!(
        report(vec![real.clone(), synthetic.clone()], date(2026, 7, 10)).last_usage(),
        Some(real.date)
    );
    assert_eq!(
        report(vec![synthetic], date(2026, 7, 10)).last_usage(),
        None
    )
}
#[test]
fn monthly_empty_days_explicit() {
    let now = date(2026, 7, 10);
    let series = report(vec![entry(date(2026, 7, 3), 500)], now).month_daily(entry(now, 0).date);
    assert_eq!(
        series.iter().map(|d| d.date).collect::<Vec<_>>(),
        (1..=10).map(|d| date(2026, 7, d)).collect::<Vec<_>>()
    );
    assert_eq!(
        series
            .iter()
            .filter(|d| d.bucket.total() > 0)
            .map(|d| d.date)
            .collect::<Vec<_>>(),
        [date(2026, 7, 3)]
    )
}
#[test]
fn month_series_stops_today() {
    let now = date(2026, 7, 10);
    let series = report(vec![], now).month_daily(entry(now, 0).date);
    assert_eq!(series.len(), 10);
    assert_eq!(series.last().unwrap().date, now)
}
#[test]
fn monthly_cost_coverage_matches() {
    let now = date(2026, 7, 10);
    let r = report(vec![entry(date(2026, 7, 5), 5000)], now);
    let series = r.month_daily(entry(now, 0).date);
    let period = r.period(date(2026, 7, 1), date(2026, 7, 11));
    assert_eq!(
        series.iter().map(|d| d.bucket.cost).sum::<f64>(),
        period.cost
    );
    assert!(period.coverage.estimated);
    assert!(
        series
            .iter()
            .find(|d| d.bucket.total() > 0)
            .unwrap()
            .bucket
            .coverage
            .estimated
    )
}
#[test]
fn absent_report_is_not_observed_zero() {
    assert!(
        Report::default()
            .month_daily(entry(now(), 0).date)
            .is_empty()
    );
    let r = Report::from_daily(crate::usage::activity::Daily {
        first_day: now().num_days_from_ce() as i64 - 719163,
        tokens: vec![0],
    });
    let series = r.month_daily(entry(now(), 0).date);
    assert_eq!(series.len(), 1);
    let mut l = UsageLedger::default();
    l.merge(&series);
    assert_eq!(l.covered_since, Some(now().to_string()));
    assert_eq!(l.tokens(now()), 0)
}
#[test]
fn daily_only_future_provider_is_summarized() {
    let r = Report::from_daily(crate::usage::activity::Daily {
        first_day: now().num_days_from_ce() as i64 - 719163 - 1,
        tokens: vec![80, 40],
    });
    let series = r.month_daily(entry(now(), 0).date);
    assert_eq!(
        series.iter().map(|d| d.bucket.total()).collect::<Vec<_>>(),
        [80, 40]
    );
    let b = r.period(now().pred_opt().unwrap(), now().succ_opt().unwrap());
    assert_eq!(b.total(), 120);
    assert!(b.coverage.unknown)
}
#[test]
fn cross_month_file_mtime_scan_preserves_but_filters_entries() {
    use std::{
        fs,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("shark-month-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let june = entry(date(2026, 6, 29), 700);
    let july = entry(date(2026, 7, 8), 300);
    let mut bytes = String::new();
    for e in [&june, &july] {
        let stamp = DateTime::<Utc>::from_timestamp(e.date as i64, 0)
            .unwrap()
            .to_rfc3339();
        bytes.push_str(&json!({"type":"assistant","timestamp":stamp,"requestId":e.id,"message":{"id":e.id,"model":e.model,"usage":{"input_tokens":10,"output_tokens":e.output}}}).to_string());
        bytes.push('\n')
    }
    let path = root.join("straddle.jsonl");
    fs::write(&path, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(july.date as u64))
        .unwrap();
    let now = date(2026, 7, 10);
    let stamp = entry(now, 0).date;
    let mut cache = crate::usage::logs::Cache::default();
    let entries = cache.entries_since(
        crate::usage::Provider::Claude,
        &crate::usage::logs::Roots(vec![root.clone()]),
        0.,
    );
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| local_date(e.date).month() == 6));
    let r = Report::from_entries(entries, stamp);
    let series = r.month_daily(stamp);
    assert_eq!(series.first().unwrap().date, date(2026, 7, 1));
    assert_eq!(series.last().unwrap().date, now);
    assert!(series.iter().all(|d| d.date.month() == 7));
    assert_eq!(
        series
            .iter()
            .find(|d| d.date.day() == 8)
            .unwrap()
            .bucket
            .output,
        300
    );
    assert_eq!(
        series.iter().map(|d| d.bucket.total()).sum::<u64>(),
        r.period(date(2026, 7, 1), date(2026, 7, 11)).total()
    );
    fs::remove_dir_all(root).unwrap()
}

#[test]
fn daily_only_report_totals_have_unknown_price() {
    let r = Report::from_daily(crate::usage::activity::Daily {
        first_day: 1,
        tokens: vec![80, 40],
    });
    assert_eq!(r.totals.total(), 120);
    assert!(r.totals.coverage.unknown);
    assert_eq!(r.totals.cost_text(false), "Unavailable");
}
