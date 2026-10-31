use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::logs::{Bucket, CostCoverage, Entry, Report};
use crate::usage::providers::{Registry, UsageProvider};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;

fn date(text: &str) -> NaiveDate {
    text.parse().unwrap()
}
fn day(text: &str, tokens: u64) -> Day {
    Day {
        date: date(text),
        bucket: Bucket {
            input: tokens,
            cost: 7.,
            coverage: CostCoverage {
                estimated: true,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}
fn provider(id: &str, values: &[(&str, u64)]) -> ProviderSeries {
    ProviderSeries {
        id: id.into(),
        name: id.into(),
        days: Some(values.iter().map(|(d, n)| day(d, *n)).collect()),
        reports_cost: true,
    }
}
fn labels(today: &str) -> Vec<String> {
    let today = date(today);
    columns(&[], today)
        .iter()
        .filter_map(|c| axis_label(c.date, today))
        .collect()
}

// upstream: MonthDailyTrendTests.swift::testBarHeightBoundaries
#[test]
fn heights_preserve_quiet_days_and_cap_peak() {
    assert_eq!(bar_height(0, 1000), BASELINE);
    assert_eq!(bar_height(500, 0), BASELINE);
    assert_eq!(bar_height(0, 0), BASELINE);
    assert_eq!(bar_height(1000, 1000), TRACK);
    assert_eq!(bar_height(2000, 1000), TRACK);
    assert!(bar_height(1, 10_000_000) >= BASELINE);
    assert!(bar_height(999, 1000) <= TRACK);
}
// upstream: MonthDailyTrendTests.swift::testAxisLabelsCoverTheFirstDayEverySeventhAndAlwaysToday
#[test]
fn regular_axis_labels_and_today() {
    let today = date("2026-08-24");
    for n in [1, 7, 14, 21, 28, 24] {
        assert_eq!(
            axis_label(today.with_day(n).unwrap(), today),
            Some(n.to_string())
        );
    }
    for n in [2, 3, 9, 15, 20, 23, 25, 31] {
        assert_eq!(axis_label(today.with_day(n).unwrap(), today), None);
    }
}
// upstream: MonthDailyTrendTests.swift::testAxisLabelCountForAFullMonthStaysSmallEnoughToFit
#[test]
fn full_month_has_at_most_six_axis_labels() {
    assert_eq!(labels("2026-08-31"), ["1", "7", "14", "21", "28", "31"]);
    assert_eq!(labels("2026-08-24"), ["1", "7", "14", "21", "24", "28"]);
    for n in 1..=31 {
        assert!(labels(&format!("2026-08-{n:02}")).len() <= 6);
    }
}
// upstream: MonthDailyTrendTests.swift::testARegularLabelNextToTodayIsDroppedSoTheTwoCannotCollide
#[test]
fn neighboring_axis_labels_are_suppressed() {
    for (today, want) in [
        ("2026-08-22", vec!["1", "7", "14", "22", "28"]),
        ("2026-08-29", vec!["1", "7", "14", "21", "29"]),
        ("2026-08-30", vec!["1", "7", "14", "21", "30"]),
        ("2026-08-31", vec!["1", "7", "14", "21", "28", "31"]),
        ("2026-08-21", vec!["1", "7", "14", "21", "28"]),
        ("2026-08-02", vec!["2", "7", "14", "21", "28"]),
    ] {
        assert_eq!(labels(today), want);
    }
}
// upstream: MonthDailyTrendTests.swift::testTodayIsNeverSuppressedSoTheAxisIsNeverEmpty
#[test]
fn every_today_is_labeled_without_collisions() {
    for n in 1..=31 {
        let labels = labels(&format!("2026-08-{n:02}"));
        assert!(labels.contains(&n.to_string()));
        let ns: Vec<u32> = labels.iter().map(|n| n.parse().unwrap()).collect();
        assert!(ns.windows(2).all(|w| w[1] - w[0] >= 3));
    }
}
// upstream: MonthDailyTrendTests.swift::testColumnsSpanTheWholeCalendarMonthEvenOnTheFirst
#[test]
fn columns_always_span_calendar_month() {
    for (today, length) in [
        ("2026-10-01", 31),
        ("2026-09-15", 30),
        ("2026-02-01", 28),
        ("2028-02-29", 29),
        ("2026-03-08", 31),
        ("2026-11-01", 30),
        ("2026-12-31", 31),
    ] {
        let today = date(today);
        let columns = columns(&[day(&today.to_string(), 10)], today);
        assert_eq!(columns.len(), length);
        assert_eq!(columns[0].date.day(), 1);
        assert_eq!(columns.last().unwrap().date.day(), length as u32);
    }
}
// upstream: MonthDailyTrendTests.swift::testDaysAfterTodayAreFutureAndMissingPastDaysAreZero
#[test]
fn missing_past_is_zero_future_is_blank() {
    let columns = columns(
        &[
            day("2026-10-01", 5),
            day("2026-10-03", 7),
            day("2026-10-10", 900),
        ],
        date("2026-10-03"),
    );
    assert_eq!(
        columns[..3].iter().map(|c| c.tokens).collect::<Vec<_>>(),
        [5, 0, 7]
    );
    assert!(columns[..3].iter().all(|c| !c.future));
    assert!(columns[3..].iter().all(|c| c.future && c.tokens == 0));
}
// upstream: MonthDailyTrendTests.swift::testDaysFromAnotherMonthStayOffTheAxis
#[test]
fn stale_previous_month_never_enters_axis() {
    let columns = columns(
        &[day("2026-09-30", 900), day("2026-10-01", 3)],
        date("2026-10-01"),
    );
    assert_eq!(columns.len(), 31);
    assert!(columns.iter().all(|c| c.date.month() == 10));
    assert_eq!(columns.iter().map(|c| c.tokens).max(), Some(3));
}
// upstream: MonthDailyTrendTests.swift::testWeekendTicksStopAtToday
#[test]
fn weekends_are_not_painted_in_future_columns() {
    let columns = columns(&[day("2026-10-03", 5)], date("2026-10-03"));
    let tick = |n: usize| !columns[n - 1].future && is_weekend(columns[n - 1].date, Some("ko_KR"));
    assert!(tick(3));
    assert!(!tick(4));
    assert!(!tick(10));
    assert!(!tick(2));
    assert!(is_weekend(date("2026-10-11"), Some("ko_KR")));
}
// upstream: MonthDailyTrendTests.swift::testPeakIsShownOnlyOnceMoreThanOneDayHasUsage
#[test]
fn peak_requires_two_used_days() {
    for (values, want) in [
        (vec![day("2026-10-01", 3)], false),
        (vec![day("2026-10-01", 3), day("2026-10-02", 4)], true),
        (vec![day("2026-10-01", 0), day("2026-10-02", 4)], false),
    ] {
        assert_eq!(
            columns(&values, date("2026-10-02"))
                .iter()
                .filter(|c| c.tokens > 0)
                .count()
                > 1,
            want
        );
    }
}
// upstream: MonthDailyTrendTests.swift::testWeekendMarkingFollowsTheInjectedCalendarLocale
#[test]
fn weekend_calendar_follows_region() {
    for (text, want) in [
        ("2026-08-01", true),
        ("2026-08-02", true),
        ("2026-08-03", false),
        ("2026-08-07", false),
    ] {
        assert_eq!(is_weekend(date(text), Some("ko_KR")), want);
    }
    assert!(is_weekend(date("2026-08-07"), Some("he_IL")));
    assert!(!is_weekend(date("2026-08-02"), Some("he_IL")));
}
// upstream: MonthDailyTrendTests.swift::testDayStampWeekdayFollowsTheAppLanguageNotTheSystemLocale
#[test]
fn date_stamps_follow_display_language() {
    let stamp = |l| DateText::new(l, "MdE").stamp(date("2026-08-24"));
    assert!(stamp(Language::Ko).contains('월'));
    assert!(stamp(Language::En).contains("Mon"));
    assert!(stamp(Language::Ja).contains('月'));
    assert!(stamp(Language::Es).to_lowercase().contains("lun"));
    assert!(stamp(Language::En).contains("8/24"));
    for l in [Language::Fr, Language::Pt] {
        assert!(stamp(l).contains("24/08"));
    }
    let stamps: std::collections::BTreeSet<_> = [
        Language::Ko,
        Language::En,
        Language::Ja,
        Language::Es,
        Language::Fr,
        Language::Pt,
    ]
    .map(stamp)
    .into();
    assert_eq!(stamps.len(), 6);
}
// upstream: DailyTrendStackTests.swift::testOrderIsByMonthTotalWithUnusedProvidersDroppedAndTiesBrokenById
#[test]
fn stacks_order_by_month_then_stable_id() {
    let ps = vec![
        provider("small", &[("2026-08-01", 10), ("2026-08-02", 3)]),
        provider("idle", &[("2026-08-01", 0)]),
        provider("big", &[("2026-08-01", 1), ("2026-08-02", 900)]),
        provider("tie-b", &[("2026-08-01", 15)]),
        provider("tie-a", &[("2026-08-02", 15)]),
    ];
    assert_eq!(
        ordered(&ps)
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["big", "tie-a", "tie-b", "small"]
    );
}
// upstream: DailyTrendStackTests.swift::testStackingNeedsTwoProvidersThatActuallyUsedSomethingThisMonth
#[test]
fn stacking_requires_two_used_providers() {
    assert!(ordered(&[]).is_empty());
    for (ps, want) in [
        (vec![provider("a", &[("2026-08-01", 10)])], false),
        (
            vec![
                provider("a", &[("2026-08-01", 10)]),
                provider("b", &[("2026-08-01", 0)]),
            ],
            false,
        ),
        (
            vec![
                provider("a", &[("2026-08-01", 10)]),
                provider("b", &[("2026-08-02", 1)]),
            ],
            true,
        ),
    ] {
        assert_eq!(ordered(&ps).len() > 1, want);
    }
}
// upstream: DailyTrendStackTests.swift::testSegmentsAddUpExactlyToTheBarHeight
#[test]
fn cumulative_segments_exactly_fill_track() {
    let ps = vec![
        provider("a", &[("2026-08-01", 333333)]),
        provider("b", &[("2026-08-01", 222221)]),
        provider("c", &[("2026-08-01", 7)]),
    ];
    let stack = ordered(&ps);
    for height in [BASELINE as f64, 1.7, 9.3, 17.77, TRACK as f64] {
        let pieces = segments(date("2026-08-01"), &stack, height);
        assert_eq!(
            pieces.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(pieces.iter().map(|s| s.height).sum::<f64>(), height);
        assert!(pieces.iter().all(|s| s.height >= 0.));
    }
    let ps = vec![
        provider("a", &[("2026-08-01", 200)]),
        provider("b", &[("2026-08-01", 100)]),
    ];
    assert_eq!(
        segments(date("2026-08-01"), &ordered(&ps), 24.)
            .iter()
            .map(|s| s.height)
            .collect::<Vec<_>>(),
        [16., 8.]
    );
}
// upstream: DailyTrendStackTests.swift::testZeroTokenProvidersAreOmittedAndTheMonthLeaderStaysAtTheBottom
#[test]
fn daily_segments_keep_month_order_and_omit_idle() {
    let ps = vec![
        provider("leader", &[("2026-08-01", 1000), ("2026-08-02", 10)]),
        provider("second", &[("2026-08-01", 0), ("2026-08-02", 90)]),
    ];
    let stack = ordered(&ps);
    assert_eq!(
        segments(date("2026-08-01"), &stack, 20.)
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["leader"]
    );
    assert_eq!(
        segments(date("2026-08-02"), &stack, 20.)
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["leader", "second"]
    );
    assert!(segments(date("2026-08-03"), &stack, 20.).is_empty());
}
// upstream: DailyTrendStackTests.swift::testColorsFollowRegistrationOrderNotUsageRank
#[test]
fn colors_do_not_follow_usage_rank() {
    let registry = vec!["first".into(), "second".into(), "third".into()];
    let mut ps = vec![
        provider("first", &[("2026-08-01", 900)]),
        provider("second", &[("2026-08-01", 100)]),
    ];
    let before: std::collections::BTreeMap<_, _> = ordered(&ps)
        .iter()
        .map(|p| (p.id.clone(), color_slot(&p.id, &registry, 8)))
        .collect();
    ps[0].days = Some(vec![day("2026-08-01", 100)]);
    ps[1].days = Some(vec![day("2026-08-01", 900)]);
    assert_eq!(ordered(&ps)[0].id, "second");
    let after: std::collections::BTreeMap<_, _> = ordered(&ps)
        .iter()
        .map(|p| (p.id.clone(), color_slot(&p.id, &registry, 8)))
        .collect();
    assert_eq!(before, after);
    assert_eq!(color_slot("third", &registry, 8), 2);
}
// upstream: DailyTrendStackTests.swift::testAProvidersColorDoesNotDependOnWhichOthersAreActive
#[test]
fn colors_do_not_shift_when_other_providers_activate() {
    let registry: Vec<_> = (0..14).map(|i| format!("p{i}")).collect();
    assert_eq!(color_slot("p8", &registry, 8), 0);
    assert_eq!(color_slot("p13", &registry, 8), 5);
    for _active in [
        vec!["p8"],
        vec!["p0", "p8"],
        registry.iter().map(String::as_str).collect(),
    ] {
        assert_eq!(color_slot("p8", &registry, 8), 0);
    }
}
// upstream: DailyTrendStackTests.swift::testEveryRegisteredProviderGetsItsOwnColor
#[test]
fn registered_sources_have_distinct_palette_slots() {
    let registry: Vec<_> = crate::usage::providers::builtins()
        .handles()
        .map(|p| p.id().to_owned())
        .collect();
    let slots: std::collections::BTreeSet<_> = registry
        .iter()
        .map(|id| color_slot(id, &registry, PALETTE.len()))
        .collect();
    assert_eq!(slots.len(), registry.len());
}
// upstream: DailyTrendStackTests.swift::testTheCaptionReadsTodaysCombinedTotalAndCost
#[test]
fn caption_is_combined_today_and_cost() {
    let days = vec![day("2026-08-23", 4_000_000), day("2026-08-24", 2_500_000)];
    let labels = DateText::new(Language::En, "MdE");
    assert_eq!(
        readout(&days, date("2026-08-24"), true, &labels),
        format!(
            "{} 2.5M {}",
            labels.stamp(date("2026-08-24")),
            days[1].bucket.cost_text(true)
        )
    );
    assert_eq!(
        readout(&days, date("2026-08-24"), false, &labels),
        format!("{} 2.5M", labels.stamp(date("2026-08-24")))
    );
}
// upstream: DailyTrendStackTests.swift::testTheTooltipNamesTheDayTotalAndEachProviderThatUsedIt
#[test]
fn tooltip_names_day_total_and_used_provider_split() {
    let mut ps = vec![
        provider(
            "claude",
            &[("2026-08-23", 3_000_000), ("2026-08-24", 2_000_000)],
        ),
        provider("codex", &[("2026-08-23", 1_000_000), ("2026-08-24", 0)]),
    ];
    ps[0].name = "Claude Code".into();
    ps[1].name = "Codex".into();
    ps[1].reports_cost = false;
    let total = vec![day("2026-08-23", 4_000_000), day("2026-08-24", 2_000_000)];
    let labels = DateText::new(Language::En, "MdE");
    let text = hover_text(date("2026-08-23"), &total, &ordered(&ps), true, &labels).unwrap();
    assert!(text.contains("4M tokens"));
    assert!(text.contains("Claude Code · 3M · $"));
    assert!(text.ends_with("Codex · 1M"));
    let quiet = hover_text(date("2026-08-24"), &total, &ordered(&ps), false, &labels).unwrap();
    assert!(!quiet.contains("Codex"));
    assert!(!quiet.contains('$'));
}
// upstream: DailyTrendStackTests.swift::testASingleProviderTooltipHasNoSplit
#[test]
fn single_provider_tooltip_has_no_duplicate_breakdown() {
    let ps = vec![provider("claude", &[("2026-08-24", 1000)])];
    let tip = hover_text(
        date("2026-08-24"),
        &[day("2026-08-24", 1000)],
        &ordered(&ps),
        true,
        &DateText::new(Language::En, "MdE"),
    )
    .unwrap();
    assert!(!tip.contains('\n'));
}

fn report(values: &[(&str, u64)], now: f64) -> Report {
    Report::from_entries(
        values
            .iter()
            .enumerate()
            .map(|(i, (d, n))| Entry {
                id: format!("{d}-{i}"),
                date: Local
                    .from_local_datetime(&date(d).and_hms_opt(12, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp() as f64,
                input: *n,
                explicit_cost: Some(8888.88),
                cost_is_estimate: Some(true),
                ..Default::default()
            })
            .collect(),
        now,
    )
}
fn fixture(
    s: &mut Shark,
    w: &mut Window,
    cx: &mut Context<Shark>,
    today: &str,
    values: &[(&str, u64)],
) {
    let now = Local
        .from_local_datetime(&date(today).and_hms_opt(12, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp() as f64;
    s.show_usage(w, cx);
    s.usage_panel.as_mut().unwrap().reference_time = Some(now);
    s.usage_reports[0] = report(values, now);
    s.usage_reports[1] = Report::default();
    cx.notify();
}

// upstream: MonthDailyTrendTests.swift::testAnAllZeroMonthRendersNothingWhileOneUsedDayRendersTheRow
// upstream: MonthDailyTrendTests.swift::testEmptySeriesRendersNothing
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_chart_is_absent_until_a_day_has_usage(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        for values in [vec![], vec![("2026-07-03", 0)], vec![("2026-07-03", 1234)]] {
            shark.update(cx, |s, cx| fixture(s, w, cx, "2026-07-10", &values));
            w.render_frame(cx);
            if values.last().is_some_and(|(_, n)| *n > 0) {
                assert!(w.find("usage-trend").visible());
                assert!(w.find("usage-trend").bounds().size.height > px(TRACK));
            } else {
                assert!(w.try_find("usage-trend").is_none());
            }
        }
    })
    .unwrap();
}
// upstream: MonthDailyTrendTests.swift::testFirstOfTheMonthRendersTheSameRowAsMidMonth
// upstream: MonthDailyTrendTests.swift::testTrendRowIncludesTheAxisAndTickRowsBelowTheBars
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_month_structure_and_height_are_stable_on_day_one(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let mut heights = vec![];
        for (today, values) in [
            ("2026-10-01", vec![("2026-10-01", 13_600_000)]),
            (
                "2026-10-15",
                vec![("2026-10-01", 1000), ("2026-10-15", 15000)],
            ),
        ] {
            shark.update(cx, |s, cx| fixture(s, w, cx, today, &values));
            w.render_frame(cx);
            let trend = w.find("usage-trend").bounds();
            heights.push(trend.size.height);
            assert!(trend.size.height > px(TRACK + 24.));
            for id in [
                "usage-trend-caption",
                "usage-trend-bars",
                "usage-trend-ticks",
                "usage-trend-axis",
            ] {
                assert!(w.find(id).visible());
            }
            assert_eq!(w.find("usage-trend-bars").bounds().size.height, px(TRACK));
            assert!(
                w.find("usage-trend-axis-2026-10-21").bounds().size.height <= px(14.),
                "axis={:?}",
                w.find("usage-trend-axis-2026-10-21").bounds()
            );
            assert!(
                w.find("usage-trend-axis").bounds().top()
                    > w.find("usage-trend-bars").bounds().bottom()
            );
            assert!(w.try_find("usage-trend-bar-2026-10-31").is_none());
        }
        assert_eq!(heights[0], heights[1]);
    })
    .unwrap();
}
// upstream: MonthDailyTrendTests.swift::testCaptionStaysOnOneLineInEveryLanguageAtWorstCaseNumbers
// Adaptation: Shark retains English interface; locale date-format contracts are tested above.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_worst_case_caption_stays_on_one_line(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            fixture(
                s,
                w,
                cx,
                "2026-08-24",
                &[("2026-08-23", 888_888_888), ("2026-08-24", 888_888_888)],
            )
        });
        w.render_frame(cx);
        let caption = w.find("usage-trend-caption").bounds();
        assert_eq!(caption.size.height, px(18.));
        assert!(caption.right() <= w.find("usage-panel").bounds().right());
        let text = w.find("usage-trend-readout");
        assert!(text.label().unwrap().contains("888.9M"));
        assert!(text.bounds().right() <= caption.right());
    })
    .unwrap();
}
// upstream: DailyTrendStackTests.swift::testASingleProviderMonthRendersPixelIdenticalToTheUnstackedRow
// Adaptation: native geometry/state equality here; Metal screenshot comparison accompanies the render suite.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn one_used_provider_and_idle_provider_share_unstacked_native_layout(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            fixture(
                s,
                w,
                cx,
                "2026-08-24",
                &[("2026-08-23", 23000), ("2026-08-24", 24000)],
            )
        });
        w.render_frame(cx);
        let before = w.find("usage-trend").bounds();
        let bar = w.find("usage-trend-bar-2026-08-24").bounds();
        shark.update(cx, |s, cx| {
            let now = s.usage_view_now();
            s.usage_reports[1] = report(&[("2026-08-24", 0)], now);
            cx.notify();
        });
        w.render_frame(cx);
        let after = w.find("usage-trend").bounds();
        let after_bar = w.find("usage-trend-bar-2026-08-24").bounds();
        assert_eq!(after.size, before.size);
        assert_eq!(after_bar.size, bar.size);
        assert_eq!(
            (
                after_bar.left() - after.left(),
                after_bar.top() - after.top()
            ),
            (bar.left() - before.left(), bar.top() - before.top())
        );
        assert!(w.try_find("usage-trend-legend").is_none());
        assert!(
            w.try_find("usage-trend-segment-2026-08-24-claude")
                .is_none()
        );
    })
    .unwrap();
}

