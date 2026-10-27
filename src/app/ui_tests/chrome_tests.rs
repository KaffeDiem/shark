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
            let press = press_in(window);
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
                        let commands = w.find("commands");
                        assert!(commands.visible());
                        let commands = commands.bounds();
                        let create = w.find("new-workspace").bounds();
                        assert_eq!(commands.right() + px(4.), create.left());
                        assert_eq!(commands.top(), create.top());
                        assert_eq!(commands.size, create.size);
                        assert!(w.try_find("tools-commands").is_none());
                        assert!(w.try_find("rail-commands").is_none());
                        let home = w.find("home-row").bounds();
                        if rail {
                            assert_eq!(home.size.width, px(32.));
                            assert_eq!(left.right() + px(4.), commands.left());
                        } else {
                            assert!(home.size.width < px(140.), "Home fits its contents");
                            assert!(home.right() < commands.left());
                        }
                        w.click("commands", cx);
                        assert!(w.has_active_dialog(cx), "Commands opens the palette");
                        w.close_dialog(cx);
                    })
                    .unwrap();
                    press(cx, &["cmd-k"]);
                    cx.update_window(window, |_, w, cx| {
                        assert!(w.has_active_dialog(cx), "Cmd+K still opens the palette");
                        w.close_dialog(cx);
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

struct MenuPixels {
    fade: bool,
    selected: bool,
}

impl Render for MenuPixels {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = if self.selected {
            theme.popover.blend(menu::highlight(theme))
        } else {
            theme.popover
        };
        let text = |id: &'static str, text: &'static str, width: f32| {
            let label = if self.fade {
                crate::overflow_fade::label(text.into(), background, None)
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(text)
            };
            label.id(id).w(px(width)).flex_none().test_support()
        };
        div().size_full().p(px(8.)).bg(theme.background).child(
            menu::chrome(theme)
                .w(px(320.))
                .rounded(theme.radius_lg)
                .font_features(inter_features())
                .children(
                    [
                        ("menu-fit-label", "Copy…", "menu-fit-hint", "main"),
                        (
                            "menu-long-label",
                            "An exceptionally long action name keeps its intentional ellipsis…",
                            "menu-long-hint",
                            "feature/a-branch-with-a-long-name",
                        ),
                    ]
                    .into_iter()
                    .map(|(label_id, label, hint_id, hint)| {
                        h_flex()
                            .h(menu::ROW_H)
                            .px(px(10.))
                            .gap(px(8.))
                            .rounded(theme.radius)
                            .when(self.selected, |d| d.bg(menu::highlight(theme)))
                            .child(Icon::new(IconName::GitBranch).with_size(px(16.)))
                            .child(text(label_id, label, 152.))
                            .child(text(hint_id, hint, 64.).text_color(theme.muted_foreground))
                            .child(Icon::new(IconName::Check).with_size(px(16.)))
                    }),
                ),
        )
    }
}

/// Opt-in main-thread test: compare real glyph pixels with and without the production fade.
pub(crate) fn render_pixels() {
    use gpui_kit::{HeadlessAppContext, assets::AllAssets};
    use std::sync::Arc;

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(AllAssets),
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
    render_menu_pixels(&mut cx);
    super::menu_pixels::render_pixels(&mut cx);
    render_app_switcher_pixels(&mut cx);
    super::workspace_picker_tests::render_pixels(&mut cx);
    super::navigation_pixels::render_pixels(&mut cx);
    super::tree_pixels::render_pixels(&mut cx);
}

/// Render the real Shark overlay, including scrolling the last app into a narrow window.
fn render_app_switcher_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let terminal = objc2_app_kit::NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&objc2_foundation::NSString::from_str(
            "com.apple.Terminal",
        ))
        .and_then(|url| url.path())
        .expect("Terminal ships with macOS");
    let png = crate::apps::icon_png(PathBuf::from(terminal.to_string()).as_path())
        .expect("production Terminal icon extraction");
    let dimensions = imagesize::blob_size(&png).expect("native icon PNG");
    assert!(
        dimensions.width >= 128 && dimensions.height >= 128,
        "large switcher icons need a large source image"
    );
    let icon = Arc::new(Image::from_bytes(ImageFormat::Png, png));
    let many: &'static [crate::apps::OpenApp] = Box::leak(
        (0..12)
            .map(|ix| crate::apps::OpenApp {
                name: format!("Terminal {}", ix + 1),
                path: PathBuf::from(terminal.to_string()),
                kind: crate::apps::Kind::Terminal,
                icon: Some(icon.clone()),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for width in [1000., 320.] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(width), px(560.)),
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
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    s.state.workspaces = vec![Workspace {
                        name: "switcher-preview".into(),
                        branch: "switcher-preview".into(),
                        ..Default::default()
                    }];
                    s.sel = SideRow::Ws(0);
                    s.sidebar_focus.focus(w, cx);
                    s.cycle_open_app(true, w, cx);
                });
                w.render_frame(cx);
                let panel = w.find("app-switcher").bounds();
                let switcher = shark.read(cx).app_switcher.as_ref().unwrap();
                let selected = w.find(format!("app-switcher-app-{}", switcher.selected));
                let tile = selected.bounds();
                assert!(selected.visible(), "{mode_name}, width={width}");
                assert!(
                    tile.left() >= panel.left() + px(20.)
                        && tile.right() <= panel.right() - px(20.),
                    "selected app must scroll fully into view: {mode_name}, width={width}, {tile:?}, {panel:?}"
                );
                assert!(
                    panel.left() >= px(16.) && panel.right() <= px(width - 16.)
                        && panel.top() >= px(16.) && panel.bottom() <= px(544.),
                    "switcher must fit the viewport: {mode_name}, width={width}, {panel:?}"
                );
                assert!(
                    (panel.left() + panel.right() - px(width)).abs() <= px(1.)
                        && (panel.top() + panel.bottom() - px(560.)).abs() <= px(1.),
                    "switcher must be centered: {mode_name}, width={width}, {panel:?}"
                );
                if width == 320. {
                    assert!(
                        w.find("app-switcher-app-0").bounds().left() < panel.left() + px(20.),
                        "narrow preview must exercise horizontal overflow"
                    );
                }
            })
            .unwrap();
            let screenshot = cx
                .capture_screenshot(window)
                .expect("real switcher renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!("app-switcher-{mode_name}-{}.png", width as usize)))
                    .unwrap();
            }
            // Exercise the image branch and scroll beyond the seven tiles that fit the width cap.
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| {
                    let switcher = s.app_switcher.as_mut().unwrap();
                    switcher.apps = many.iter().collect();
                    switcher.selected = 0;
                    s.cycle_open_app(true, w, cx);
                });
                w.render_frame(cx);
                assert_switcher_selection_visible(w, cx, &shark);
            })
            .unwrap();
            cx.run_until_parked();
            for resized_width in [320., 1000.] {
                cx.update_window(window, |_, w, cx| {
                    w.resize(size(px(resized_width), px(560.)));
                    w.bounds_changed(cx);
                    assert_eq!(w.viewport_size().width, px(resized_width));
                    w.render_frame(cx);
                })
                .unwrap();
                // The house scroll pattern settles a changed viewport on the next frame.
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    assert_switcher_selection_visible(w, cx, &shark);
                    for backwards in [false, true, true, false] {
                        shark.update(cx, |s, cx| s.cycle_open_app(backwards, w, cx));
                        w.render_frame(cx);
                        assert_switcher_selection_visible(w, cx, &shark);
                    }
                })
                .unwrap();
                let screenshot = cx
                    .capture_screenshot(window)
                    .expect("native app icon renderer");
                if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                    screenshot
                        .save(PathBuf::from(dir).join(format!(
                            "app-switcher-native-{mode_name}-{}-{}.png",
                            width as usize, resized_width as usize,
                        )))
                        .unwrap();
                }
            }
            // A single app must still show its icon and wrap the longer keyboard hint inside the panel.
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, _| {
                    let switcher = s.app_switcher.as_mut().unwrap();
                    switcher.apps = vec![&many[0]];
                    switcher.selected = 0;
                });
                w.render_frame(cx);
                assert_switcher_selection_visible(w, cx, &shark);
                let panel = w.find("app-switcher").bounds();
                assert!(panel.top() >= px(16.) && panel.bottom() <= px(544.));
            })
            .unwrap();
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    fs::remove_dir_all(&home).ok();
    println!(
        "app switcher previews: both themes, one/many apps, native icons, cycling and resize passed"
    );
}

