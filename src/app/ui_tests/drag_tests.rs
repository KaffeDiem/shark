use super::*;
use gpui_kit::test::TestEventExt;

fn fixture() -> State {
    let mut state = State::default();
    for name in ["Alpha", "Beta", "Gamma"] {
        let mut ws = Workspace {
            name: name.into(),
            branch: "main".into(),
            chats: ["one", "two", "three"]
                .map(|id| ChatMeta {
                    id: format!("{name}-{id}"),
                    title: id.into(),
                    ..Default::default()
                })
                .to_vec(),
            ..Default::default()
        };
        ws.normalize_tabs();
        fs::create_dir_all(ws.dir()).unwrap();
        state.workspaces.push(ws);
        state.sections.push(Section {
            name: format!("{name} section"),
            icon: "folder".into(),
            collapsed: false,
            workspaces: vec![name.into()],
        });
    }
    state.last_open = Some(("Alpha".into(), "chat".into()));
    state
}

fn options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(1200.), px(720.)),
        ))),
        ..Default::default()
    }
}

fn assert_inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>) {
    assert!(
        inner.left() >= outer.left() && inner.right() <= outer.right(),
        "{inner:?} outside {outer:?}"
    );
    assert!(
        inner.top() >= outer.top() && inner.bottom() <= outer.bottom(),
        "{inner:?} outside {outer:?}"
    );
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn tab_drag_is_bounded_and_animates_before_release(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let state = fixture();
    let original = state.workspaces[0].tab_order.clone();
    let (window, shark) = boot(cx, state, options());
    cx.run_until_parked();
    let (source, target) = cx
        .update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let source = w.find("chat-tab-Alpha-one").bounds();
            let target = w.find("chat-tab-Alpha-three").bounds();
            w.pointer_down(source.center(), MouseButton::Left, cx);
            w.pointer_move(
                source.center() + point(px(8.), px(0.)),
                Some(MouseButton::Left),
                cx,
            );
            w.pointer_move(target.center(), Some(MouseButton::Left), cx);
            (source, target)
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(50));
    let middle = cx
        .update_window(window, |_, w, cx| {
            w.simulate_next_frame(cx);
            w.render_frame(cx);
            assert_eq!(shark.read(cx).active_ws().unwrap().tab_order, original);
            assert_eq!(State::load().workspaces[0].tab_order, original);
            let middle = w.find("chat-tab-Alpha-three").bounds();
            assert!(
                middle.left() < target.left(),
                "neighbor moves before release"
            );
            assert_eq!(middle.top(), target.top(), "tab animation stays horizontal");
            middle
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
        let settled = w.find("chat-tab-Alpha-three").bounds();
        assert!(
            settled.left() < middle.left(),
            "neighbor slides over several frames"
        );
        // Repeated events at the same coordinate must not flip the destination back.
        w.pointer_move(target.center(), Some(MouseButton::Left), cx);
        let outside = point(px(1600.), px(600.));
        w.pointer_move(outside, Some(MouseButton::Left), cx);
        let preview = w.find("reorder-preview").bounds();
        assert_eq!(
            preview.top(),
            source.top(),
            "preview locks the vertical axis"
        );
        assert_inside(preview, shark.read(cx).tab_scroll.bounds());
        w.pointer_up(outside, MouseButton::Left, cx);
        assert_eq!(
            shark.read(cx).active_ws().unwrap().tab_order,
            original,
            "release outside cancels"
        );
        assert_eq!(State::load().workspaces[0].tab_order, original);
    })
    .unwrap();
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.pointer_down(source.center(), MouseButton::Left, cx);
        w.pointer_move(
            source.center() + point(px(8.), px(0.)),
            Some(MouseButton::Left),
            cx,
        );
        w.pointer_move(target.center(), Some(MouseButton::Left), cx);
        let neighbor = w.find("chat-tab-Alpha-three").bounds();
        assert!(
            neighbor.left() < target.left(),
            "reduced motion updates without advancing time"
        );
        assert_eq!(neighbor.top(), target.top());
        w.pointer_move(target.center(), Some(MouseButton::Left), cx);
        assert_eq!(
            w.find("chat-tab-Alpha-three").bounds(),
            neighbor,
            "stationary pointer keeps the destination"
        );
        w.pointer_up(target.center(), MouseButton::Left, cx);
        let mut expected = original.clone();
        move_item(&mut expected, 0, 2);
        assert_eq!(shark.read(cx).active_ws().unwrap().tab_order, expected);
        assert_eq!(State::load().workspaces[0].tab_order, expected);
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.find("chat-tab-Alpha-one").bounds().center();
        let target = w.find("chat-tab-Alpha-two").bounds().center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(-8.), px(0.)), Some(MouseButton::Left), cx);
        shark.update(cx, |s, cx| s.close_chat(w, cx));
        let remaining = shark.read(cx).active_ws().unwrap().tab_order.clone();
        assert!(!remaining.contains(&TabId::Chat("Alpha-one".into())));
        w.pointer_move(target, Some(MouseButton::Left), cx);
        w.pointer_up(target, MouseButton::Left, cx);
        assert_eq!(
            shark.read(cx).active_ws().unwrap().tab_order,
            remaining,
            "closed drag source cannot reorder remaining tabs"
        );
        assert_eq!(State::load().workspaces[0].tab_order, remaining);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn sidebar_drag_is_vertical_and_preserves_groups(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let state = fixture();
    let original = state.sections.clone();
    let (window, shark) = boot(cx, state, options());
    cx.run_until_parked();
    let (source, target) = cx
        .update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let source = w.within("sidebar").find(("section", 0usize)).bounds();
            let target = w.within("sidebar").find(("section", 2usize)).bounds();
            w.pointer_down(source.center(), MouseButton::Left, cx);
            w.pointer_move(
                source.center() + point(px(0.), px(8.)),
                Some(MouseButton::Left),
                cx,
            );
            // Crossing more than one header must keep the original dragged section.
            let second = w
                .within("sidebar")
                .find(("section", 1usize))
                .bounds()
                .center();
            w.pointer_move(second, Some(MouseButton::Left), cx);
            w.pointer_move(target.center(), Some(MouseButton::Left), cx);
            (source, target)
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(50));
    let middle = cx
        .update_window(window, |_, w, cx| {
            w.simulate_next_frame(cx);
            w.render_frame(cx);
            assert_eq!(shark.read(cx).state.sections, original);
            assert_eq!(State::load().sections, original);
            let middle = w.within("sidebar").find(("section", 2usize)).bounds();
            assert!(middle.top() < target.top(), "section moves before release");
            assert_eq!(
                middle.left(),
                target.left(),
                "section animation stays vertical"
            );
            middle
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
        assert!(w.within("sidebar").find(("section", 2usize)).bounds().top() < middle.top());
        let outside = point(px(900.), px(1100.));
        w.pointer_move(outside, Some(MouseButton::Left), cx);
        let preview = w.find("reorder-preview").bounds();
        assert_eq!(
            preview.left(),
            source.left(),
            "preview locks the horizontal axis"
        );
        assert_inside(preview, shark.read(cx).sidebar_scroll.bounds());
        w.pointer_move(target.center(), Some(MouseButton::Left), cx);
        w.pointer_up(target.center(), MouseButton::Left, cx);
        let mut expected = original.clone();
        move_item(&mut expected, 0, 2);
        assert_eq!(shark.read(cx).state.sections, expected);
        assert_eq!(State::load().sections, expected);
    })
    .unwrap();
    // Moving into a collapsed section keeps it collapsed and preserves the other members.
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.sections[0].collapsed = true;
        cx.notify();
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.within("sidebar").find(0usize).bounds().center();
        let target = w
            .within("sidebar")
            .find(("section", 0usize))
            .bounds()
            .center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(0.), px(-8.)), Some(MouseButton::Left), cx);
        w.pointer_move(target, Some(MouseButton::Left), cx);
        assert_eq!(shark.read(cx).section_of("Alpha"), Some(2));
        w.pointer_up(target, MouseButton::Left, cx);
        assert_eq!(
            shark.read(cx).state.sections[0].workspaces,
            ["Beta", "Alpha"]
        );
        assert!(shark.read(cx).state.sections[0].collapsed);
        assert!(shark.read(cx).state.sections[2].workspaces.is_empty());
        assert_eq!(State::load().sections, shark.read(cx).state.sections);
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.sections[0].collapsed = false;
        cx.notify();
    });
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.within("sidebar").find(1usize).bounds().center();
        let neighbor = w.within("sidebar").find(0usize).bounds();
        let header = w
            .within("sidebar")
            .find(("section", 0usize))
            .bounds()
            .center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(0.), px(-8.)), Some(MouseButton::Left), cx);
        w.pointer_move(header, Some(MouseButton::Left), cx);
        assert_eq!(
            w.within("sidebar").find(0usize).bounds(),
            neighbor,
            "own section header is a live-preview no-op"
        );
        w.pointer_up(header, MouseButton::Left, cx);
        assert_eq!(
            shark.read(cx).state.sections[0].workspaces,
            ["Beta", "Alpha"]
        );
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.within("sidebar").find(0usize).bounds().center();
        let target = w.find("ws-header").bounds().center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(0.), px(-8.)), Some(MouseButton::Left), cx);
        w.pointer_move(target, Some(MouseButton::Left), cx);
        w.pointer_up(target, MouseButton::Left, cx);
        assert_eq!(shark.read(cx).section_of("Alpha"), None);
        assert_eq!(shark.read(cx).state.sections[0].workspaces, ["Beta"]);
        assert_eq!(State::load().sections, shark.read(cx).state.sections);
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        s.state.sections[0].name =
            "A very long section name that must stay within the narrow rail".into();
        s.toggle_sidebar(cx);
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.within("sidebar").find(("section", 0usize)).bounds();
        w.pointer_down(source.center(), MouseButton::Left, cx);
        w.pointer_move(
            source.center() + point(px(0.), px(8.)),
            Some(MouseButton::Left),
            cx,
        );
        for outside in [point(px(-100.), px(-100.)), point(px(1000.), px(1100.))] {
            w.pointer_move(outside, Some(MouseButton::Left), cx);
            let preview = w.find("reorder-preview").bounds();
            assert_eq!(preview.left(), source.left());
            assert_inside(preview, shark.read(cx).sidebar_scroll.bounds());
        }
        w.pointer_up(point(px(1000.), px(1100.)), MouseButton::Left, cx);
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.within("sidebar").find(0usize).bounds().center();
        let target = w.within("sidebar").find(1usize).bounds().center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(0.), px(-8.)), Some(MouseButton::Left), cx);
        w.pointer_move(target, Some(MouseButton::Left), cx);
        let preview_neighbor = w.within("sidebar").find(1usize).bounds();
        w.pointer_up(target, MouseButton::Left, cx);
        let beta = shark
            .read(cx)
            .state
            .workspaces
            .iter()
            .position(|ws| ws.name == "Beta")
            .unwrap();
        assert_eq!(
            w.within("sidebar").find(beta).bounds(),
            preview_neighbor,
            "last rail row has the same spacing during preview and after release"
        );
    })
    .unwrap();
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        let mut state = fixture();
        state.appearance.mode = mode;
        state.save().unwrap();
        let (window, _) = cx
            .update(|cx| {
                gpui_kit::open_window(
                    WindowOptions {
                        focus: false,
                        show: false,
                        ..options()
                    },
                    cx,
                    |w, cx| cx.new(|cx| Shark::new(w, cx)),
                )
            })
            .unwrap();
        cx.run_until_parked();
        for sidebar in [false, true] {
            let end = cx
                .update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                    let (start, end) = if sidebar {
                        (
                            w.within("sidebar")
                                .find(("section", 0usize))
                                .bounds()
                                .center(),
                            w.within("sidebar")
                                .find(("section", 2usize))
                                .bounds()
                                .center(),
                        )
                    } else {
                        (
                            w.find("chat-tab-Alpha-one").bounds().center(),
                            w.find("chat-tab-Alpha-three").bounds().center(),
                        )
                    };
                    w.pointer_down(start, MouseButton::Left, cx);
                    w.pointer_move(
                        start
                            + if sidebar {
                                point(px(0.), px(8.))
                            } else {
                                point(px(8.), px(0.))
                            },
                        Some(MouseButton::Left),
                        cx,
                    );
                    w.pointer_move(end, Some(MouseButton::Left), cx);
                    end
                })
                .unwrap();
            cx.advance_clock(Duration::from_millis(50));
            cx.update_window(window, |_, w, cx| {
                w.simulate_next_frame(cx);
                w.render_frame(cx);
            })
            .unwrap();
            let screenshot = cx.capture_screenshot(window).expect("real drag renderer");
            if let Some(dir) = std::env::var_os("SHARK_RENDER_DIR") {
                let dir = PathBuf::from(dir);
                fs::create_dir_all(&dir).unwrap();
                screenshot
                    .save(dir.join(format!(
                        "drag-{name}-{}.png",
                        if sidebar { "sidebar" } else { "tabs" }
                    )))
                    .unwrap();
            }
            cx.update_window(window, |_, w, cx| w.pointer_up(end, MouseButton::Left, cx))
                .unwrap();
        }
        cx.update_window(window, |_, w, _| w.remove_window())
            .unwrap();
    }
}
