//! Check the production navigation's colors with the real GPU renderer.
use super::*;
use gpui_kit::test::TestEventExt;

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    for (mode, mode_name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        let mut state = State::default();
        state.appearance.mode = mode;
        state.save().unwrap();
        let (window, shark) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(1000.), px(720.)),
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
                s.state.workspaces = ["Displayed", "Cursor", "Inactive", "Warning", "Unread", "Running"]
                    .map(|name| Workspace {
                        name: name.into(),
                        branch: name.into(),
                        repos: vec![crate::state::WsRepo {
                            name: "api".into(), src: home.join("api"), base: "main".into(), branch: None,
                        }],
                        chats: vec![ChatMeta { id: name.into(), ..Default::default() }],
                        ..Default::default()
                    }).to_vec();
                s.state.repos = vec![Repo { name: "api".into(), path: home.join("api") }];
                s.state.sections = vec![Section {
                    name: "Projects".into(), icon: "folder".into(), collapsed: false,
                    workspaces: vec!["Warning".into()],
                }];
                s.unread.insert("Unread".into());
                for ix in [0, 1, 3, 5] {
                    let ws = s.state.workspaces[ix].clone();
                    let chat = cx.new(|cx| Chat::new(ws.clone(), ws.chats[0].clone(), s.state.vim, w, cx));
                    chat.update(cx, |c, cx| {
                        if ix == 5 {
                            c.busy = true;
                        } else if ix == 3 {
                            crate::chat::ui_tests::play(c, &[r#"{"type":"control_request","request_id":"nav-question","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"nav-tool","input":{"questions":[{"header":"Color","question":"Which color?","options":[{"label":"Blue","description":"Blue"},{"label":"Red","description":"Red"}]}]}}}"#], cx);
                        }
                    });
                    s.chats.insert(ws.chats[0].id.clone(), chat);
                }
                assert!(matches!(s.attention_of("Warning", cx), Attn::Needs(_)));
                assert_eq!(s.attention_of("Unread", cx), Attn::Unread);
                assert_eq!(s.running_chats("Running", cx), 1);
            });
        }).unwrap();

        for rail in [false, true] {
            for case in [
                "workspace",
                "home",
                "focus",
                "section-focus",
                "folded",
                "rename",
                "switched",
                "hover",
            ] {
                if rail && case == "rename" {
                    continue;
                }
                let checks = cx
                    .update_window(window, |_, w, cx| {
                        shark.update(cx, |s, cx| {
                            s.rail = rail;
                            s.naming = None;
                            s.state.sections[0].collapsed = case == "folded";
                            s.active = Some(
                                if case == "switched" {
                                    "Cursor"
                                } else {
                                    "Displayed"
                                }
                                .into(),
                            );
                            s.home.on = case == "home";
                            s.sel = if case == "section-focus" {
                                SideRow::Section(0)
                            } else {
                                SideRow::Ws(1)
                            };
                            if matches!(case, "focus" | "section-focus") {
                                s.sidebar_focus.focus(w, cx);
                            } else {
                                s.home.focus.focus(w, cx);
                            }
                            if case == "rename" {
                                s.start_naming(Some(0), None, w, cx);
                            }
                            cx.notify();
                        });
                        w.pointer_move(point(px(999.), px(719.)), None, cx);
                        w.render_frame(cx);
                        if case == "hover" {
                            w.within("sidebar").hover(2usize, cx);
                            w.render_frame(cx);
                        }
                        let theme = cx.theme();
                        let (dim, bright, secondary, primary, warning, unread) = (
                            theme.sidebar_foreground,
                            theme.foreground,
                            theme.muted_foreground,
                            theme.primary,
                            theme.warning_foreground,
                            theme.success_foreground,
                        );
                        let mut checks = vec![];
                        let home_row = w.find("home-row").bounds();
                        checks.push(("Home", home_row, if case == "home" { bright } else { dim }));
                        for ix in 0..6 {
                            if ix == 3 && case == "folded" {
                                continue;
                            }
                            let b = w.within("sidebar").find(ix).bounds();
                            let selected = case != "home" && ix == usize::from(case == "switched");
                            let focused = case == "focus" && ix == 1;
                            let fg = if selected || focused { bright } else { dim };
                            let icon = Bounds::new(
                                point(b.left() + px(8.), b.top() + (b.size.height - px(16.)) / 2.),
                                size(px(16.), px(16.)),
                            );
                            checks.push((
                                "workspace icon",
                                icon,
                                match ix {
                                    3 => warning,
                                    4 => unread,
                                    5 => primary,
                                    _ => fg,
                                },
                            ));
                            if !rail {
                                checks.push((
                                    "workspace label",
                                    Bounds::new(
                                        point(b.left() + px(32.), b.top() + px(2.)),
                                        size(b.size.width - px(40.), px(22.)),
                                    ),
                                    fg,
                                ));
                                checks.push((
                                    "workspace metadata",
                                    Bounds::new(
                                        point(b.left() + px(32.), b.top() + px(24.)),
                                        size(b.size.width - px(40.), px(18.)),
                                    ),
                                    if selected || focused { secondary } else { dim },
                                ));
                            }
                            if focused {
                                checks.push((
                                    "focus ring",
                                    Bounds::new(b.origin, size(b.size.width, px(1.))),
                                    primary,
                                ));
                            }
                        }
                        if case == "rename" {
                            checks.push(("inline name", w.find("section-name").bounds(), bright));
                        } else {
                            let b = w.find(("section", 0usize)).bounds();
                            checks.push((
                                "section icon",
                                Bounds::new(
                                    point(
                                        b.left() + px(8.),
                                        b.top() + (b.size.height - px(16.)) / 2.,
                                    ),
                                    size(px(16.), px(16.)),
                                ),
                                match case {
                                    "folded" => warning,
                                    "section-focus" => bright,
                                    _ => dim,
                                },
                            ));
                        }
                        checks.push(("repo", w.find(("repo", 0usize)).bounds(), dim));
                        for id in ["toggle-sidebar", "new-workspace"] {
                            checks.push((id, w.find(id).bounds(), dim));
                        }
                        checks
                    })
                    .unwrap();
                let screenshot = cx
                    .capture_screenshot(window)
                    .expect("real navigation renderer");
                if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                    let dir = PathBuf::from(dir);
                    fs::create_dir_all(&dir).unwrap();
                    screenshot
                        .save(dir.join(format!(
                            "navigation-{mode_name}-{}-{case}.png",
                            if rail { "rail" } else { "expanded" }
                        )))
                        .unwrap();
                }
                let scale = screenshot.width() as f32 / 1000.;
                for (label, bounds, color) in checks {
                    let rgb = color.to_rgb();
                    let expected = [rgb.r, rgb.g, rgb.b].map(|c| (c * 255.).round() as u8);
                    let mut matches = 0;
                    for y in (bounds.top().as_f32() * scale).ceil() as u32
                        ..(bounds.bottom().as_f32() * scale).floor() as u32
                    {
                        for x in (bounds.left().as_f32() * scale).ceil() as u32
                            ..(bounds.right().as_f32() * scale).floor() as u32
                        {
                            let pixel = screenshot.get_pixel(x, y);
                            matches +=
                                usize::from((0..3).all(|c| pixel[c].abs_diff(expected[c]) <= 4));
                        }
                    }
                    assert!(
                        matches > 2,
                        "{mode_name}, rail={rail}, {case}: {label} must render {expected:?} in {bounds:?}, got {matches} pixels"
                    );
                }
            }
        }
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
    }
    fs::remove_dir_all(&home).ok();
    println!(
        "navigation pixels: dark/light, expanded/rail, Home/workspace/focus, statuses, folding and naming passed"
    );
}
