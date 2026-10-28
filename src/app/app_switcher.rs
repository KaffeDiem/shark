//! Hold Cmd and tap O to choose an app; releasing Cmd opens the workspace there.
use super::*;
use crate::apps::OpenApp;

pub(super) struct AppSwitcher {
    pub(super) apps: Vec<&'static OpenApp>,
    pub(super) selected: usize,
    workspace: String,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    command_held: bool,
    scroll: ScrollHandle,
    width: Rc<std::cell::Cell<Pixels>>,
    _keys: Subscription,
    _blur: Subscription,
}

impl Shark {
    pub(super) fn cycle_open_app(
        &mut self,
        backwards: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(s) = &mut self.app_switcher {
            let n = s.apps.len();
            s.selected = (s.selected + if backwards { n - 1 } else { 1 }) % n;
            s.scroll.scroll_to_item(s.selected);
        } else {
            let Some(target) = self.target(window) else {
                return notify_err(window, cx, "Open a workspace first");
            };
            let apps = palette::available_apps(Some(&target));
            if apps.is_empty() {
                return notify_err(
                    window,
                    cx,
                    "No applications available to open this workspace",
                );
            }
            let previous_focus = window.focused(cx);
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            let this = cx.entity().downgrade();
            // Key bindings dispatch before raw key events; intercept while this overlay owns focus.
            let keys = cx.intercept_keystrokes(move |ev, w, cx| {
                // GPUI turns a bare Command release into a keystroke before dispatching modifiers.
                if ev.keystroke.key == "platform" {
                    return;
                }
                this.update(cx, |s, cx| {
                    if s.app_switcher
                        .as_ref()
                        .is_some_and(|s| s.focus.is_focused(w))
                    {
                        s.app_switcher_key(&ev.keystroke, w, cx);
                        cx.stop_propagation();
                    }
                })
                .ok();
            });
            let selected = if backwards { apps.len() - 1 } else { 0 };
            let blur = cx.on_blur(&focus, window, |s, _, cx| {
                s.app_switcher = None;
                cx.notify();
            });
            let scroll = ScrollHandle::new();
            // The scroll handle learns its overflow after the first layout; start at the end.
            if backwards {
                scroll.set_offset(point(-px(apps.len() as f32 * 96.), px(0.)));
            }
            self.app_switcher = Some(AppSwitcher {
                apps,
                selected,
                workspace: target.name,
                focus,
                previous_focus,
                command_held: window.modifiers().platform,
                scroll,
                width: Rc::default(),
                _keys: keys,
                _blur: blur,
            });
        }
        cx.notify();
    }

    fn app_switcher_key(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        match key.key.as_str() {
            "o" if key.modifiers.platform => self.cycle_open_app(key.modifiers.shift, window, cx),
            "left" => self.cycle_open_app(true, window, cx),
            "right" => self.cycle_open_app(false, window, cx),
            "escape" => self.close_app_switcher(false, window, cx),
            "enter" => self.close_app_switcher(true, window, cx),
            _ => {}
        }
    }

    pub(super) fn app_switcher_modifiers(
        &mut self,
        command_held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = &mut self.app_switcher else {
            return;
        };
        let released = s.command_held && !command_held;
        s.command_held = command_held;
        if released {
            self.close_app_switcher(true, window, cx);
        }
    }

    pub(super) fn close_app_switcher(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self.app_switcher.take() else {
            return;
        };
        // Blur notifications can arrive after a modifier event; never launch or reclaim lost focus.
        if !s.focus.is_focused(window) {
            cx.notify();
            return;
        }
        // Restore focus before the launch path can open a repo/project picker.
        if let Some(focus) = s.previous_focus {
            focus.focus(window, cx);
        } else {
            self.focus_fallback(window, cx);
        }
        if open {
            match self.ws(&s.workspace).cloned() {
                Some(ws) => self.open_in_workspace(s.apps[s.selected], &ws, window, cx),
                None => notify_err(window, cx, "The selected workspace is no longer available"),
            }
        }
        cx.notify();
    }

    pub(super) fn render_app_switcher(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let s = self.app_switcher.as_ref()?;
        let theme = cx.theme();
        let width = px((s.apps.len() as f32 * 96. + 40.).min(760.))
            .min((window.viewport_size().width - px(32.)).max(px(0.)));
        let (layout, scroll, selected) = (s.width.clone(), s.scroll.clone(), s.selected);
        let hint = if s.command_held {
            "Release ⌘ to open · ⇧O reverse · Esc cancel"
        } else {
            "Enter to open · ⌘O cycle · Esc cancel"
        };
        let panel = menu::chrome(theme)
            .id("app-switcher")
            .role(Role::Dialog)
            .aria_label("Open workspace in application")
            .w(width)
            .p(px(20.))
            .gap_3()
            .rounded(px(24.))
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .on_children_prepainted(move |_, window, cx| {
                        let width = scroll.bounds().size.width;
                        if layout.replace(width) != width {
                            let scroll = scroll.clone();
                            window.defer(cx, move |window, _| {
                                scroll.scroll_to_item(selected);
                                window.refresh();
                            });
                        }
                    })
                    .id("app-switcher-apps")
                    .role(Role::ListBox)
                    .aria_label("Applications")
                    .aria_description(hint)
                    .track_focus(&s.focus)
                    .w_full()
                    .overflow_x_scroll()
                    .track_scroll(&s.scroll)
                    .children(s.apps.iter().enumerate().map(|(ix, app)| {
                        let selected = ix == s.selected;
                        v_flex()
                            .id(format!("app-switcher-app-{ix}"))
                            .role(Role::ListBoxOption)
                            .aria_label(app.name.clone())
                            .aria_selected(selected)
                            .when(selected, |d| d.aria_active_descendant())
                            .size(px(96.))
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded(px(16.))
                            .border_2()
                            .border_color(if selected {
                                theme.primary
                            } else {
                                theme.transparent
                            })
                            .when(selected, |d| d.bg(side_on(theme)))
                            .hover(|d| d.bg(side_hover(theme)))
                            .child(match &app.icon {
                                Some(icon) => img(icon.clone()).size(px(72.)).into_any_element(),
                                None => Icon::new(crate::ui::app_icon(app))
                                    .with_size(px(64.))
                                    .into_any_element(),
                            })
                            .on_click(cx.listener(move |this, _, w, cx| {
                                if let Some(s) = &mut this.app_switcher {
                                    s.selected = ix;
                                }
                                this.close_app_switcher(true, w, cx);
                            }))
                            .test_support()
                    })),
            )
            .child(
                div()
                    .text_center()
                    .font_weight(FontWeight::MEDIUM)
                    .child(s.apps[s.selected].name.clone()),
            )
            .child(
                div()
                    .text_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(hint),
            )
            .test_support();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("app-switcher-overlay")
                        .occlude()
                        .w(window.viewport_size().width)
                        .h(window.viewport_size().height)
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_any_mouse_down(cx.listener(|this, _, w, cx| {
                            cx.stop_propagation();
                            this.close_app_switcher(false, w, cx);
                        }))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(panel),
                ),
            )
            .with_priority(100)
            .into_any_element(),
        )
    }
}
