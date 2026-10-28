//! Account usage belongs to the window, independent of the selected workspace.
use super::*;
use crate::usage::{Failure, Provider, Snapshot, Status};
use gpui_kit::component::progress::ProgressCircle;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(300);

pub(super) struct Panel {
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
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
            _keys: keys,
        });
        self.refresh_usage(cx);
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
        let now = cx.background_executor().now();
        if self.usage_due.is_none()
            || (window.is_window_active() && self.usage_due.is_some_and(|due| due <= now))
        {
            self.refresh_usage(cx);
            self.usage_due = Some(now + POLL_INTERVAL);
        }
        if !window.is_window_active() {
            self.usage_poll = None;
        } else if self.usage_poll.is_none() {
            let delay = self.usage_due.unwrap().saturating_duration_since(now);
            self.usage_poll = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(delay).await;
                this.update_in(cx, |s, w, cx| {
                    s.usage_poll = None;
                    s.usage_polling(w, cx);
                })
                .ok();
            }));
        }
    }

    pub(super) fn render_usage_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let at = self.usage_at.clone();
        let providers: Vec<_> = Provider::ALL
            .into_iter()
            .filter(|p| !self.usage[p.index()].allowances.is_empty())
            .collect();
        let fallback = providers.is_empty();
        div()
            .id("usage-footer")
            .relative()
            .flex_none()
            .p_2()
            .child(
                h_flex()
                    .gap_2()
                    .when(self.rail, |d| d.flex_col().items_center())
                    .when(fallback, |d| {
                        d.child(
                            Button::new("show-usage")
                                .ghost()
                                .small()
                                .icon(IconName::ChartBar)
                                .when(!self.rail, |b| b.label("Usage"))
                                .accessibility_label("Show usage")
                                .tooltip("Show account usage")
                                .on_click(cx.listener(|s, _, w, cx| s.show_usage(w, cx))),
                        )
                    })
                    .children(providers.into_iter().map(|provider| {
                        let snapshot = &self.usage[provider.index()];
                        let remaining = snapshot.limiting().and_then(|row| row.remaining);
                        let color = ring_color(remaining, snapshot.outdated, cx.theme());
                        let description = ring_description(provider, snapshot);
                        Button::new(format!("usage-trigger-{}", provider.name()))
                            .ghost()
                            .size(px(32.))
                            .p_0()
                            .accessibility_label(description.clone())
                            .tooltip(description.clone())
                            .child(
                                ProgressCircle::new(format!("usage-ring-{}", provider.name()))
                                    .size(px(24.))
                                    .value(remaining.unwrap_or(0.) as f32)
                                    .color(color)
                                    .accessibility_label(description)
                                    .child(
                                        Icon::new(match provider {
                                            Provider::Claude => IconName::Sparkles,
                                            Provider::Codex => IconName::SquareTerminal,
                                        })
                                        .with_size(px(12.))
                                        .text_color(color),
                                    ),
                            )
                            .on_click(cx.listener(|s, _, w, cx| s.show_usage(w, cx)))
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
            .test_support()
    }

    pub(super) fn render_usage(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let panel = self.usage_panel.as_ref()?;
        let viewport = window.viewport_size();
        let footer = self.usage_at.get();
        let left = footer
            .left()
            .max(px(8.))
            .min((viewport.width - px(328.)).max(px(8.)));
        let bottom = (viewport.height - footer.top() + px(4.)).max(px(8.));
        let content = menu::chrome(cx.theme())
            .id("usage-panel")
            .role(Role::Dialog)
            .aria_label("Account usage")
            .aria_description("Subscription allowances. Enter to refresh. Escape to close.")
            .track_focus(&panel.focus)
            .w(px(320.).min(viewport.width - px(16.)))
            .max_h((viewport.height - bottom - px(8.)).max(px(0.)))
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
                        .items_end()
                        .pl(left)
                        .pb(bottom)
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

fn ring_color(remaining: Option<f64>, outdated: bool, theme: &Theme) -> Hsla {
    match remaining {
        _ if outdated => theme.muted_foreground,
        Some(n) if n <= 5. => theme.danger_foreground,
        Some(n) if n <= 20. => theme.warning_foreground,
        Some(_) => theme.primary,
        None => theme.muted_foreground,
    }
}

fn ring_description(provider: Provider, snapshot: &Snapshot) -> String {
    let mut text = match snapshot.limiting() {
        Some(row) => format!(
            "{} · {} · {}% remaining · {}",
            provider.name(),
            row.label,
            row.remaining.unwrap(),
            row.resets_at.map_or_else(
                || "Reset time unknown".into(),
                |at| format!("Resets {}", crate::usage::local_time(at))
            )
        ),
        None => format!("{} · Remaining unknown", provider.name()),
    };
    if snapshot
        .allowances
        .iter()
        .any(|row| row.remaining.is_none())
    {
        text.push_str(" · Some allowances unknown");
    }
    if snapshot.outdated {
        text.push_str(" · Outdated");
    }
    if snapshot.status == Status::Loading {
        text.push_str(" · Checking…");
    }
    if let Some(at) = snapshot.checked_at {
        text.push_str(&format!(" · Last checked {}", crate::usage::local_time(at)));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Failure, Provider, Snapshot, Theme, ring_color, ring_description};
    use gpui_kit::rgb;

    #[test]
    fn usage_ring_thresholds_and_unknown_stale_labels() {
        let mut theme = Theme::default();
        theme.primary = rgb(0x123456).into();
        theme.warning_foreground = rgb(0x654321).into();
        theme.danger_foreground = rgb(0xff0000).into();
        theme.muted_foreground = rgb(0x888888).into();
        for (remaining, expected) in [
            (Some(21.), theme.primary),
            (Some(20.), theme.warning_foreground),
            (Some(5.1), theme.warning_foreground),
            (Some(5.), theme.danger_foreground),
            (Some(0.), theme.danger_foreground),
            (None, theme.muted_foreground),
        ] {
            assert_eq!(ring_color(remaining, false, &theme), expected);
            assert_eq!(ring_color(remaining, true, &theme), theme.muted_foreground);
        }
        let mut snapshot = Snapshot::default();
        snapshot.finish(Ok(vec![
            crate::usage::Allowance {
                label: "Weekly".into(),
                remaining: Some(0.),
                resets_at: Some(0.),
            },
            crate::usage::Allowance {
                label: "Model".into(),
                remaining: None,
                resets_at: None,
            },
        ]));
        snapshot.finish(Err(Failure::Temporary));
        snapshot.begin();
        let text = ring_description(Provider::Claude, &snapshot);
        for part in [
            "Claude",
            "Weekly",
            "0% remaining",
            "Resets",
            "Some allowances unknown",
            "Outdated",
            "Checking",
            "Last checked",
        ] {
            assert!(text.contains(part), "{text} should contain {part}");
        }
        snapshot.allowances.remove(0);
        assert!(ring_description(Provider::Claude, &snapshot).contains("Remaining unknown"));
    }
}
