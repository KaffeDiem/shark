use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
fn quota(label: &str, remaining: f64, span: f64) -> Allowance {
    Allowance {
        label: label.into(),
        remaining: Some(remaining),
        resets_at: None,
        window: Some(span),
    }
}
#[test]
fn provider_ids_remain_unique_and_stable() {
    let ids: Vec<_> = Provider::ALL.iter().map(|p| p.id()).collect();
    assert_eq!(ids, ["claude", "codex"]);
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_snapshots_distinguish_no_result_fresh_and_failed_refresh(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            assert!(s.usage[0].fetched_at.is_none());
            assert!(s.usage[0].checked_at.is_none());
            s.complete_usage_query(
                Provider::Claude,
                Ok(vec![quota("5 hours", 50., 18000.)]),
                cx,
            );
            assert!(s.usage[0].fetched_at.is_some());
            assert!(!s.usage[0].outdated);
            s.complete_usage_query(Provider::Claude, Err(Failure::Temporary), cx);
            assert!(s.usage[0].outdated);
            assert_eq!(s.usage[0].allowances[0].remaining, Some(50.));
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repeated_refreshes_share_one_query_per_provider(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            let before = s.usage_queries.clone();
            for _ in 0..100 {
                s.refresh_usage(cx);
            }
            assert_eq!(s.usage_queries, before);
            s.complete_usage_query(
                Provider::Claude,
                Ok(vec![quota("5 hours", 50., 18000.)]),
                cx,
            );
            for _ in 0..100 {
                s.refresh_usage(cx);
            }
            assert_eq!(s.usage_queries[0], before[0] + 1);
            assert_eq!(s.usage_queries[1], before[1]);
            assert_eq!(s.usage[0].status, Status::Loading);
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_remaining_gauges_keep_official_fraction_and_clamp_fill(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        for remaining in [60., 100., 0., 59.5] {
            shark.update(cx, |s, cx| {
                s.usage[0].finish(Ok(vec![quota("5 hours", remaining, 18000.)]));
                s.show_usage(w, cx);
            });
            w.render_frame(cx);
            let bar = w.find("usage-bar-claude-0").bounds();
            let fill = w.find("usage-fill-claude-0").bounds();
            assert!((fill.size.width / bar.size.width - remaining as f32 / 100.).abs() < 0.005);
            assert!(
                w.find("usage-bar-claude-0")
                    .label()
                    .unwrap()
                    .contains("remaining")
            );
        }
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pace_tick_renders_inside_the_bar_only_when_pace_is_known(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let mut row = quota("5 hours", 50., 18000.);
            row.resets_at = Some(crate::usage::now() + 9000.);
            assert!(row.pace(crate::usage::now()).is_some());
            s.usage[0].finish(Ok(vec![row, quota("Weekly", 50., 604800.)]));
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        let track = w.find("usage-bar-claude-0").bounds();
        let marker = w.find("usage-marker-claude-0").bounds();
        assert_eq!(track.size.height, px(4.));
        assert_eq!(marker.size.height, track.size.height + px(4.));
        assert_eq!(track.top() - marker.top(), px(2.));
        assert!(marker.left() >= track.left() && marker.right() <= track.right());
        assert!(w.try_find("usage-marker-claude-1").is_none());
    })
    .unwrap();
}
