//! Account usage belongs to the window, independent of the selected workspace.
use super::*;
use crate::usage::{Allowance, Failure, Provider, Snapshot, Status};
use gpui_kit::component::FocusTrapElement as _;
use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle, NSTimeZone};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(300);
#[cfg(all(test, feature = "test-support"))]
mod pixels;
#[cfg(test)]
mod store_state;
#[cfg(all(test, feature = "test-support"))]
pub(in crate::app) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    pixels::render_pixels(cx);
}

pub(super) struct Panel {
    pub(super) focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    _keys: Subscription,
}

impl Shark {
    pub(super) fn show_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &self.usage_panel {
            panel.focus.focus(window, cx);
            cx.notify();
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
        self.restore_focus(panel.previous_focus, window, cx);
        cx.notify();
    }

    pub(super) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        for provider in Provider::ALL {
            let query = self.usage[provider.index()].begin();
            // Headless tests complete snapshots explicitly; never query the user's account.
            #[cfg(test)]
            {
                if query {
                    self.usage_queries[provider.index()] += 1;
                }
                if !self.usage_live {
                    continue;
                }
            }
            if query {
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { crate::usage::query(provider) })
                        .await;
                    this.update(cx, |s, cx| s.complete_usage_query(provider, result, cx))
                        .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn complete_usage_query(
        &mut self,
        provider: Provider,
        result: Result<Vec<Allowance>, Failure>,
        cx: &mut Context<Self>,
    ) {
        self.usage[provider.index()].finish(result);
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
        div()
            .id("usage-footer")
            .relative()
            .flex_none()
            .p_2()
            .child(
                v_flex()
                    .when(self.rail, |d| d.items_center().gap_1())
                    .child(self.render_usage_quota_button(cx)),
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

    fn render_usage_quota_button(&self, cx: &Context<Self>) -> AnyElement {
        let now = crate::usage::now();
        let theme = cx.theme();
        let quotas: Vec<_> = Provider::ALL
            .into_iter()
            .map(|provider| {
                let snapshot = &self.usage[provider.index()];
                let limiting = snapshot.limiting(now);
                let remaining = limiting.and_then(|row| row.remaining);
                let text = match remaining {
                    Some(n) => format!("{}%", n.ceil()),
                    None if snapshot.checked_at.is_none() => "…".into(),
                    None => "—".into(),
                };
                let dot = quota_color(
                    remaining,
                    limiting.and_then(|row| row.pace(now)),
                    snapshot.outdated,
                    theme,
                );
                (provider, text, dot)
            })
            .collect();
        let quota = Provider::ALL
            .into_iter()
            .map(|provider| quota_details(provider, &self.usage[provider.index()], now))
            .collect::<Vec<_>>()
            .join(" · ");
        Button::new("show-usage")
            .ghost()
            .h_auto()
            .p_1()
            .when(self.rail, |b| b.px_0())
            .w_full()
            .accessibility_label(format!("{quota}. Open usage"))
            .tooltip(quota)
            .on_click(cx.listener(|s, _, w, cx| s.show_usage(w, cx)))
            .child(
                h_flex()
                    .id("usage-footer-quotas")
                    .min_w_0()
                    .when(!self.rail, |d| d.w_full())
                    .when(self.rail, |d| d.justify_center())
                    .overflow_hidden()
                    .items_center()
                    .gap(px(3.))
                    .text_size(px(if self.rail { 9. } else { 12. }))
                    .whitespace_nowrap()
                    .children(quotas.into_iter().enumerate().map(
                        |(index, (provider, text, dot))| {
                            let id = provider.id();
                            h_flex()
                                .flex_none()
                                .gap(px(if self.rail { 2. } else { 3. }))
                                .when(index > 0 && !self.rail, |d| {
                                    d.child(div().text_color(theme.sidebar_foreground).child("·"))
                                })
                                .when(!self.rail, |d| {
                                    // The hue stays out of the text so light themes keep sidebar contrast.
                                    d.items_center().child(
                                        div()
                                            .id(format!("usage-footer-pace-{id}"))
                                            .size(px(6.))
                                            .flex_none()
                                            .rounded_full()
                                            .bg(dot)
                                            .test_support(),
                                    )
                                })
                                .child(
                                    div()
                                        .id(format!("usage-footer-quota-{id}"))
                                        .text_color(theme.sidebar_foreground)
                                        .child(if self.rail {
                                            text
                                        } else {
                                            format!("{} {text}", provider.name())
                                        })
                                        .test_support(),
                                )
                        },
                    ))
                    .when(!self.rail, |d| {
                        d.child(div().text_color(theme.sidebar_foreground).child("left"))
                    })
                    .test_support(),
            )
            .into_any_element()
    }

    pub(super) fn render_usage(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let panel = self.usage_panel.as_ref()?;
        let viewport = window.viewport_size();
        let footer = self.usage_at.get();
        let left = footer
            .left()
            .max(px(8.))
            .min((viewport.width - px(352.)).max(px(8.)));
        let bottom = (viewport.height - footer.top() + px(4.)).max(px(8.));
        let content = menu::chrome(cx.theme())
            .id("usage-panel")
            .role(Role::Dialog)
            .aria_label("Account usage")
            .aria_description("Subscription allowances. Enter to refresh. Escape to close.")
            .track_focus(&panel.focus)
            .w(px(344.).min(viewport.width - px(16.)))
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
                            .accessibility_label("Refresh usage")
                            .tooltip("Refresh usage")
                            .icon(IconName::RefreshCw)
                            .on_click(cx.listener(|s, _, _, cx| s.refresh_usage(cx)))
                            .map(|b| crate::ui::toolbar("refresh-usage", b)),
                    ),
            )
            .children(
                Provider::ALL.map(|provider| section(provider, &self.usage[provider.index()], cx)),
            )
            .test_support()
            .focus_trap("usage-panel", &panel.focus);
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
        .id(format!("usage-{}", provider.id()))
        .role(Role::Group)
        .aria_label(provider.name().to_owned())
        .aria_description(snapshot.checked_at.map_or_else(
            || "Last checked: never".into(),
            |at| format!("Last checked: {}", local_time(at)),
        ))
        .gap_2()
        .p_3()
        .border_1()
        .border_color(theme.border)
        .rounded_lg()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(provider.name()),
        )
        .children(status.map(|text| {
            div()
                .id(format!("usage-status-{}", provider.id()))
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
                        format!("{}% remaining", n.ceil())
                    }
                },
            );
            let now = crate::usage::now();
            let pace = row.pace(now);
            let mut reset = resets(row.resets_at);
            if let Some(p) = pace {
                reset.push_str(&even_pace(p));
            }
            v_flex()
                .id(format!("usage-{}-{ix}", provider.id()))
                .role(Role::Group)
                .aria_description(reset.clone())
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
                        .relative()
                        .w_full()
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme.muted)
                        .id(format!("usage-bar-{}-{ix}", provider.id()))
                        .role(Role::ProgressIndicator)
                        .aria_label(format!("{}: {remaining}", row.label))
                        .when_some(row.remaining, |d, n| {
                            d.child(
                                div()
                                    .id(format!("usage-fill-{}-{ix}", provider.id()))
                                    .h_full()
                                    .w(relative(n.clamp(0., 100.) as f32 / 100.))
                                    .rounded_full()
                                    .bg(quota_color(Some(n), pace, snapshot.outdated, theme))
                                    .test_support(),
                            )
                        })
                        .when_some(pace, |d, p| {
                            // A 1.5px line, knocked out of the bar by 1.5px of panel background.
                            d.child(quota_marker(provider, ix, p, theme))
                        })
                        .test_support(),
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
                |at| format!("Last checked: {}", local_time(at)),
            ),
        ))
        .when(snapshot.outdated, |d| {
            d.children(snapshot.fetched_at.map(|at| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("Previous result: {}", local_time(at)))
            }))
        })
        .test_support()
}

