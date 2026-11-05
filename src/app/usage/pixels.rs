use super::*;
use crate::app::ui_tests::{UI_LOCK, test_home};
use gpui_kit::test::TestWindowExt;

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

/// Where the tick's centre lies on a bar `width` wide for `percent` left at an even pace.
fn tick_center(percent: f64, width: f32) -> f32 {
    let left = (percent.clamp(0., 100.) / 100.) as f32 * width - 2.25;
    left.clamp(0., (width - 4.5).max(0.)) + 2.25
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
    let expected = tick_center(percent, width);
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

// Native renders of the footer and the panel, with the painted pace tick checked against its geometry.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for (width, height) in [(1120., 840.)] {
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
            for page in [
                "footer",
                "footer-rail",
                "panel",
                "pace-none",
                "pace0",
                "pace25",
                "pace75",
                "pace100",
            ] {
                let bounds = cx
                    .update_window(window, |_, w, cx| {
                        shark.update(cx, |s, cx| {
                            s.rail = page == "footer-rail";
                            s.usage[0].finish(Ok(vec![
                                Allowance {
                                    label: "5 hours".into(),
                                    remaining: Some(42.),
                                    resets_at: Some(crate::usage::now() + 9000.),
                                    window: Some(18000.),
                                },
                                Allowance {
                                    label: "Weekly".into(),
                                    remaining: Some(8.),
                                    resets_at: Some(crate::usage::now() + 302400.),
                                    window: Some(604800.),
                                },
                            ]));
                            s.usage[1].finish(Ok(vec![Allowance {
                                label: "5 hours".into(),
                                remaining: Some(68.),
                                resets_at: Some(crate::usage::now() + 9000.),
                                window: Some(18000.),
                            }]));
                            if page.starts_with("footer") {
                                s.close_usage(w, cx);
                            } else {
                                s.show_usage(w, cx);
                            }
                            if page.starts_with("pace") {
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
                            cx.notify();
                        });
                        w.render_frame(cx);
                        w.refresh();
                        w.render_frame(cx);
                        if page.starts_with("pace") {
                            let bar = w.find("usage-bar-claude-0").bounds();
                            Bounds::new(
                                point(bar.left(), bar.top() - px(2.)),
                                size(bar.size.width, px(8.)),
                            )
                        } else {
                            w.find(if page.starts_with("footer") {
                                "usage-footer"
                            } else {
                                "usage-panel"
                            })
                            .bounds()
                        }
                    })
                    .unwrap();
                let screenshot = cx
                    .capture_screenshot(window)
                    .expect("native Usage Metal renderer");
                let component = crop(&screenshot, bounds, width);
                save(&screenshot, &format!("usage-{page}-{label}.png"));
                save(&component, &format!("usage-{page}-component-{label}.png"));
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
            }
        }
    }
    println!("Usage real Metal renders:16 passed; footer, panel and pace ticks in dark+light");
}