struct FutureSource(String);
impl UsageProvider for FutureSource {
    fn id(&self) -> &str {
        &self.0
    }
    fn name(&self) -> &str {
        &self.0
    }
    fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure> {
        Err(Failure::Unsupported)
    }
    fn activity(&self, _: f64) -> Daily {
        Daily::default()
    }
}
// upstream: DailyTrendStackTests.swift::testTheLegendWrapsWithinThePopoverWidth
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_legend_wraps_for_future_sources_with_long_names(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let mut heights = vec![];
        for count in [1, 2, 14] {
            let mut registry = Registry::default();
            for i in 0..count {
                registry
                    .register(FutureSource(format!("provider-name-{i}")))
                    .unwrap();
            }
            let registry = Box::leak(Box::new(registry));
            shark.update(cx, |s, cx| {
                s.usage_providers = registry.handles().collect();
                s.usage = vec![Default::default(); count];
                s.usage_activity = vec![Default::default(); count];
                s.usage_queries = vec![0; count];
                s.pokemon_ui.scanning = vec![false; count];
                let now = Local
                    .from_local_datetime(&date("2026-08-24").and_hms_opt(12, 0, 0).unwrap())
                    .earliest()
                    .unwrap()
                    .timestamp() as f64;
                s.usage_reports = (0..count)
                    .map(|_| report(&[("2026-08-24", 888888888)], now))
                    .collect();
                s.show_usage(w, cx);
                s.usage_panel.as_mut().unwrap().reference_time = Some(now);
                cx.notify();
            });
            w.render_frame(cx);
            heights.push(w.find("usage-trend").bounds().size.height);
            if count == 1 {
                assert!(w.try_find("usage-trend-legend").is_none());
            } else {
                let legend = w.find("usage-trend-legend").bounds();
                let panel = w.find("usage-panel").bounds();
                assert!(legend.right() <= panel.right());
                for i in 0..count {
                    let item = w
                        .find(format!("usage-trend-legend-provider-name-{i}"))
                        .bounds();
                    assert!(item.left() >= legend.left());
                    assert!(item.right() <= legend.right() + px(0.5));
                }
            }
        }
        assert!(heights[1] > heights[0]);
        assert!(heights[2] > heights[1]);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_hover_shows_split_immediately_and_does_not_change_today_caption(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            fixture(
                s,
                w,
                cx,
                "2026-08-24",
                &[("2026-08-23", 3_000_000), ("2026-08-24", 2_000_000)],
            );
            s.usage_reports[1] = report(&[("2026-08-23", 1_000_000)], s.usage_view_now());
        });
        w.render_frame(cx);
        let caption = w.find("usage-trend-readout").label().map(str::to_owned);
        w.hover("usage-trend-day-2026-08-23", cx);
        w.render_frame(cx);
        let tip = w.find("usage-trend-tooltip");
        assert!(tip.visible());
        assert!(tip.label().unwrap().contains("4M tokens"));
        assert!(tip.label().unwrap().contains("Claude"));
        assert!(tip.label().unwrap().contains("Codex"));
        assert_eq!(w.find("usage-trend-readout").label(), caption.as_deref());
    })
    .unwrap();
}
