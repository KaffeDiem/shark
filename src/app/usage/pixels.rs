use super::*;
use crate::app::ui_tests::{UI_LOCK, test_home};
use crate::usage::logs::{Entry, Report};
use crate::usage::summary::{Scope, UsageLedger};
use chrono::{Local, NaiveDate, TimeZone};
use gpui_kit::test::TestWindowExt;
use std::collections::BTreeMap;

fn now() -> f64 {
    Local
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(2026, 8, 24)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        )
        .earliest()
        .unwrap()
        .timestamp() as f64
}
fn report(multiplier: u64) -> Report {
    Report::from_entries(
        (1..=24)
            .map(|day| Entry {
                id: format!("fixture-{day}"),
                date: Local
                    .from_local_datetime(
                        &NaiveDate::from_ymd_opt(2026, 8, day)
                            .unwrap()
                            .and_hms_opt(12, 0, 0)
                            .unwrap(),
                    )
                    .earliest()
                    .unwrap()
                    .timestamp() as f64,
                input: day as u64 * 1000 * multiplier,
                explicit_cost: Some(0.3),
                cost_is_estimate: Some(true),
                ..Default::default()
            })
            .collect(),
        now(),
    )
}
fn crop(image: &image::RgbaImage, bounds: Bounds<Pixels>, width: f32) -> image::RgbaImage {
    let scale = image.width() as f32 / width;
    let left = (f32::from(bounds.left()) * scale).round().max(0.) as u32;
    let top = (f32::from(bounds.top()) * scale).round().max(0.) as u32;
    let width = (f32::from(bounds.size.width) * scale).round() as u32;
    let height = (f32::from(bounds.size.height) * scale).round() as u32;
    image::imageops::crop_imm(image, left, top, width, height).to_image()
}
fn save(image: &image::RgbaImage, name: &str) {
    if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        image.save(dir.join(name)).unwrap();
    }
}

fn quiet_report(unavailable: bool) -> Report {
    let at = |day| {
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
    };
    Report::from_entries(
        [(17, 4_800_000, 31.64), (1, 13_100_000, 89.66)]
            .into_iter()
            .map(|(day, input, cost)| Entry {
                id: format!("quiet-{day}"),
                date: at(day),
                input,
                explicit_cost: (!unavailable).then_some(cost),
                cost_unavailable: unavailable.then_some(true),
                ..Default::default()
            })
            .collect(),
        at(18),
    )
}
fn validate_marker(component: &image::RgbaImage, percent: Option<f64>, ink: [u8; 3], scale: f32) {
    let hits: Vec<_> = component
        .enumerate_pixels()
        .filter(|(_, _, p)| (0..3).all(|i| p.0[i].abs_diff(ink[i]) <= 6))
        .map(|(x, y, _)| (x, y))
        .collect();
    let Some(percent) = percent else {
        assert!(hits.is_empty(), "no pace must paint no marker");
        return;
    };
    assert!(
        !hits.is_empty(),
        "pace marker must paint actual theme-ink pixels"
    );
    let center =
        hits.iter().map(|(x, _)| *x as f64 + 0.5).sum::<f64>() / hits.len() as f64 / scale as f64;
    let width = component.width() as f32 / scale;
    let expected = marker_left(percent, width) + 2.25;
    assert!(
        (center - expected as f64).abs() <= 1.5,
        "pace{percent}:paintedcenter{center} expected{expected}"
    );
    let marker_height = (hits.iter().map(|(_, y)| *y).max().unwrap()
        - hits.iter().map(|(_, y)| *y).min().unwrap()
        + 1) as f32
        / scale;
    let x = (10. * scale) as u32;
    let background = component.get_pixel(x, 0).0;
    let track_height = (0..component.height())
        .filter(|y| (0..3).any(|i| component.get_pixel(x, *y).0[i].abs_diff(background[i]) > 10))
        .count() as f32
        / scale;
    assert!(track_height > 0.);
    assert!(
        (marker_height - track_height - 4.).abs() <= 1.,
        "marker{marker_height} track{track_height} must differ by4pt"
    );
}

