//! Quiet toolbar capsules, retaining the house button's interaction and accessibility.
use gpui_kit::base::StyledExt as _;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, Size};
use gpui_kit::*;
use std::time::Duration;

#[derive(Default)]
struct Interaction {
    hovered: bool,
    pressed: bool,
    #[cfg(test)]
    progress: f32,
}

#[derive(IntoElement)]
struct ToolbarButton {
    id: ElementId,
    button: Button,
}

/// `id` must match the button's id so keyboard focus shares the house button's state.
pub(crate) fn toolbar(id: impl Into<ElementId>, button: Button) -> impl IntoElement {
    ToolbarButton {
        id: id.into(),
        button,
    }
}

impl RenderOnce for ToolbarButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut button = self.button;
        let original = button.style().clone();
        let held = button.is_selected() || button.is_open();
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let interaction = window.use_keyed_state(
            ElementId::NamedChild(self.id.clone().into(), "toolbar-interaction".into()),
            cx,
            |_, _| Interaction::default(),
        );
        let state = interaction.read(cx);
        let immediate = held || state.pressed || focus.is_focused(window);
        let engaged = immediate || state.hovered;
        let progress = transition(
            (self.id, "toolbar-engagement"),
            if engaged { 1.0_f32 } else { 0.0 },
            Transition::new(if immediate {
                Duration::ZERO
            } else {
                Duration::from_millis(120)
            }),
            window,
            cx,
        );
        #[cfg(test)]
        interaction.update(cx, |state, _| state.progress = progress);
        let theme = cx.theme();
        let dark = theme.is_dark();
        let bright = theme.foreground;
        let muted = if dark {
            theme.background.blend(bright.alpha(0.61))
        } else {
            theme.muted_foreground
        };
        let foreground = muted.blend(bright.alpha(progress));
        let background = theme.background.blend(bright.alpha(if dark {
            0.045 + 0.045 * progress
        } else {
            0.025 + 0.035 * progress
        }));
        let active = theme
            .background
            .blend(bright.alpha(if dark { 0.09 } else { 0.06 }));
        let border = theme.background.blend(bright.alpha(0.14));

        // Rendering inline shares the focus key without adding a layout or accessibility wrapper.
        let button = button
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(border)
                    .foreground(foreground)
                    .hover(background)
                    .active(active),
            )
            // Custom variants tint their idle fill; set it explicitly to match the reference.
            .bg(background)
            .with_size(Size::Medium)
            .h(px(32.))
            .min_w(px(32.))
            .rounded(px(16.))
            .border_1()
            .border_color(border)
            .refine_style(&original);
        Observe {
            element: RenderOnce::render(button, window, cx).into_any_element(),
            interaction,
        }
    }
}

// The native tooltip owns on_hover; observe without replacing any button callbacks or layout.
struct Observe {
    element: AnyElement,
    interaction: Entity<Interaction>,
}

