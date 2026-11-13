//! Real Metal checks for quiet toolbar states and the Git split-button geometry.
use super::{quiet_toolbar, toolbar};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Selectable as _, h_flex, v_flex};
use gpui_kit::test::{TestEventExt, TestWindowExt};
use gpui_kit::*;

struct Preview {
    state: &'static str,
    clicks: usize,
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .p(px(24.))
            .gap(px(16.))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_features(super::inter_features())
            .child(format!("Toolbar · {}", self.state))
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(toolbar(
                        "preview",
                        Button::new("preview")
                            .size(px(32.))
                            .icon(IconName::Link)
                            .accessibility_label("Copy link")
                            .selected(self.state == "selected")
                            .open(self.state == "open")
                            .disabled(self.state == "disabled")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clicks += 1;
                                cx.notify();
                            })),
                    ))
                    .child(quiet_toolbar(
                        "quiet",
                        Button::new("quiet")
                            .size(px(32.))
                            .icon(IconName::Plus)
                            .accessibility_label("Add"),
                    ))
                    .child(toolbar(
                        "model",
                        Button::new("model").label("Opus 4.6").dropdown_caret(true),
                    ))
                    .child(
                        h_flex()
                            .child(toolbar(
                                "split-action",
                                Button::new("split-action")
                                    .icon(IconName::GitBranch)
                                    .label("Copy branch")
                                    .rounded_tl(px(16.))
                                    .rounded_bl(px(16.))
                                    .rounded_tr(px(0.))
                                    .rounded_br(px(0.))
                                    .border_r_0(),
                            ))
                            .child(toolbar(
                                "split-menu",
                                Button::new("split-menu")
                                    .size(px(32.))
                                    .icon(IconName::ChevronDown)
                                    .accessibility_label("Git menu")
                                    .rounded_tl(px(0.))
                                    .rounded_bl(px(0.))
                                    .rounded_tr(px(16.))
                                    .rounded_br(px(16.))
                                    .border_l_0(),
                            )),
                    ),
            )
    }
}