fn marker_left(remaining: f64, width: f32) -> f32 {
    ((remaining.clamp(0., 100.) / 100.) as f32 * width - 2.25).clamp(0., (width - 4.5).max(0.))
}
fn quota_marker(provider: Provider, index: usize, remaining: f64, theme: &Theme) -> AnyElement {
    let background = theme.popover;
    let ink = theme.popover_foreground;
    div()
        .id(format!("usage-marker-{}-{index}", provider.id()))
        .role(Role::Label)
        .aria_label(format!(
            "Even pace: {:.1}% remaining",
            remaining.clamp(0., 100.)
        ))
        .absolute()
        .top(px(-2.))
        .left_0()
        .w_full()
        .h(px(8.))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let left =
                        bounds.left() + px(marker_left(remaining, f32::from(bounds.size.width)));
                    window.paint_quad(fill(
                        Bounds::new(point(left, bounds.top()), size(px(4.5), bounds.size.height)),
                        background,
                    ));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(left + px(1.5), bounds.top()),
                            size(px(1.5), bounds.size.height),
                        ),
                        ink,
                    ));
                },
            )
            .size_full(),
        )
        .test_support()
        .into_any_element()
}

/// The dot and bar colour: what is left, and for what is left, how far it trails an even burn.
fn quota_color(remaining: Option<f64>, even: Option<f64>, outdated: bool, theme: &Theme) -> Hsla {
    let Some(n) = remaining.filter(|_| !outdated) else {
        return faint(theme);
    };
    match even {
        _ if n <= 5. => theme.danger,
        _ if n <= 20. => theme.warning,
        Some(even) if even - n > 25. => theme.danger,
        Some(even) if even - n > 10. => theme.warning,
        _ => theme.success,
    }
}