impl IntoElement for Observe {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Observe {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.element.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.element.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.element.paint(window, cx);
        let hovered = hitbox.is_hovered(window);
        if self.interaction.read(cx).hovered != hovered {
            self.interaction.update(cx, |state, _| {
                state.hovered = hovered;
                if !hovered {
                    state.pressed = false;
                }
            });
            window.refresh();
        }
        let state = self.interaction.clone();
        let move_hitbox = hitbox.clone();
        window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let hovered = move_hitbox.is_hovered(window);
            if state.read(cx).hovered != hovered {
                state.update(cx, |state, _| {
                    state.hovered = hovered;
                    if !hovered {
                        state.pressed = false;
                    }
                });
                window.refresh();
            }
        });
        let state = self.interaction.clone();
        let down_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture
                && event.button == MouseButton::Left
                && down_hitbox.is_hovered_at(event.position, window)
            {
                state.update(cx, |state, _| {
                    state.hovered = true;
                    state.pressed = true;
                });
                window.refresh();
            }
        });
        let state = self.interaction.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture
                && event.button == MouseButton::Left
                && state.read(cx).pressed
            {
                state.update(cx, |state, _| state.pressed = false);
                window.refresh();
            }
        });
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::{Interaction, ToolbarButton};
    use core::prelude::v1::test;
    use gpui_kit::assets::IconName;
    use gpui_kit::base::TestSupportExt as _;
    use gpui_kit::component::Disableable as _;
    use gpui_kit::component::Selectable as _;
    use gpui_kit::component::button::Button;
    use gpui_kit::test::{TestEventExt as _, TestWindowExt as _};
    use gpui_kit::{
        AnyWindowHandle, AppContext, Context, ElementId, Entity, InteractiveElement, IntoElement,
        MouseButton, ParentElement, Render, RenderOnce, Styled, TestAppContext, Window,
        WindowOptions, div, px, size,
    };
    use std::time::Duration;

    #[derive(Default)]
    struct Fixture {
        calls: usize,
        disabled: bool,
        selected: bool,
        open: bool,
        compact: bool,
        progress: f32,
    }

    impl Render for Fixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let mut button = Button::new("control")
                .icon(IconName::Plus)
                .accessibility_label("Add item")
                .tooltip("Add item")
                .disabled(self.disabled)
                .selected(self.selected)
                .open(self.open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.calls += 1;
                    cx.notify();
                }));
            if self.compact {
                button = button.size(px(24.)).min_w_0().rounded_tr(px(0.));
            }
            let element = RenderOnce::render(
                ToolbarButton {
                    id: "control".into(),
                    button,
                },
                window,
                cx,
            )
            .into_any_element();
            self.progress = window
                .use_keyed_state(
                    ElementId::NamedChild(
                        ElementId::from("control").into(),
                        "toolbar-interaction".into(),
                    ),
                    cx,
                    |_, _| Interaction::default(),
                )
                .read(cx)
                .progress;
            div()
                .size_full()
                .p(px(16.))
                .tab_group()
                .child(element)
                .child(div().id("away").size(px(80.)).test_support())
        }
    }

    fn fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Fixture>) {
        cx.update(gpui_kit::init);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                cx.new(|_| Fixture::default())
            })
            .unwrap()
        });
        let window = window.into();
        cx.update_window(window, |_, w, cx| w.render_frame(cx))
            .unwrap();
        (window, view)
    }

    #[gpui_kit::test]
    fn toolbar_preserves_activation_focus_disabled_and_explicit_geometry(cx: &mut TestAppContext) {
        let (window, view) = fixture(cx);
        cx.update_window(window, |_, w, cx| {
            assert_eq!(w.find("control").bounds().size, size(px(32.), px(32.)));
            assert_eq!(w.find("control").label(), Some("Add item"));
            w.click("control", cx);
            assert_eq!(view.read(cx).calls, 1);
            w.focus_next(cx);
            w.render_frame(cx);
            assert_eq!(w.find("control").focused(), Some(true));
            assert_eq!(view.read(cx).progress, 1.0);
            w.press("enter", cx);
            w.press("space", cx);
            assert_eq!(view.read(cx).calls, 3);
            view.update(cx, |v, cx| {
                v.disabled = true;
                v.compact = true;
                cx.notify();
            });
            w.render_frame(cx);
            assert_eq!(w.find("control").bounds().size, size(px(24.), px(24.)));
            assert_eq!(w.find("control").disabled(), Some(true));
            w.click("control", cx);
            w.press("enter", cx);
            w.press("space", cx);
            assert_eq!(view.read(cx).calls, 3);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn toolbar_hover_reverses_and_reduced_motion_resolves_immediately(cx: &mut TestAppContext) {
        let (window, view) = fixture(cx);
        cx.update_window(window, |_, w, cx| {
            assert_eq!(view.read(cx).progress, 0.0);
            w.hover("control", cx);
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_millis(40));
        let halfway = cx
            .update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let value = view.read(cx).progress;
                assert!(value > 0.0 && value < 1.0, "hover fades in: {value}");
                w.hover("away", cx);
                assert!(
                    (view.read(cx).progress - value).abs() < 0.001,
                    "reversal starts at the current value"
                );
                value
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            let value = view.read(cx).progress;
            assert!(value < halfway && value > 0.0, "hover fades back: {value}");
            cx.set_reduce_motion(true);
            w.hover("control", cx);
            assert_eq!(view.read(cx).progress, 1.0);
            w.hover("away", cx);
            assert_eq!(view.read(cx).progress, 0.0);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn toolbar_selected_open_and_pressed_states_brighten_without_waiting(cx: &mut TestAppContext) {
        let (window, view) = fixture(cx);
        cx.update_window(window, |_, w, cx| {
            for (selected, open) in [(true, false), (false, true)] {
                view.update(cx, |v, cx| {
                    v.selected = selected;
                    v.open = open;
                    cx.notify();
                });
                w.render_frame(cx);
                assert_eq!(view.read(cx).progress, 1.0);
            }
            view.update(cx, |v, cx| {
                v.open = false;
                cx.notify();
            });
            cx.set_reduce_motion(true);
            w.render_frame(cx);
            assert_eq!(view.read(cx).progress, 0.0);
            let center = w.find("control").bounds().center();
            w.pointer_down(center, MouseButton::Left, cx);
            assert_eq!(view.read(cx).progress, 1.0);
            let outside = w.find("away").bounds().center();
            w.pointer_move(outside, Some(MouseButton::Left), cx);
            w.pointer_up(outside, MouseButton::Left, cx);
            assert_eq!(view.read(cx).calls, 0, "cancelled press does not activate");
            assert_eq!(view.read(cx).progress, 0.0);
            view.update(cx, |v, cx| {
                v.disabled = true;
                cx.notify();
            });
            w.render_frame(cx);
            w.hover("control", cx);
            w.click("control", cx);
            assert_eq!(view.read(cx).calls, 0, "disabled controls remain inert");
            w.hover("away", cx);
            view.update(cx, |v, cx| {
                v.disabled = false;
                cx.notify();
            });
            w.render_frame(cx);
            assert_eq!(
                view.read(cx).progress,
                0.0,
                "re-enabling does not retain hover"
            );
            w.pointer_move(center, None, cx);
            w.pointer_down(center, MouseButton::Left, cx);
            view.update(cx, |v, cx| {
                v.disabled = true;
                cx.notify();
            });
            w.render_frame(cx);
            w.pointer_move(outside, Some(MouseButton::Left), cx);
            w.pointer_up(outside, MouseButton::Left, cx);
            view.update(cx, |v, cx| {
                v.disabled = false;
                cx.notify();
            });
            w.render_frame(cx);
            assert_eq!(
                view.read(cx).progress,
                0.0,
                "disabling during a press cannot leave it stuck"
            );
            assert_eq!(view.read(cx).calls, 0);
        })
        .unwrap();
    }
}
