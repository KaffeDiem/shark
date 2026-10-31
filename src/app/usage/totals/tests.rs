use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::logs::{Entry, Report};
use crate::usage::providers::{Registry, UsageProvider};
use chrono::{Local, NaiveDate, TimeZone};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
fn at(day: u32) -> f64 {
    Local
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(2026, 9, day)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        )
        .earliest()
        .unwrap()
        .timestamp() as f64
}
fn history(unavailable: bool) -> Report {
    Report::from_entries(
        [(17, 4_800_000, 31.64), (1, 13_100_000, 89.66)]
            .into_iter()
            .map(|(day, input, cost)| Entry {
                id: format!("fixture-{day}"),
                date: at(day),
                input,
                model: "fixture".into(),
                explicit_cost: (!unavailable).then_some(cost),
                cost_unavailable: unavailable.then_some(true),
                ..Default::default()
            })
            .collect(),
        at(18),
    )
}
fn open(s: &mut Shark, w: &mut Window, cx: &mut Context<Shark>, report: Report) {
    s.usage_reports[0] = report;
    s.usage_reports[1] = Report::default();
    s.show_usage(w, cx);
    s.usage_panel.as_mut().unwrap().reference_time = Some(at(18));
    cx.notify();
}
// Adaptation: Shark's native Usage view owns historical accounting; Pokémon Home remains the companion view.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn quiet_today_keeps_month_history_and_zero_today_visible(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx, history(false)));
        w.render_frame(cx);
        assert_eq!(shark.read(cx).usage_summary(at(18)).today.total(), 0);
        assert!(
            w.find("usage-today-total")
                .label()
                .unwrap()
                .contains("0 tokens")
        );
        assert!(w.find("usage-trend").visible());
        assert!(w.find("usage-period-week").visible());
        assert!(w.find("usage-period-month").visible());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn empty_history_keeps_zero_period_summaries_without_inventing_graph(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            open(s, w, cx, Report::from_entries(vec![], at(18)))
        });
        w.render_frame(cx);
        assert!(
            w.find("usage-today-total")
                .label()
                .unwrap()
                .contains("0 tokens")
        );
        assert_eq!(w.find("usage-period-week").label(), Some("This week: 0"));
        assert_eq!(w.find("usage-period-month").label(), Some("This month: 0"));
        assert!(w.try_find("usage-trend").is_none());
    })
    .unwrap();
}
// Adaptation: native host interface is English; exact known currencies stay atomic on their period row.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn known_period_costs_remain_complete_on_single_native_lines(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx, history(false)));
        w.render_frame(cx);
        for (id, amount) in [
            ("usage-period-week", "$31.64"),
            ("usage-period-month", "$121.30"),
        ] {
            let row = w.find(id);
            assert!(row.visible());
            assert!(row.label().unwrap().contains(amount));
            assert!(row.bounds().size.height <= px(20.));
            assert!(row.bounds().right() <= w.find("usage-panel").bounds().right());
        }
        assert!(w.find("usage-open-recap").visible());
    })
    .unwrap();
}
// Adaptation: Shark's host language is retained; German unavailable wording is covered by the shared formatter contract.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn unknown_period_costs_remain_visible_without_expanding_native_popover(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx, history(true)));
        w.render_frame(cx);
        for id in ["usage-period-week", "usage-period-month"] {
            let row = w.find(id);
            assert!(row.label().unwrap().contains("Unavailable"));
            assert!(row.bounds().size.height <= px(20.));
            assert!(row.bounds().right() <= w.find("usage-panel").bounds().right());
        }
    })
    .unwrap();
}
struct FlatTool;
impl UsageProvider for FlatTool {
    fn id(&self) -> &str {
        "flat"
    }
    fn name(&self) -> &str {
        "Future flat-rate tool"
    }
    fn reports_cost(&self) -> bool {
        false
    }
    fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure> {
        Err(Failure::Unsupported)
    }
    fn activity(&self, _: f64) -> Daily {
        Daily::default()
    }
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_summary_and_chart_omit_money_for_non_cost_sources(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let mut registry = Registry::default();
        registry.register(FlatTool).unwrap();
        let registry = Box::leak(Box::new(registry));
        shark.update(cx, |s, cx| {
            s.usage_providers = registry.handles().collect();
            s.usage.truncate(1);
            s.usage_activity.truncate(1);
            s.usage_queries.truncate(1);
            s.pokemon_ui.scanning.truncate(1);
            s.usage_reports = vec![history(false)];
            s.show_usage(w, cx);
            s.usage_panel.as_mut().unwrap().reference_time = Some(at(18));
            cx.notify();
        });
        w.render_frame(cx);
        for id in [
            "usage-today-total",
            "usage-period-week",
            "usage-period-month",
            "usage-trend-readout",
        ] {
            assert!(!w.find(id).label().unwrap().contains('$'));
        }
        assert!(w.try_find("usage-cost-flat").is_none());
    })
    .unwrap();
}
