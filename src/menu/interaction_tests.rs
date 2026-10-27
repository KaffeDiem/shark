use super::*;
use gpui_kit::test::{TestEventExt, TestWindowExt};

struct Fixture {
    menu: Option<Entity<Menu>>,
    focus: FocusHandle,
    calls: Rc<Cell<usize>>,
    underneath: usize,
    _dismiss: Subscription,
}

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .child(
                div()
                    .id("underneath")
                    .absolute()
                    .bottom_0()
                    .right_0()
                    .size(px(40.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.underneath += 1;
                        cx.notify();
                    })),
            )
            .children(self.menu.clone().map(layer))
    }
}

fn fixture(
    cx: &mut TestAppContext,
    width: f32,
    height: f32,
    x: f32,
    tall: bool,
) -> (AnyWindowHandle, Entity<Fixture>, Entity<Menu>) {
    cx.update(gpui_kit::init);
    let (window, fixture) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(width), px(height)),
                    ))),
                    ..Default::default()
                },
                cx,
                |w, cx| {
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        focus.focus(w, cx);
                        let calls = Rc::new(Cell::new(0));
                        let count = calls.clone();
                        let mut entries = vec![
                            item(
                                IconName::GitBranch,
                                "Create workspace from branch…",
                                move |_, _| count.set(count.get() + 1),
                            )
                            .hint("origin/a-very-long-branch-name-that-must-yield-to-the-action"),
                            Entry::Sep,
                            sub(
                                IconName::Folder,
                                "Targets",
                                std::iter::once(item(None, "Alpha", |_, _| {}))
                                    .chain([Entry::Sep, item(None, "Beta", |_, _| {})])
                                    .chain(
                                        (0..30).map(|i| {
                                            item(None, format!("Child {i:02}"), |_, _| {})
                                        }),
                                    )
                                    .collect(),
                            ),
                            item(None, "Checked", |_, _| {}).checked(true),
                        ];
                        if tall {
                            entries.extend(
                                (0..40).map(|i| item(None, format!("Item {i:02}"), |_, _| {})),
                            );
                            entries.push(sub(
                                IconName::Folder,
                                "Zebra targets",
                                vec![item(None, "Child", |_, _| {})],
                            ));
                        }
                        let menu = open(entries, point(px(x), px(20.)), w, cx);
                        let dismiss =
                            cx.subscribe(&menu, |this: &mut Fixture, _, _: &DismissEvent, cx| {
                                this.menu = None;
                                cx.notify();
                            });
                        Fixture {
                            menu: Some(menu),
                            focus,
                            calls,
                            underneath: 0,
                            _dismiss: dismiss,
                        }
                    })
                },
            )
        })
        .unwrap();
    cx.run_until_parked();
    let menu = cx.update(|cx| fixture.read(cx).menu.clone().unwrap());
    cx.update_window(window, |_, w, cx| w.render_frame(cx))
        .unwrap();
    (window, fixture, menu)
}

