//! Account usage belongs to the window, independent of the selected workspace.
use super::*;
use crate::usage::{Failure, Provider, Snapshot, Status};
use std::time::Duration;

pub(super) struct Panel {
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    pub(super) poll: Option<Task<()>>,
    _keys: Subscription,
}

impl Shark {
    pub(super) fn show_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &self.usage_panel {
            panel.focus.focus(window, cx);
            return;
        }
        let previous_focus = window.focused(cx);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let this = cx.entity().downgrade();
        let keys = cx.intercept_keystrokes(move |ev, w, cx| {
            this.update(cx, |s, cx| {
                let Some(panel) = &s.usage_panel else { return };
                if !panel.focus.contains_focused(w, cx) {
                    return;
                }
                match ev.keystroke.key.as_str() {
                    "escape" => s.close_usage(w, cx),
                    "enter" | "space" if panel.focus.is_focused(w) => s.refresh_usage(cx),
                    "tab" => {
                        if panel.focus.is_focused(w) {
                            w.focus_next(cx);
                        } else {
                            panel.focus.focus(w, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            })
            .ok();
        });
        self.usage_panel = Some(Panel {
            focus,
            previous_focus,
            poll: None,
            _keys: keys,
        });
        self.refresh_usage(cx);
        self.usage_polling(window, cx);
        cx.notify();
    }

    pub(super) fn close_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.usage_panel.take() else {
            return;
        };
        if let Some(focus) = panel.previous_focus {
            focus.focus(window, cx);
        } else {
            self.focus_fallback(window, cx);
        }
        cx.notify();
    }

    pub(super) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        for provider in Provider::ALL {
            if !self.usage[provider.index()].begin() {
                continue;
            }
            // Headless tests complete snapshots explicitly; never query the user's account.
            #[cfg(test)]
            {
                self.usage_queries[provider.index()] += 1;
                if !self.usage_live {
                    continue;
                }
            }
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { crate::usage::query(provider) })
                    .await;
                this.update(cx, |s, cx| {
                    s.usage[provider.index()].finish(result);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    pub(super) fn usage_polling(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &mut self.usage_panel else {
            return;
        };
        if !window.is_window_active() {
            panel.poll = None;
        } else if panel.poll.is_none() {
            panel.poll = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_secs(60))
                        .await;
                    let keep = this
                        .update_in(cx, |s, w, cx| {
                            if s.usage_panel.is_none() || !w.is_window_active() {
                                return false;
                            }
                            s.refresh_usage(cx);
                            true
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
            }));
        }
    }

    pub(super) fn render_usage_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let at = self.usage_at.clone();
        div()
            .relative()
            .flex_none()
            .mx(px(8.))
            .child(
                Button::new("show-usage")
                    .ghost()
                    .xsmall()
                    .label("Usage")
                    .accessibility_label("Show usage")
                    .on_click(cx.listener(|s, _, w, cx| {
                        if s.usage_panel.is_some() {
                            s.close_usage(w, cx);
                        } else {
                            s.show_usage(w, cx);
                        }
                    })),
            )
            .child(
                canvas(
                    move |b, w, cx| {
                        if at.replace(b) != b {
                            w.defer(cx, |w, _| w.refresh());
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    pub(super) fn render_usage(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let panel = self.usage_panel.as_ref()?;
        let viewport = window.viewport_size();
        let right = (viewport.width - self.usage_at.get().right()).max(px(8.));
        let content = menu::chrome(cx.theme())
            .id("usage-panel")
            .role(Role::Dialog)
            .aria_label("Account usage")
            .aria_description("Subscription allowances. Enter to refresh. Escape to close.")
            .track_focus(&panel.focus)
            .w(px(320.).min(viewport.width - px(16.)))
            .max_h(viewport.height - px(60.))
            .overflow_y_scroll()
            .p_4()
            .gap_4()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Usage"))
                    .child(
                        Button::new("refresh-usage")
                            .ghost()
                            .xsmall()
                            .label("Refresh")
                            .icon(IconName::RefreshCw)
                            .on_click(cx.listener(|s, _, _, cx| s.refresh_usage(cx))),
                    ),
            )
            .children(
                Provider::ALL.map(|provider| section(provider, &self.usage[provider.index()], cx)),
            )
            .test_support();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("usage-overlay")
                        .occlude()
                        .w(viewport.width)
                        .h(viewport.height)
                        .flex()
                        .justify_end()
                        .items_start()
                        .pt(px(48.))
                        .pr(right)
                        .on_any_mouse_down(cx.listener(|s, _, w, cx| {
                            cx.stop_propagation();
                            s.close_usage(w, cx);
                        }))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(content),
                ),
            )
            .with_priority(100)
            .into_any_element(),
        )
    }
}

fn section(provider: Provider, snapshot: &Snapshot, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let status = match snapshot.status {
        Status::Idle => Some("Not checked"),
        Status::Loading if snapshot.outdated => Some("Checking… · Previous result is outdated"),
        Status::Loading => Some("Checking…"),
        Status::Ready => None,
        Status::Failed(Failure::Temporary) if !snapshot.allowances.is_empty() => {
            Some("Outdated · Could not refresh usage")
        }
        Status::Failed(error) => Some(error.message()),
    };
    v_flex()
        .id(format!("usage-{}", provider.name()))
        .gap_2()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(provider.name()),
        )
        .children(status.map(|text| {
            div()
                .id(format!("usage-status-{}", provider.name()))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
                .test_support()
        }))
        .children(snapshot.allowances.iter().enumerate().map(|(ix, row)| {
            let remaining = row.remaining.map_or_else(
                || "Remaining unknown".into(),
                |n| {
                    if n > 0. && n < 1. {
                        "<1% remaining".into()
                    } else {
                        format!("{n:.0}% remaining")
                    }
                },
            );
            let reset = row.resets_at.map_or_else(
                || "Reset time unknown".into(),
                |at| format!("Resets {}", crate::usage::local_time(at)),
            );
            v_flex()
                .id(format!("usage-{}-{ix}", provider.name()))
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(div().min_w_0().flex_1().child(row.label.clone()))
                        .child(div().flex_none().text_xs().child(remaining.clone())),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme.muted)
                        .id(format!("usage-bar-{}-{ix}", provider.name()))
                        .role(Role::ProgressIndicator)
                        .aria_label(format!("{}: {remaining}", row.label))
                        .when_some(row.remaining, |d, n| {
                            d.child(
                                div()
                                    .h_full()
                                    .w(relative(n as f32 / 100.))
                                    .rounded_full()
                                    .bg(theme.primary),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(reset),
                )
                .test_support()
        }))
        .child(div().text_xs().text_color(theme.muted_foreground).child(
            snapshot.checked_at.map_or_else(
                || "Last checked: never".into(),
                |at| format!("Last checked: {}", crate::usage::local_time(at)),
            ),
        ))
        .when(snapshot.outdated, |d| {
            d.children(snapshot.fetched_at.map(|at| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Previous result: {}", crate::usage::local_time(at)))
            }))
        })
        .test_support()
}
