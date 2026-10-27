use super::*;
use gpui_kit::test::TestEventExt;

/// Both themes keep compact Home chrome and align pane toggles at either window width.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn sidebar_chrome_themes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let repos = repos(&home, &["api"]);
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for width in [800., 1440.] {
            let mut state = State {
                repos: repos.clone(),
                ..Default::default()
            };
            state.appearance.mode = mode;
            let (window, shark) = boot(
                cx,
                state,
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(width), px(700.)),
                    ))),
                    ..Default::default()
                },
            );
            let on = on_app(window, shark.clone());
            let name = format!("chrome-{label}-{}", width as usize);
            on(cx, &|s, w, cx| {
                s.create_workspace(
                    name.clone(),
                    git::on_branch(&repos, &name),
                    vec![],
                    None,
                    w,
                    cx,
                )
            });
            cx.wait_for(window, Duration::from_secs(10), |_, cx| {
                shark.read(cx).active.as_deref() == Some(name.as_str())
            })
            .await;
            for rail in [false, true] {
                for tree in [false, true] {
                    on(cx, &|s, w, cx| {
                        if s.rail != rail {
                            s.toggle_sidebar(cx);
                        }
                        if s.show_tree != tree {
                            s.toggle_tree(w, cx);
                        }
                    });
                    cx.update_window(window, |_, w, cx| {
                        w.render_frame(cx);
                        let left = w.find("toggle-sidebar").bounds();
                        let right_id = if tree { "hide-tree" } else { "show-tree" };
                        let right = w.find(right_id);
                        assert!(right.visible(), "{label}, width={width}, rail={rail}");
                        assert_eq!(right.bounds().top(), left.top());
                        assert_eq!(right.bounds().size.height, left.size.height);
                        assert_eq!(left.top(), px(8.));
                        let home = w.find("home-row").bounds();
                        if rail {
                            assert_eq!(home.size.width, px(32.));
                        } else {
                            assert!(home.size.width < px(140.), "Home fits its contents");
                            assert!(home.right() < w.find("new-workspace").bounds().left());
                        }
                    })
                    .unwrap();
                }
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    fs::remove_dir_all(&home).ok();
}

struct TabPixels {
    fade: bool,
    active: bool,
}

impl Render for TabPixels {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .size_full()
            .p(px(8.))
            .gap(px(4.))
            .bg(theme.sidebar)
            .font_features(inter_features())
            .children(
                [
                    ("short", "Chat"),
                    (
                        "long",
                        "A deliberately long workspace title extending beyond the tab",
                    ),
                ]
                .into_iter()
                .map(|(id, text)| {
                    let label = if self.fade {
                        tab_label(text.into(), self.active, theme)
                    } else {
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(text)
                    };
                    pill(id, self.active, theme)
                        .px_2p5()
                        .child(label.id(format!("{id}-label")).test_support())
                        .test_support()
                }),
            )
    }
}

/// Opt-in main-thread test: compare real glyph pixels with and without the production fade.
pub(crate) fn render_pixels() {
    use gpui_kit::{HeadlessAppContext, assets::Assets};
    use std::sync::Arc;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::install_fonts(cx);
    });
    let (window, tabs) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(420.), px(60.)),
                    ))),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|_| TabPixels {
                        fade: false,
                        active: false,
                    })
                },
            )
        })
        .unwrap();
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        cx.update(|cx| {
            crate::settings::apply(
                &crate::state::Appearance {
                    mode,
                    ..Default::default()
                },
                cx,
            )
        });
        for active in [false, true] {
            for hover in [None, Some("short"), Some("long")] {
                let bounds = cx
                    .update_window(window, |_, w, cx| {
                        tabs.update(cx, |tabs, cx| {
                            tabs.active = active;
                            tabs.fade = false;
                            cx.notify();
                        });
                        w.render_frame(cx);
                        if let Some(id) = hover {
                            w.hover(id, cx);
                        } else {
                            w.pointer_move(point(px(0.), px(0.)), None, cx);
                        }
                        w.render_frame(cx);
                        w.find("long-label").bounds()
                    })
                    .unwrap();
                let plain = cx
                    .capture_screenshot(window)
                    .expect("real offscreen renderer");
                cx.update_window(window, |_, w, cx| {
                    tabs.update(cx, |tabs, cx| {
                        tabs.fade = true;
                        cx.notify();
                    });
                    w.render_frame(cx);
                })
                .unwrap();
                let faded = cx
                    .capture_screenshot(window)
                    .expect("real offscreen renderer");
                let scale = plain.width() as f32 / 420.;
                let start = ((bounds.right() - px(24.)).as_f32() * scale).floor() as u32;
                let end = (bounds.right().as_f32() * scale).ceil() as u32;
                let top = (bounds.top().as_f32() * scale).floor() as u32;
                let bottom = (bounds.bottom().as_f32() * scale).ceil() as u32;
                let mut changes = 0;
                for (x, y, before) in plain.enumerate_pixels() {
                    let after = faded.get_pixel(x, y);
                    if x >= start && x < end && y >= top && y < bottom {
                        changes += usize::from(before != after);
                    } else {
                        assert_eq!(
                            before, after,
                            "fade changed a fitting title or pixels outside overflow: {mode_name}, active={active}, hover={hover:?}, ({x},{y})"
                        );
                    }
                }
                assert!(
                    changes > 10,
                    "overflow must fade visible glyph pixels: {mode_name}, active={active}, hover={hover:?}"
                );
                // Metal dithers gradients by ±2/255; the surface must match within that noise.
                for x in start..end {
                    assert!(
                        (0..4).all(|channel| plain.get_pixel(x, top)[channel]
                            .abs_diff(faded.get_pixel(x, top)[channel])
                            <= 2),
                        "fade background mismatch: {mode_name}, active={active}, hover={hover:?}, x={x}, y={top}, before={:?}, after={:?}",
                        plain.get_pixel(x, top),
                        faded.get_pixel(x, top)
                    );
                }
                let background = plain.get_pixel(end - 1, top);
                let mut before_contrast = 0u64;
                let mut after_contrast = 0u64;
                for x in end - ((6. * scale) as u32)..end {
                    for y in top..bottom {
                        for channel in 0..3 {
                            before_contrast +=
                                plain.get_pixel(x, y)[channel].abs_diff(background[channel]) as u64;
                            after_contrast +=
                                faded.get_pixel(x, y)[channel].abs_diff(background[channel]) as u64;
                        }
                    }
                }
                assert!(
                    before_contrast > 0 && after_contrast * 2 < before_contrast,
                    "text must fade toward the right edge: {mode_name}, active={active}, hover={hover:?}"
                );
                if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                    let dir = PathBuf::from(dir);
                    fs::create_dir_all(&dir).unwrap();
                    faded
                        .save(dir.join(format!(
                            "tabs-{mode_name}-{}-{}.png",
                            if active { "active" } else { "idle" },
                            hover.unwrap_or("normal")
                        )))
                        .unwrap();
                }
            }
        }
    }
    println!("tab fade pixels: 12 passed (real offscreen renderer)");
}