#[gpui_kit::test]
fn placement_and_columns(cx: &mut TestAppContext) {
    for (width, x, left) in [
        (1200., 20., false),
        (1200., 1000., true),
        (900., 420., true),
    ] {
        let (window, _, menu) = fixture(cx, width, 400., x, false);
        cx.update_window(window, |_, w, cx| {
            let parent = w.find("menu-parent").bounds();
            assert!(parent.left() >= px(8.) && parent.right() <= px(width - 8.));
            let label = w.find(("menu-label", 0usize)).bounds();
            let hint = w.find(("menu-hint", 0usize)).bounds();
            let icon = w.find(("menu-icon", 0usize)).bounds();
            let control = w.find(("menu-control", 0usize)).bounds();
            assert!(icon.right() + px(8.) <= label.left());
            assert!(label.right() + px(8.) <= hint.left());
            assert!(hint.right() + px(8.) <= control.left());
            w.press("t", cx);
            w.press("right", cx);
            let child = w.find("menu-child").bounds();
            assert!(!menu.read(cx).replacement);
            if left {
                assert!(child.right() + px(4.) <= parent.left());
            } else {
                assert!(parent.right() + px(4.) <= child.left());
            }
            assert!(child.left() >= px(8.) && child.right() <= px(width - 8.));
            assert!(child.top() >= px(8.) && child.bottom() <= px(392.));
            assert_eq!(
                w.within("menu-child").find(0usize).bounds().size.height,
                ROW_H
            );
            w.press("up", cx);
            assert!(menu.read(cx).sub_scroll.offset().y < px(0.));
            let last = w.within("menu-child").find(32usize).bounds();
            assert!(last.top() >= child.top() && last.bottom() <= child.bottom());
            w.press("b", cx);
            assert_eq!(menu.read(cx).sub_sel, Some(2));
            w.press("left", cx);
            assert!(menu.read(cx).sub.is_none());
            assert_eq!(menu.read(cx).sel, Some(2));
            let sub_row = w.find(2usize).bounds();
            let sibling = w.find(3usize).bounds();
            w.pointer_move(sub_row.center(), None, cx);
            assert!(menu.read(cx).sub.is_some());
            let toward = if left { px(-8.) } else { px(8.) };
            w.pointer_move(
                point(sub_row.center().x + toward, sibling.center().y),
                None,
                cx,
            );
            assert_eq!(
                menu.read(cx).sub,
                Some(2),
                "protect movement toward the actual child side"
            );
            w.pointer_move(
                point(sub_row.center().x - toward, sibling.center().y),
                None,
                cx,
            );
            assert!(menu.read(cx).sub.is_none());
            assert_eq!(menu.read(cx).sel, Some(3));
            w.remove_window();
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn replacement_scroll_and_back(cx: &mut TestAppContext) {
    let (window, _, menu) = fixture(cx, 360., 240., 20., true);
    cx.update_window(window, |_, w, cx| {
        w.press("z", cx);
        assert!(menu.read(cx).scroll.offset().y < px(0.));
        let offset = menu.read(cx).scroll.offset();
        let row = w.find(44usize).bounds();
        assert!(row.bottom() <= px(232.));
        w.hover(44usize, cx);
        assert!(
            menu.read(cx).sub.is_none(),
            "hover never replaces the parent"
        );
        w.press("right", cx);
        assert!(menu.read(cx).replacement);
        assert!(w.try_find("menu-parent").is_none());
        assert!(w.find("menu-back").visible());
        let child = w.find("menu-child").bounds();
        assert!(
            child.left() >= px(8.) && child.right() <= px(352.),
            "replacement bounds {child:?}"
        );
        w.press("escape", cx);
        assert!(!menu.read(cx).replacement);
        assert_eq!(menu.read(cx).sel, Some(44));
        assert_eq!(menu.read(cx).scroll.offset(), offset);
        w.click(44usize, cx);
        assert!(menu.read(cx).replacement);
        w.click("menu-back", cx);
        assert_eq!(menu.read(cx).scroll.offset(), offset);
        w.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn keyboard_action_focus_and_outside_click(cx: &mut TestAppContext) {
    let (window, fixture, menu) = fixture(cx, 1000., 400., 20., false);
    cx.update_window(window, |_, w, cx| {
        w.press("down", cx);
        assert_eq!(menu.read(cx).sel, Some(0));
        w.press("down", cx);
        assert_eq!(menu.read(cx).sel, Some(2), "skip separator");
        w.press("c", cx);
        assert_eq!(menu.read(cx).sel, Some(3));
        w.press("c", cx);
        assert_eq!(
            menu.read(cx).sel,
            Some(0),
            "repeated letter cycles matching rows"
        );
        w.press("c", cx);
        assert_eq!(menu.read(cx).sel, Some(3));
        w.press("r", cx);
        assert_eq!(menu.read(cx).sel, Some(0), "typeahead narrows from c to cr");
        w.press("enter", cx);
        assert!(menu.read(cx).closed);
        assert!(fixture.read(cx).focus.is_focused(w));
        assert_eq!(fixture.read(cx).calls.get(), 1);
        menu.update(cx, |m, cx| m.activate(0, false, w, cx));
        assert_eq!(
            fixture.read(cx).calls.get(),
            1,
            "dismissed action cannot execute twice"
        );
        w.remove_window();
    })
    .unwrap();
    let (window, fixture, _) = self::fixture(cx, 1000., 400., 20., false);
    cx.update_window(window, |_, w, cx| {
        let point = point(px(980.), px(380.));
        w.pointer_down(point, MouseButton::Left, cx);
        w.pointer_up(point, MouseButton::Left, cx);
        assert_eq!(fixture.read(cx).underneath, 0);
        assert!(fixture.read(cx).focus.is_focused(w));
        w.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(fixture.read(cx).menu.is_none()));
}

#[gpui_kit::test]
fn scrolling_parent_closes_child(cx: &mut TestAppContext) {
    let (window, _, menu) = fixture(cx, 1200., 240., 20., true);
    cx.update_window(window, |_, w, cx| {
        w.press("t", cx);
        w.press("right", cx);
        assert!(menu.read(cx).sub.is_some());
        w.scroll(
            "menu-parent-scroll",
            ScrollDelta::Pixels(point(px(0.), px(-80.))),
            cx,
        );
        assert!(menu.read(cx).sub.is_none());
        assert!(menu.read(cx).scroll.offset().y < px(0.));
        w.remove_window();
    })
    .unwrap();
}

#[gpui_kit::test]
fn replacement_child_scroll_stays_inside_window(cx: &mut TestAppContext) {
    let (window, _, menu) = fixture(cx, 360., 240., 20., false);
    cx.update_window(window, |_, w, cx| {
        w.press("t", cx);
        w.press("right", cx);
        assert!(menu.read(cx).replacement);
        w.press("up", cx);
        let child = w.find("menu-child").bounds();
        let last = w.within("menu-child").find(32usize).bounds();
        assert!(menu.read(cx).sub_scroll.offset().y < px(0.));
        assert!(child.left() >= px(8.) && child.right() <= px(352.));
        assert!(child.top() >= px(8.) && child.bottom() <= px(232.));
        assert!(last.top() >= child.top() + ROW_H && last.bottom() <= child.bottom());
        assert_eq!(last.size.height, ROW_H);
        w.remove_window();
    })
    .unwrap();
}
