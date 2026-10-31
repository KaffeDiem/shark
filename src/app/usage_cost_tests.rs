use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::logs::{CostCoverage, Entry, Report};
use crate::usage::providers::Observation;
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
fn entry(model: &str, now: f64) -> Entry {
    Entry {
        id: model.into(),
        date: now,
        model: model.into(),
        input: 1000,
        output: 100,
        cache_read: 500,
        ..Default::default()
    }
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cost_coverage_survives_store_and_native_usage(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::now();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            for (provider, model) in [
                (Provider::Claude, "gpt-5.5"),
                (Provider::Codex, "new-model"),
            ] {
                let report = Report::from_entries(vec![entry(model, now)], now);
                s.complete_usage_activity(
                    provider.into(),
                    s.pokemon_ui.generation,
                    Observation {
                        activity: report.daily.clone(),
                        companion: report,
                    },
                    cx,
                )
            }
            let combined = crate::usage::summary::combine_series(
                s.usage_reports.iter().map(|r| r.month_daily(now)),
            );
            let today = combined.last().unwrap();
            assert_eq!(today.bucket.total(), 3200);
            assert_eq!(
                today.bucket.coverage,
                CostCoverage {
                    estimated: true,
                    unknown: true,
                    reported: false
                }
            );
            assert_eq!(
                s.state
                    .usage_ledger
                    .tokens(crate::usage::summary::local_date(now)),
                3200
            );
            s.show_usage(w, cx);
            s.usage_panel.as_mut().unwrap().details = true
        });
        w.render_frame(cx);
        let priced = w.find("usage-cost-claude");
        assert!(priced.visible());
        assert!(priced.label().unwrap().contains("$0.01"));
        assert!(!priced.label().unwrap().contains('≈'));
        assert!(!priced.label().unwrap().contains('+'));
        assert!(
            w.find("usage-cost-codex")
                .label()
                .unwrap()
                .contains("Unavailable")
        );
    })
    .unwrap();
}
// Shark retains its interface language; pure cost localization tests cover all upstream languages.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_cost_states_have_accessible_labels_and_layout(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::now();
    for (reported, estimated, unknown, label) in [
        (false, true, false, "includes estimates"),
        (true, false, false, "reported"),
        (false, false, true, "Unavailable"),
        (false, true, true, "partial coverage"),
    ] {
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                let mut report = Report::from_entries(vec![entry("gpt-5.5", now)], now);
                report.totals.cost = if reported { 0. } else { 12.5 };
                report.totals.coverage = CostCoverage {
                    reported,
                    estimated,
                    unknown,
                };
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    s.pokemon_ui.generation,
                    Observation {
                        activity: report.daily.clone(),
                        companion: report,
                    },
                    cx,
                );
                s.show_usage(w, cx);
                s.usage_panel.as_mut().unwrap().details = true
            });
            w.render_frame(cx);
            let node = w.find("usage-cost-claude");
            assert!(node.visible());
            assert!(node.label().unwrap().contains(label));
            assert!(node.bounds().size.width > px(100.));
            assert!(node.bounds().size.height > px(8.));
        })
        .unwrap();
    }
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_local_usage_scan_preserves_prior_report_and_recovers(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::now();
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let report = Report::from_entries(
                vec![Entry {
                    input: 100000000,
                    output: 0,
                    cache_read: 0,
                    ..entry("gpt-5.5", now)
                }],
                now,
            );
            s.complete_usage_activity(
                Provider::Claude.into(),
                s.pokemon_ui.generation,
                Observation {
                    activity: report.daily.clone(),
                    companion: report,
                },
                cx,
            );
            let ledger = s.state.usage_ledger.clone();
            let failed = Report {
                error: Some("fixture read failure".into()),
                ..Default::default()
            };
            s.complete_usage_activity(
                Provider::Claude.into(),
                s.pokemon_ui.generation,
                Observation {
                    activity: Daily::default(),
                    companion: failed,
                },
                cx,
            );
            assert_eq!(s.usage_reports[0].today(), 100000000);
            assert!(s.usage_reports[0].error.is_some());
            assert_eq!(s.state.usage_ledger, ledger);
            s.show_usage(w, cx)
        });
        w.render_frame(cx);
        assert!(w.find("usage-scan-error-claude").visible());
        shark.update(cx, |s, cx| {
            let report = Report::from_entries(vec![entry("gpt-5.5", now)], now);
            s.complete_usage_activity(
                Provider::Claude.into(),
                s.pokemon_ui.generation,
                Observation {
                    activity: report.daily.clone(),
                    companion: report,
                },
                cx,
            );
            assert!(s.usage_reports[0].error.is_none())
        });
        w.render_frame(cx);
        assert!(w.try_find("usage-scan-error-claude").is_none());
    })
    .unwrap();
}
fn apply_report(s: &mut Shark, provider: Provider, report: Report, cx: &mut Context<Shark>) {
    s.complete_usage_activity(
        provider.into(),
        s.pokemon_ui.generation,
        Observation {
            activity: report.daily.clone(),
            companion: report,
        },
        cx,
    )
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stale_source_report_cannot_pollute_another_sources_ledger_refresh(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::now();
    let report = |input| {
        Report::from_entries(
            vec![Entry {
                input,
                output: 0,
                cache_read: 0,
                ..entry("gpt-5.5", now)
            }],
            now,
        )
    };
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            apply_report(s, Provider::Claude, report(100), cx);
            apply_report(s, Provider::Codex, report(100), cx);
            let old = s.pokemon_ui.generation;
            s.set_usage_roots("claude", "/tmp/shark-recap-stale-fixture", cx);
            s.complete_usage_activity(
                Provider::Claude.into(),
                old,
                Observation {
                    activity: report(10000).daily,
                    companion: report(10000),
                },
                cx,
            );
            assert_eq!(s.usage_reports[0].today(), 100);
            apply_report(s, Provider::Codex, report(200), cx);
            assert_eq!(
                s.state
                    .usage_ledger
                    .tokens(crate::usage::summary::local_date(now)),
                300
            );
            s.set_usage_roots("claude", "", cx);
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn provider_identity_survives_empty_and_recovered_reports(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::now();
    let report = |tokens| {
        Report::from_entries(
            vec![Entry {
                input: tokens,
                output: 0,
                cache_read: 0,
                ..entry("gpt-5.5", now)
            }],
            now,
        )
    };
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            apply_report(s, Provider::Claude, report(1000), cx);
            apply_report(s, Provider::Codex, report(500), cx);
            assert_eq!(
                s.usage_summary(now).today_by_provider,
                std::collections::BTreeMap::from([("claude".into(), 1000), ("codex".into(), 500)])
            );
            apply_report(s, Provider::Claude, report(1200), cx);
            apply_report(s, Provider::Codex, Report::from_entries(vec![], now), cx);
            let summary = s.usage_summary(now);
            assert_eq!(
                summary.today_by_provider,
                std::collections::BTreeMap::from([("claude".into(), 1200)])
            );
            assert_eq!(summary.providers.len(), 1);
            apply_report(s, Provider::Codex, report(700), cx);
            assert_eq!(
                s.usage_summary(now).today_by_provider,
                std::collections::BTreeMap::from([("claude".into(), 1200), ("codex".into(), 700)])
            );
        })
    })
    .unwrap();
}
// Rust publishes one atomic report; a failed refresh retains its complete period data.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_atomic_report_retains_week_and_month(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::timestamp(&serde_json::json!("2026-09-18T12:00:00Z")).unwrap();
    let report = Report::from_entries(
        [
            (now, 1000),
            (now - 86400., 6000),
            (now - 10. * 86400., 23000),
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
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            apply_report(s, Provider::Claude, report, cx);
            let before = s.usage_summary(now);
            assert_eq!(before.week.total(), 7000);
            assert_eq!(before.month.total(), 30000);
            apply_report(
                s,
                Provider::Claude,
                Report {
                    error: Some("period scan unavailable".into()),
                    ..Default::default()
                },
                cx,
            );
            let after = s.usage_summary(now);
            assert_eq!(after.week.total(), 7000);
            assert_eq!(after.month.total(), 30000);
            assert_eq!(after.month_daily, before.month_daily);
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_idle_history_retained_but_successful_empty_clears_snapshot(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    let now = crate::usage::now();
    let day = crate::usage::summary::local_date(now);
    let observed = crate::usage::activity::Daily {
        first_day: activity::local_day(now) - 1,
        tokens: vec![12000, 0],
    };
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            for _ in 0..2 {
                apply_report(s, Provider::Codex, Report::from_daily(observed.clone()), cx);
                let summary = s.usage_summary(now);
                assert_eq!(summary.today.total(), 0);
                assert!(summary.today_by_provider.is_empty());
                assert!(summary.providers.iter().any(|p| p.id == "codex"));
            }
            let before = s.usage_summary(now);
            let history = s.state.usage_ledger.clone();
            apply_report(
                s,
                Provider::Codex,
                Report {
                    error: Some("fixture unavailable".into()),
                    ..Default::default()
                },
                cx,
            );
            let after = s.usage_summary(now);
            assert_eq!(after.week.total(), before.week.total());
            assert_eq!(after.month.total(), before.month.total());
            assert_eq!(after.month_daily, before.month_daily);
            apply_report(s, Provider::Codex, Report::from_entries(vec![], now), cx);
            assert!(s.usage_summary(now).providers.is_empty());
            assert!(
                s.state.usage_ledger.tokens(day.pred_opt().unwrap())
                    >= history.tokens(day.pred_opt().unwrap())
            );
        })
    })
    .unwrap();
}