fn assert_switcher_selection_visible(w: &Window, cx: &App, shark: &Entity<Shark>) {
    let panel = w.find("app-switcher").bounds();
    let selected = shark.read(cx).app_switcher.as_ref().unwrap().selected;
    let tile = w.find(format!("app-switcher-app-{selected}")).bounds();
    assert!(
        tile.left() >= panel.left() + px(20.) && tile.right() <= panel.right() - px(20.),
        "selected app {selected} must remain visible: {tile:?}, panel {panel:?}"
    );
    assert!(panel.left() >= px(16.) && panel.right() <= w.viewport_size().width - px(16.));
}

fn render_menu_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let (window, menu) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(336.), px(100.)),
                    ))),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|_| MenuPixels {
                        fade: false,
                        selected: false,
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
        for selected in [false, true] {
            let bounds = cx
                .update_window(window, |_, w, cx| {
                    menu.update(cx, |menu, cx| {
                        menu.fade = false;
                        menu.selected = selected;
                        cx.notify();
                    });
                    w.render_frame(cx);
                    [
                        w.find("menu-long-label").bounds(),
                        w.find("menu-long-hint").bounds(),
                    ]
                })
                .unwrap();
            let plain = cx
                .capture_screenshot(window)
                .expect("real offscreen renderer");
            cx.update_window(window, |_, w, cx| {
                menu.update(cx, |menu, cx| {
                    menu.fade = true;
                    cx.notify();
                });
                w.render_frame(cx);
            })
            .unwrap();
            let faded = cx
                .capture_screenshot(window)
                .expect("real offscreen renderer");
            let scale = plain.width() as f32 / 336.;
            let strips = bounds.map(|b| {
                (
                    ((b.right() - px(24.)).as_f32() * scale).floor() as u32,
                    (b.right().as_f32() * scale).ceil() as u32,
                    (b.top().as_f32() * scale).floor() as u32,
                    (b.bottom().as_f32() * scale).ceil() as u32,
                )
            });
            for (x, y, before) in plain.enumerate_pixels() {
                if !strips.iter().any(|&(start, end, top, bottom)| {
                    x >= start && x < end && y >= top && y < bottom
                }) {
                    assert_eq!(
                        before,
                        faded.get_pixel(x, y),
                        "menu fade changed fitting text or controls: {mode_name}, selected={selected}, ({x},{y})"
                    );
                }
            }
            for (start, end, top, bottom) in strips {
                for x in start..end {
                    assert!(
                        (0..4).all(|channel| plain.get_pixel(x, top)[channel]
                            .abs_diff(faded.get_pixel(x, top)[channel])
                            <= 2),
                        "menu fade background seam: {mode_name}, selected={selected}, ({x},{top})"
                    );
                }
                let background = plain.get_pixel(end - 1, top);
                let mut before_contrast = 0u64;
                let mut after_contrast = 0u64;
                for x in end - (6. * scale) as u32..end {
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
                    "menu text must fade at the edge: {mode_name}, selected={selected}"
                );
            }
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                faded
                    .save(dir.join(format!(
                        "menu-{mode_name}-{}.png",
                        if selected { "selected" } else { "normal" }
                    )))
                    .unwrap();
            }
        }
    }
    println!("menu fade pixels: 4 passed (real offscreen renderer)");
}