// Native host-language artifacts; exact chart crop equality and painted pace geometry are verified.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for (width, height, size_label) in [(1120., 840., ""), (640., 480., "-small")] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(height)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| cx.new(|cx| Shark::new(w, cx)),
                    )
                })
                .unwrap();
            let mut single = None;
            for page in [
                "companion",
                "companion-details",
                "single",
                "idle",
                "stacked",
                "hover",
                "Week",
                "Month",
                "Year",
                "quiet-history",
                "quiet-empty",
                "quiet-known",
                "quiet-unavailable",
                "pace-none",
                "pace0",
                "pace25",
                "pace75",
                "pace100",
            ] {
                if width == 640. && page != "companion" {
                    continue;
                }
                let bounds = cx
                    .update_window(window, |_, w, cx| {
                        shark.update(cx, |s, cx| {
                            if page.starts_with("companion") {
                                s.state.pokemon.egg_tokens =
                                    crate::pokemon::Balance::EGG_HATCH_THRESHOLD / 2;
                                s.show_pokemon(w, cx);
                            } else {
                                s.show_usage(w, cx);
                            }
                            s.usage_panel.as_mut().unwrap().reference_time = Some(now());
                            s.usage_panel.as_mut().unwrap().details = page != "companion";
                            s.usage_reports[0] = report(1);
                            s.usage_reports[1] = if page == "single" {
                                Report::default()
                            } else if page == "idle" {
                                report(0)
                            } else {
                                report(2)
                            };
                            for snapshot in &mut s.usage {
                                snapshot.finish(Ok(vec![]));
                            }
                            if page.starts_with("companion") {
                                for (index, snapshot) in s.usage.iter_mut().enumerate() {
                                    snapshot.finish(Ok(vec![
                                        Allowance {
                                            label: "5 hours".into(),
                                            remaining: Some(if index == 0 { 42. } else { 68. }),
                                            resets_at: Some(crate::usage::now() + 9000.),
                                            window: Some(18000.),
                                        },
                                        Allowance {
                                            label: "Weekly".into(),
                                            remaining: Some(if index == 0 { 75. } else { 84. }),
                                            resets_at: Some(crate::usage::now() + 259200.),
                                            window: Some(604800.),
                                        },
                                    ]));
                                }
                            }
                            if page.starts_with("quiet-") {
                                let report = quiet_report(page == "quiet-unavailable");
                                let date = Local
                                    .from_local_datetime(
                                        &NaiveDate::from_ymd_opt(2026, 9, 18)
                                            .unwrap()
                                            .and_hms_opt(12, 0, 0)
                                            .unwrap(),
                                    )
                                    .earliest()
                                    .unwrap()
                                    .timestamp() as f64;
                                s.usage_reports[0] = if page == "quiet-empty" {
                                    Report::from_entries(vec![], date)
                                } else {
                                    report
                                };
                                s.usage_reports[1] = Report::default();
                                s.usage_panel.as_mut().unwrap().reference_time = Some(date);
                            }
                            if page.starts_with("pace") {
                                s.usage_reports = vec![Report::default(); s.usage_providers.len()];
                                let percent = page
                                    .strip_prefix("pace")
                                    .and_then(|value| value.parse::<f64>().ok());
                                s.usage[0].finish(Ok(vec![Allowance {
                                    label: "5 hours".into(),
                                    remaining: Some(50.),
                                    resets_at: percent
                                        .map(|p| crate::usage::now() + (18000. * p / 100.).max(1.)),
                                    window: Some(18000.),
                                }]));
                            }
                            if ["Week", "Month", "Year"].contains(&page) {
                                s.state.usage_ledger = UsageLedger {
                                    covered_since: Some("2026-01-01".into()),
                                    tokens_by_day: BTreeMap::from([
                                        ("2026-08-23".into(), 2500000),
                                        ("2026-08-24".into(), 4000000),
                                        ("2026-08-17".into(), 1500000),
                                    ]),
                                };
                                s.open_usage_recap(cx);
                                s.usage_panel.as_mut().unwrap().recap = Some((
                                    match page {
                                        "Month" => Scope::Month,
                                        "Year" => Scope::Year,
                                        _ => Scope::Week,
                                    },
                                    0,
                                ));
                            }
                            cx.notify();
                        });
                        w.render_frame(cx);
                        w.refresh();
                        w.render_frame(cx);
                        if page.starts_with("companion") {
                            let panel = w.find("usage-panel").bounds();
                            let progress = w.find("pokemon-usage-progress").bounds();
                            if width != 640. {
                                assert!(w.find("pokemon-usage-progress").visible());
                                assert!(
                                    progress.left() >= panel.left()
                                        && progress.right() <= panel.right()
                                        && progress.top() >= panel.top()
                                        && progress.bottom() <= panel.bottom(),
                                    "progress must fit without scrolling: {progress:?} vs {panel:?}"
                                );
                            }
                            assert!(w.find("usage-codex-1").bounds().bottom() <= progress.top());
                            if page == "companion-details" {
                                assert!(progress.bottom() <= w.find("usage-trend").bounds().top());
                            } else {
                                assert!(w.try_find("usage-trend").is_none());
                                assert!(w.try_find("usage-accounting-claude").is_none());
                                assert!(w.try_find("pokemon-tab-Home").is_none());
                            }
                            assert!(w.try_find("usage-heatmap-claude").is_none());
                            assert!(w.try_find("usage-heatmap-codex").is_none());
                        }
                        if page == "hover" {
                            w.hover("usage-trend-day-2026-08-23", cx);
                            w.render_frame(cx);
                            assert!(w.find("usage-trend-tooltip").visible());
                        }
                        if page.starts_with("companion") {
                            w.find("usage-panel").bounds()
                        } else if page.starts_with("pace") {
                            let bar = w.find("usage-bar-claude-0").bounds();
                            Bounds::new(
                                point(bar.left(), bar.top() - px(2.)),
                                size(bar.size.width, px(8.)),
                            )
                        } else {
                            w.find(if ["Week", "Month", "Year"].contains(&page) {
                                "usage-recap-card"
                            } else if page.starts_with("quiet-") {
                                "usage-combined-totals"
                            } else {
                                "usage-trend"
                            })
                            .bounds()
                        }
                    })
                    .unwrap();
                let screenshot = cx
                    .capture_screenshot(window)
                    .expect("native Usage Metal renderer");
                if width == 640. {
                    cx.update_window(window, |_, w, cx| {
                        let panel = w.find("usage-panel").bounds();
                        for _ in 0..10 {
                            let progress = w.find("pokemon-usage-progress");
                            if progress.visible() && progress.bounds().bottom() <= panel.bottom() {
                                break;
                            }
                            w.scroll(
                                "usage-panel",
                                ScrollDelta::Pixels(point(px(0.), px(-80.))),
                                cx,
                            );
                            w.render_frame(cx);
                        }
                        let progress = w.find("pokemon-usage-progress");
                        assert!(progress.visible());
                        assert!(
                            progress.bounds().top() >= panel.top()
                                && progress.bounds().bottom() <= panel.bottom()
                        );
                    })
                    .unwrap();
                    let scrolled = cx
                        .capture_screenshot(window)
                        .expect("reachable compact progress");
                    save(
                        &scrolled,
                        &format!("usage-companion-small-scrolled-{label}.png"),
                    );
                }
                let component = crop(&screenshot, bounds, width);
                save(
                    &screenshot,
                    &format!("usage-{page}{size_label}-{label}.png"),
                );
                save(
                    &component,
                    &format!("usage-{page}{size_label}-component-{label}.png"),
                );
                if page.starts_with("pace") {
                    let ink = cx.update(|cx| cx.theme().popover_foreground.to_rgb());
                    eprintln!(
                        "{page}-{label} bounds={bounds:?} ink={ink:?} component={}x{}",
                        component.width(),
                        component.height()
                    );
                    validate_marker(
                        &component,
                        page.strip_prefix("pace").and_then(|p| p.parse().ok()),
                        [
                            (ink.r * 255.).round() as u8,
                            (ink.g * 255.).round() as u8,
                            (ink.b * 255.).round() as u8,
                        ],
                        screenshot.width() as f32 / width,
                    );
                }
                assert!(component.width() > 0 && component.height() > 0);
                if page == "single" {
                    single = Some(component.clone());
                }
                if page == "idle" {
                    assert_eq!(
                        single.as_ref().unwrap(),
                        &component,
                        "single-active and idle-provider chart pixels must be identical ({label})"
                    );
                }
                save(
                    &screenshot,
                    &format!("usage-{page}{size_label}-{label}.png"),
                );
                save(
                    &component,
                    &format!("usage-{page}{size_label}-component-{label}.png"),
                );
            }
        }
    }
    println!(
        "Usage real Metal renders:38passed; single/idle chart RGBA crops identical in dark+light; recap Week/Month/Year rendered"
    );
}
