use super::*;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::test::TestWindowExt as _;
use std::cell::Cell;

struct Fixture(FocusHandle);

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.0)
    }
}

#[gpui_kit::test]
fn dialogs_keep_spacing_viewport_bounds_and_focus(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    for mode in [Mode::Dark, Mode::Light] {
        for width in [360., 1000.] {
            let (window, fixture) = cx.update(|cx| {
                crate::theme::apply(
                    &crate::state::Appearance {
                        mode,
                        ..Default::default()
                    },
                    cx,
                );
                gpui_kit::open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            Point::default(),
                            size(px(width), px(400.)),
                        ))),
                        ..Default::default()
                    },
                    cx,
                    |w, cx| {
                        cx.new(|cx| {
                            let focus = cx.focus_handle();
                            focus.focus(w, cx);
                            Fixture(focus)
                        })
                    },
                )
                .unwrap()
            });
            cx.update_window(window, |_, w, cx| {
                w.open_dialog(cx, |d, _, _| {
                    dialog(d).title("Settings").w(px(640.)).child(
                        div()
                            .id("dialog-body")
                            .test_support()
                            .w_full()
                            .h(px(64.))
                            .child("Preferences"),
                    )
                });
                w.render_frame(cx);
                let surface = w.within("dialog").find(0usize).bounds();
                let body = w.find("dialog-body").bounds();
                assert!(surface.left() >= px(0.) && surface.right() <= px(width));
                assert!(surface.top() >= px(0.) && surface.bottom() <= px(400.));
                assert_eq!(body.left() - surface.left(), px(21.));
                assert_eq!(surface.right() - body.right(), px(21.));
                w.press("escape", cx);
                assert!(!w.has_active_dialog(cx));
                assert!(fixture.read(cx).0.is_focused(w));

                let confirmations = Rc::new(Cell::new(0));
                let count = confirmations.clone();
                w.open_alert_dialog(cx, move |d, _, _| {
                    let count = count.clone();
                    alert_dialog(d)
                        .title("Discard edits?")
                        .description("Your saved copy stays available.")
                        .show_cancel(true)
                        .on_ok(move |_, _, _| {
                            count.set(count.get() + 1);
                            true
                        })
                });
                w.render_frame(cx);
                w.press("escape", cx);
                assert!(!w.has_active_dialog(cx));
                assert_eq!(confirmations.get(), 0, "Escape must not confirm the action");
                assert!(fixture.read(cx).0.is_focused(w));
                w.remove_window();
            })
            .unwrap();
        }
    }
}