pub(crate) fn render_pixels(cx: &mut HeadlessAppContext) {
    let previous_motion = cx.update(|cx| {
        let previous = cx.reduce_motion();
        cx.set_reduce_motion(true);
        previous
    });
    for (mode, name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        cx.update(|cx| {
            crate::theme::apply(
                &crate::state::Appearance {
                    mode,
                    ..Default::default()
                },
                cx,
            );
        });
        let (window, preview) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            Point::default(),
                            size(px(600.), px(140.)),
                        ))),
                        focus: false,
                        show: false,
                        ..Default::default()
                    },
                    cx,
                    |_, cx| {
                        cx.new(|_| Preview {
                            state: "idle",
                            clicks: 0,
                        })
                    },
                )
            })
            .unwrap();
        for hovered in [false, true] {
            let (bounds, surface) = cx
                .update_window(window, |_, w, cx| {
                    preview.update(cx, |preview, cx| {
                        preview.state = "idle";
                        cx.notify();
                    });
                    w.pointer_move(point(px(0.), px(0.)), None, cx);
                    if hovered {
                        w.hover("quiet", cx);
                    }
                    w.render_frame(cx);
                    let surface = if hovered {
                        cx.theme().secondary_hover
                    } else {
                        cx.theme().background
                    };
                    (w.find("quiet").bounds(), surface)
                })
                .unwrap();
            let screenshot = cx
                .capture_screenshot(window)
                .expect("real toolbar renderer");
            let scale = screenshot.width() as f32 / 600.;
            let edge = bounds.origin + point(px(1.), bounds.size.height / 2.);
            let inside = bounds.origin + point(px(6.), bounds.size.height / 2.);
            let expected = Rgba::from(surface);
            for sample in [edge, inside] {
                let pixel = screenshot.get_pixel(
                    (f32::from(sample.x) * scale) as u32,
                    (f32::from(sample.y) * scale) as u32,
                );
                for (c, want) in [expected.r, expected.g, expected.b].into_iter().enumerate() {
                    assert!(
                        pixel[c].abs_diff((want * 255.) as u8) <= 2,
                        "{name} quiet hovered={hovered}: {pixel:?} at {sample:?}"
                    );
                }
            }
        }
        let mut idle = None;
        for state in ["idle", "hover", "selected", "open", "disabled"] {
            let bounds = cx
                .update_window(window, |_, w, cx| {
                    w.pointer_move(point(px(0.), px(0.)), None, cx);
                    preview.update(cx, |preview, cx| {
                        preview.state = state;
                        cx.notify();
                    });
                    w.render_frame(cx);
                    if state == "hover" {
                        w.hover("preview", cx);
                    }
                    w.render_frame(cx);
                    let left = w.find("split-action").bounds();
                    let right = w.find("split-menu").bounds();
                    assert_eq!(left.right(), right.left(), "split halves must touch");
                    assert_eq!(left.top(), right.top());
                    assert_eq!(left.size.height, px(32.));
                    assert_eq!(right.size, size(px(32.), px(32.)));
                    w.find("preview").bounds()
                })
                .unwrap();
            let screenshot = cx
                .capture_screenshot(window)
                .expect("real toolbar renderer");
            let engaged = matches!(state, "hover" | "selected" | "open");
            let (fill, border) = match (name, engaged) {
                ("dark", false) => ([28_u8, 28, 29], [48_u8, 48, 49]),
                ("dark", true) => ([37, 37, 38], [48, 48, 49]),
                (_, false) => ([244, 244, 245], [221, 221, 222]),
                (_, true) => ([237, 237, 238], [221, 221, 222]),
            };
            let scale = screenshot.width() as f32 / 600.;
            for (part, expected) in [("fill", fill), ("border", border)] {
                let matches = screenshot
                    .enumerate_pixels()
                    .filter(|(x, y, pixel)| {
                        bounds.contains(&point(px(*x as f32 / scale), px(*y as f32 / scale)))
                            && (0..3).all(|c| pixel[c].abs_diff(expected[c]) <= 2)
                    })
                    .count();
                assert!(matches > 4, "{name} {state} keeps its {part}: {expected:?}");
            }
            if let Some(baseline) = &idle {
                let baseline: &image::RgbaImage = baseline;
                let scale = screenshot.width() as f32 / 600.;
                let changed = (0..screenshot.height())
                    .flat_map(|y| (0..screenshot.width()).map(move |x| (x, y)))
                    .filter(|&(x, y)| {
                        bounds.contains(&point(px(x as f32 / scale), px(y as f32 / scale)))
                            && screenshot.get_pixel(x, y) != baseline.get_pixel(x, y)
                    })
                    .count();
                assert!(changed > 10, "{name} {state} must look distinct from idle");
            } else {
                idle = Some(screenshot.clone());
            }
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(std::path::PathBuf::from(dir).join(format!("toolbar-{name}-{state}.png")))
                    .unwrap();
            }
        }
        cx.update_window(window, |_, w, cx| {
            let target = w.find("preview").bounds().center();
            w.pointer_down(target, MouseButton::Left, cx);
            w.pointer_up(target, MouseButton::Left, cx);
            assert_eq!(
                preview.read(cx).clicks,
                0,
                "disabled toolbar actions stay inert"
            );
            preview.update(cx, |preview, cx| {
                preview.state = "idle";
                cx.notify();
            });
            w.render_frame(cx);
            w.click("preview", cx);
            assert_eq!(
                preview.read(cx).clicks,
                1,
                "enabled toolbar action executes once"
            );
            w.remove_window();
        })
        .unwrap();
    }
    cx.update(|cx| cx.set_reduce_motion(previous_motion));
    println!(
        "toolbar pixels: 14 passed (dark/light states, quiet surfaces, split geometry and disabled actions)"
    );
}
