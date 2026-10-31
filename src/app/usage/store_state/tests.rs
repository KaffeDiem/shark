use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::logs::Report;
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
fn today(tokens: u64) -> Report {
    Report::from_daily(Daily {
        first_day: activity::local_day(crate::usage::now()),
        tokens: vec![tokens],
    })
}
// Shark warnings use official quotas regardless of local activity; footer no-activity coverage
// lives in pokemon::footer_usage tests, adapting upstream
// Adaptation: user requested both provider quotas remain visible regardless of local activity.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn observed_headline_warning_is_independent_of_local_activity(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, _| {
            s.usage[0].finish(Ok(vec![quota("5 hours", 4., 18000.)]));
            s.usage[1].finish(Ok(vec![quota("5 hours", 60., 18000.)]));
            s.usage_reports[0] = today(0);
            s.usage_reports[1] = today(500000);
            assert!(s.observed_headline_warning());
            s.usage_reports[0] = today(100000);
            s.usage_reports[0].daily.first_day -= 1;
            assert!(s.observed_headline_warning());
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn monetary_and_model_caps_never_drive_companion_token_warning(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, _| {
            s.usage_reports[1] = today(10000);
            s.usage[1].finish(Ok(vec![
                quota("5 hours", 70., 18000.),
                quota("Individual spend", 1., 604800.),
                quota("Model · Weekly", 0., 604800.),
            ]));
            assert!(!s.observed_headline_warning());
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn observed_critical_warning_uses_official_headline_windows_and_fixed_shark_thresholds(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, _| {
            s.usage[0].finish(Ok(vec![quota("5 hours", 4., 18000.)]));
            assert!(s.observed_headline_warning());
            s.usage[0].finish(Ok(vec![quota("5 hours", 50., 18000.)]));
            assert!(!s.observed_headline_warning());
            s.usage[1].finish(Ok(vec![
                quota("5 hours", 90., 18000.),
                quota("Weekly", 3., 604800.),
            ]));
            assert!(s.observed_headline_warning());
            s.usage[1].finish(Err(Failure::Temporary));
            assert!(
                !s.observed_headline_warning(),
                "outdated data cannot assert a current exhausted quota"
            );
        })
    })
    .unwrap();
}
#[test]
fn scoped_registry_ids_remain_unique_and_stable() {
    let ids: Vec<_> = crate::usage::providers::builtins()
        .handles()
        .map(|p| p.id())
        .collect();
    assert_eq!(ids, ["claude", "codex"]);
    assert_eq!(
        ids.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        ids.len()
    );
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
                Provider::Claude.into(),
                s.pokemon_ui.generation,
                Ok(vec![quota("5 hours", 50., 18000.)]),
                cx,
            );
            assert!(s.usage[0].fetched_at.is_some());
            assert!(!s.usage[0].outdated);
            s.complete_usage_query(
                Provider::Claude.into(),
                s.pokemon_ui.generation,
                Err(Failure::Temporary),
                cx,
            );
            assert!(s.usage[0].outdated);
            assert_eq!(s.usage[0].allowances[0].remaining, Some(50.));
        })
    })
    .unwrap();
}
// Adaptation: preserve Shark's per-provider in-flight query sharing; an invalidated configuration always queues a fresh result.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn repeated_refreshes_share_queries_but_configuration_refresh_is_not_lost(
    cx: &mut TestAppContext,
) {
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
            let generation = s.pokemon_ui.generation;
            s.set_usage_roots("claude", "/tmp/shark-refresh-fixture", cx);
            s.complete_usage_query(
                Provider::Claude.into(),
                generation,
                Ok(vec![quota("5 hours", 50., 18000.)]),
                cx,
            );
            assert_eq!(s.usage_queries[0], before[0] + 1);
            assert_eq!(s.usage[0].status, Status::Loading);
            s.set_usage_roots("claude", "", cx);
        })
    })
    .unwrap();
}
// Adaptation: Shark always displays remaining quota; percentages come from the retained CLI decoder, not a new display-mode setting.
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
async fn each_native_quota_window_uses_its_own_reset_pace(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let now = crate::usage::now();
            let mut session = quota("5 hours", 50., 18000.);
            session.resets_at = Some(now + 18000. * 0.25);
            let mut weekly = quota("Weekly", 50., 604800.);
            weekly.resets_at = Some(now + 604800. * 0.75);
            s.usage[0].finish(Ok(vec![session, weekly]));
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        assert_eq!(
            w.find("usage-marker-claude-0").label(),
            Some("Even pace: 25.0% remaining")
        );
        assert_eq!(
            w.find("usage-marker-claude-1").label(),
            Some("Even pace: 75.0% remaining")
        );
    })
    .unwrap();
}
#[test]
fn marker_geometry_mirrors_elapsed_time_and_clamps_at_both_track_ends() {
    assert!((marker_left(75., 200.) + 2.25 - 150.).abs() < 0.01);
    assert_eq!(marker_left(0., 200.), 0.);
    assert_eq!(marker_left(100., 200.), 195.5);
    assert_eq!(marker_left(-20., 200.), 0.);
    assert_eq!(marker_left(120., 200.), 195.5);
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_pace_marker_only_extends_two_points_above_and_below_track(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let mut row = quota("5 hours", 50., 18000.);
            row.resets_at = Some(crate::usage::now() + 9000.);
            s.usage[0].finish(Ok(vec![row]));
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        let track = w.find("usage-bar-claude-0").bounds();
        let marker = w.find("usage-marker-claude-0").bounds();
        assert_eq!(track.size.height, px(4.));
        assert_eq!(marker.size.height, track.size.height + px(4.));
        assert_eq!(track.top() - marker.top(), px(2.));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn expired_allowances_do_not_raise_the_headline_warning(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, _| {
            let expired = Allowance {
                resets_at: Some(crate::usage::now() - 60.),
                ..quota("5 hours", 2., 18000.)
            };
            s.usage[0].finish(Ok(vec![expired.clone()]));
            assert!(!s.observed_headline_warning());
            s.usage[0].finish(Ok(vec![Allowance {
                resets_at: Some(crate::usage::now() + 600.),
                ..expired
            }]));
            assert!(s.observed_headline_warning());
        })
    })
    .unwrap();
}