/// "Claude · 5 hours · 42% left · Even pace: 50% left" per allowance, then a status word.
fn quota_details(provider: Provider, snapshot: &Snapshot, now: f64) -> String {
    let name = provider.name();
    let mut parts: Vec<String> = snapshot
        .allowances
        .iter()
        .filter(|row| row.resets_at.is_none_or(|at| at > now))
        .map(|row| {
            let mut text = match row.remaining {
                Some(n) => format!("{name} · {} · {}% left", row.label, n.ceil()),
                None => format!("{name} · {} · left unknown", row.label),
            };
            if let Some(p) = row.pace(now) {
                text.push_str(&even_pace(p));
            }
            text
        })
        .collect();
    if parts.is_empty() {
        parts.push(format!("{name} · left unknown"));
    }
    match snapshot.status {
        _ if snapshot.outdated => parts.push("Outdated".into()),
        Status::Loading => parts.push("Checking…".into()),
        Status::Failed(_) => parts.push("Unavailable".into()),
        Status::Idle | Status::Ready => {}
    }
    parts.join(" · ")
}

/// "Resets Oct 28, 2026 at 11:00 PM", or that it's unknown.
fn resets(at: Option<f64>) -> String {
    at.map_or_else(
        || "Reset time unknown".into(),
        |at| format!("Resets {}", local_time(at)),
    )
}

/// " · Even pace: 40% left", to append to a reset line.
fn even_pace(p: f64) -> String {
    format!(" · Even pace: {}% left", p.round())
}

fn local_time(seconds: f64) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setDateStyle(NSDateFormatterStyle::MediumStyle);
    formatter.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    formatter.setTimeZone(Some(&NSTimeZone::localTimeZone()));
    formatter
        .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(seconds))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{Allowance, Provider, Snapshot, Status, local_time};

    #[test]
    fn reset_and_pace_text() {
        assert_eq!(super::resets(None), "Reset time unknown");
        assert!(super::resets(Some(0.)).starts_with("Resets "));
        assert_eq!(super::even_pace(39.6), " · Even pace: 40% left");
        assert!(!local_time(1e9).is_empty());
    }

    #[test]
    fn details_join_allowances_and_name_the_status() {
        let mut snapshot = Snapshot::default();
        assert_eq!(
            super::quota_details(Provider::Claude, &snapshot, 0.),
            "Claude · left unknown"
        );
        snapshot.allowances.push(Allowance {
            label: "5 hours".into(),
            remaining: Some(41.2),
            resets_at: Some(9000.),
            window: Some(18000.),
        });
        snapshot.status = Status::Ready;
        assert_eq!(
            super::quota_details(Provider::Claude, &snapshot, 0.),
            "Claude · 5 hours · 42% left · Even pace: 50% left"
        );
        snapshot.outdated = true;
        assert!(super::quota_details(Provider::Claude, &snapshot, 0.).ends_with(" · Outdated"));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod lifecycle_tests {
    use super::*;
    use crate::app::ui_tests::{UI_LOCK, boot, test_home};
    use core::prelude::v1::test;
    use gpui_kit::test::TestWindowExt;

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn quota_colors_follow_remaining_then_even_pace(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            let theme = cx.theme();
            assert_eq!(quota_color(Some(50.), Some(55.), true, theme), faint(theme));
            assert_eq!(quota_color(None, Some(55.), false, theme), faint(theme));
            assert_eq!(quota_color(Some(5.), Some(5.), false, theme), theme.danger);
            assert_eq!(quota_color(Some(20.), None, false, theme), theme.warning);
            assert_eq!(
                quota_color(Some(50.), Some(76.), false, theme),
                theme.danger
            );
            assert_eq!(
                quota_color(Some(50.), Some(75.), false, theme),
                theme.warning
            );
            assert_eq!(
                quota_color(Some(50.), Some(61.), false, theme),
                theme.warning
            );
            assert_eq!(
                quota_color(Some(50.), Some(60.), false, theme),
                theme.success
            );
            assert_eq!(quota_color(Some(50.), None, false, theme), theme.success);
            shark.update(cx, |s, cx| {
                s.usage[0].finish(Ok(vec![Allowance {
                    label: "5 hours".into(),
                    remaining: Some(20.),
                    resets_at: None,
                    window: Some(18000.),
                }]));
                s.show_usage(w, cx);
            });
            w.render_frame(cx);
            let bar = w.find("usage-bar-claude-0").bounds();
            let fill = w.find("usage-fill-claude-0").bounds();
            assert!(
                (fill.size.width / bar.size.width - 0.2).abs() < 0.01,
                "bar fills remaining 20%, consistently with its label"
            );
        })
        .unwrap();
    }
}
