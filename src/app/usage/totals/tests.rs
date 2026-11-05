use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::logs::{Entry, Report};
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
fn history() -> Report {
    Report::from_entries(
        [(17, 4_800_000), (1, 13_100_000)]
            .into_iter()
            .map(|(day, input)| Entry {
                id: format!("fixture-{day}"),
                date: at(day),
                input,
                model: "fixture".into(),
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
    s.usage_panel.as_mut().unwrap().details = true;
    s.usage_panel.as_mut().unwrap().reference_time = Some(at(18));
    cx.notify();
}
// Adaptation: Shark's native Usage view owns historical accounting.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn quiet_today_keeps_month_history_and_zero_today_visible(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx, history()));
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
        assert_eq!(
            w.find("usage-period-week").label(),
            Some("This week: 0 tokens")
        );
        assert_eq!(
            w.find("usage-period-month").label(),
            Some("This month: 0 tokens")
        );
        assert!(w.try_find("usage-trend").is_none());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn period_tiles_show_tokens_and_share_row_layout(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx, history()));
        w.render_frame(cx);
        for (id, tokens) in [
            ("usage-period-week", 4_800_000),
            ("usage-period-month", 17_900_000),
        ] {
            let row = w.find(id);
            assert!(row.visible());
            assert!(row.label().unwrap().contains(&compact(tokens)));
            assert_eq!(
                row.bounds().top(),
                w.find("usage-today-total").bounds().top()
            );
            assert_eq!(
                row.bounds().size.height,
                w.find("usage-today-total").bounds().size.height
            );
            assert!(row.bounds().right() <= w.find("usage-panel").bounds().right());
        }
        assert!(w.find("usage-open-recap").visible());
    })
    .unwrap();
}
