use super::*;
use gpui_kit::test::TestEventExt;

fn fixture() -> State {
    let mut state = State::default();
    for name in ["Alpha", "Beta", "Gamma"] {
        let mut ws = Task {
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
        state.tasks.push(ws);
        state.sections.push(Section {
            agent: None,
            name: format!("{name} section"),
            icon: "folder".into(),
            collapsed: false,
            tasks: vec![name.into()],
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
    let original = state.tasks[0].tab_order.clone();
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
            assert_eq!(shark.read(cx).active_task().unwrap().tab_order, original);
            assert_eq!(State::load().tasks[0].tab_order, original);
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
            shark.read(cx).active_task().unwrap().tab_order,
            original,
            "release outside cancels"
        );
        assert_eq!(State::load().tasks[0].tab_order, original);
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
        assert_eq!(shark.read(cx).active_task().unwrap().tab_order, expected);
        assert_eq!(State::load().tasks[0].tab_order, expected);
    })
    .unwrap();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.find("chat-tab-Alpha-one").bounds().center();
        let target = w.find("chat-tab-Alpha-two").bounds().center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(-8.), px(0.)), Some(MouseButton::Left), cx);
        shark.update(cx, |s, cx| s.close_chat(w, cx));
        let remaining = shark.read(cx).active_task().unwrap().tab_order.clone();
        assert!(!remaining.contains(&TabId::Chat("Alpha-one".into())));
        w.pointer_move(target, Some(MouseButton::Left), cx);
        w.pointer_up(target, MouseButton::Left, cx);
        assert_eq!(
            shark.read(cx).active_task().unwrap().tab_order,
            remaining,
            "closed drag source cannot reorder remaining tabs"
        );
        assert_eq!(State::load().tasks[0].tab_order, remaining);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn sidebar_drag_reorders_only_within_agent_and_group(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = fixture();
    state.sections.clear();
    state.agents = ["designer", "developer", "empty"]
        .map(|slug| crate::state::Agent {
            slug: slug.into(),
            name: slug.into(),
            icon: "bot".into(),
            ..Default::default()
        })
        .to_vec();
    for (i, task) in state.tasks.iter_mut().enumerate() {
        let slug = if i < 2 { "designer" } else { "developer" };
        task.agent = Some(crate::state::AgentLink {
            slug: slug.into(),
            name: slug.into(),
            instructions: String::new(),
            siblings: vec![],
        });
    }
    state.tasks.push(Task {
        name: "Oneoff".into(),
        ..Default::default()
    });
    let (window, shark) = boot(cx, state, options());
    cx.run_until_parked();
    cx.update(|cx| {
        let s = shark.read(cx);
        assert_eq!(
            s.side_rows(false),
            vec![
                SideRow::Agent(0),
                SideRow::Task(0),
                SideRow::Task(1),
                SideRow::Agent(1),
                SideRow::Task(2),
                SideRow::Agent(2),
                SideRow::Task(3)
            ]
        );
    });
    cx.update(|cx| cx.set_reduce_motion(true));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let source = w.within("sidebar").find(0usize).bounds();
        let target = w.within("sidebar").find(1usize).bounds();
        let next_agent = w.within("sidebar").find(("agent", 1usize)).bounds();
        w.pointer_down(source.center(), MouseButton::Left, cx);
        w.pointer_move(
            source.center() + point(px(0.), px(8.)),
            Some(MouseButton::Left),
            cx,
        );
        w.pointer_move(target.center(), Some(MouseButton::Left), cx);
        let preview = w.find("reorder-preview").bounds();
        assert_eq!(preview.left(), source.left());
        assert_eq!(
            w.within("sidebar").find(1usize).bounds(),
            source,
            "last agent task moves into the first task slot"
        );
        assert_eq!(
            w.within("sidebar").find(("agent", 1usize)).bounds(),
            next_agent,
            "next agent header stays still"
        );
        assert_inside(preview, shark.read(cx).sidebar_scroll.bounds());
        assert_eq!(
            shark.read(cx).state.tasks[0].name,
            "Alpha",
            "only release persists order"
        );
        w.pointer_up(target.center(), MouseButton::Left, cx);
        assert_eq!(shark.read(cx).state.tasks[0].name, "Beta");
        assert_eq!(State::load().tasks[0].name, "Beta");
        // Crossing into another agent is not an assignment or a reorder.
        w.render_frame(cx);
        let source = w.within("sidebar").find(1usize).bounds().center();
        let target = w.within("sidebar").find(2usize).bounds().center();
        w.pointer_down(source, MouseButton::Left, cx);
        w.pointer_move(source + point(px(0.), px(8.)), Some(MouseButton::Left), cx);
        w.pointer_move(target, Some(MouseButton::Left), cx);
        w.pointer_up(target, MouseButton::Left, cx);
        assert_eq!(shark.read(cx).state.tasks[1].name, "Alpha");
        assert_eq!(
            shark.read(cx).state.tasks[1].agent.as_ref().unwrap().slug,
            "designer"
        );
    })
    .unwrap();
    on_app(window, shark.clone())(cx, &|s, _, cx| {
        // A legacy section binding cannot assign the unrelated task to the empty agent.
        s.state.sections.push(Section {
            agent: Some("empty".into()),
            name: "Legacy".into(),
            tasks: vec!["Oneoff".into()],
            icon: "folder".into(),
            collapsed: false,
        });
        s.sel = SideRow::Task(1);
        s.set_agent_collapsed(0, true, cx);
        assert_eq!(s.sel, SideRow::Agent(0));
        assert_eq!(s.ws_order(false), vec![2, 3]);
        assert_eq!(s.ws_order(true), vec![0, 1, 2, 3]);
        assert!(!s.can_reorder_tasks("Alpha", "Gamma"));
        s.state.sections.push(Section {
            name: "Manual".into(),
            tasks: vec!["Alpha".into()],
            agent: None,
            icon: "folder".into(),
            collapsed: false,
        });
        assert!(!s.can_reorder_tasks("Alpha", "Beta"));
    });
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.within("sidebar").find(("agent", 2usize));
        w.within("sidebar").find(3usize);
        w.within("sidebar").click(("collapse-agent", 0usize), cx);
        assert!(!shark.read(cx).collapsed_agents.contains("designer"));
        assert!(!matches!(&shark.read(cx).home.destination, home::Destination::Agent(slug) if slug == "designer"),
            "chevron does not navigate");
        w.within("sidebar").click(("agent", 0usize), cx);
        assert!(matches!(&shark.read(cx).home.destination, home::Destination::Agent(slug) if slug == "designer"));
    }).unwrap();
    on_app(window, shark.clone())(cx, &|s, w, cx| {
        s.state.sections.push(Section {
            name: "Other group".into(),
            tasks: vec!["Gamma".into()],
            agent: None,
            icon: "folder".into(),
            collapsed: false,
        });
        s.active = Some("Gamma".into());
        s.enter_home(w, cx);
        s.home.sel = Some("Alpha".into());
        let mut target = None;
        s.with_section(w, cx, |_, i, _, _| target = Some(i));
        assert_eq!(
            target,
            Some(1),
            "group actions follow the selected task, not hidden active task"
        );
    });
}

pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, name) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        let mut state = fixture();
        state.sections.clear();
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
                            w.within("sidebar").find(0usize).bounds().center(),
                            w.within("sidebar").find(2usize).bounds().center(),
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
