//! Account usage belongs to the window, independent of the selected workspace.
use super::*;
#[cfg(test)]
use crate::usage::Provider;
use crate::usage::activity::WEEKS;
#[cfg(test)]
use crate::usage::activity::{self, Daily};
use crate::usage::providers::ProviderHandle;
use crate::usage::{Allowance, Failure, Snapshot, Status};
use gpui_kit::component::FocusTrapElement as _;
use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle, NSTimeZone};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(300);
mod footer;
#[cfg(all(test, feature = "test-support"))]
mod pixels;
mod provider_tabs;
mod recap;
#[cfg(test)]
mod store_state;
mod totals;
mod trend;
#[cfg(all(test, feature = "test-support"))]
pub(in crate::app) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    pixels::render_pixels(cx);
}

pub(super) struct Panel {
    pub(super) focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    details: bool,
    pub(super) recap: Option<(crate::usage::summary::Scope, i32)>,
    hovered_day: Option<chrono::NaiveDate>,
    selected_provider: Option<String>,
    #[cfg(test)]
    reference_time: Option<f64>,
    _keys: Subscription,
}

impl Shark {
    pub(super) fn show_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &mut self.usage_panel {
            panel.recap = None;
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
                    "escape" if panel.recap.is_some() => {
                        s.usage_panel.as_mut().unwrap().recap = None;
                        cx.notify();
                    }
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
            details: false,
            recap: None,
            hovered_day: None,
            selected_provider: None,
            #[cfg(test)]
            reference_time: None,
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
        for provider in self.usage_providers.clone() {
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
                let generation = self.usage_generation;
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move { provider.source().remaining_usage() })
                        .await;
                    this.update(cx, |s, cx| {
                        s.complete_usage_query(provider, generation, result, cx);
                    })
                    .ok();
                })
                .detach();
            }
            // Token scans are independent of a slow or unavailable quota endpoint.
            if self.usage_scanning[provider.index()] {
                continue;
            }
            self.usage_scanning[provider.index()] = true;
            let generation = self.usage_generation;
            cx.spawn(async move |this, cx| {
                let observation = cx
                    .background_executor()
                    .spawn(async move { provider.source().observations(crate::usage::now()) })
                    .await;
                this.update(cx, |s, cx| {
                    s.complete_usage_activity(provider, generation, observation, cx);
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn complete_usage_activity(
        &mut self,
        provider: ProviderHandle,
        generation: u64,
        observation: crate::usage::providers::Observation,
        cx: &mut Context<Self>,
    ) {
        self.usage_scanning[provider.index()] = false;
        // An invalidated scan must not enter the shared report set: the next provider's
        // successful scan records that entire set in the persistent recap ledger.
        if self.usage_generation != generation {
            self.refresh_usage(cx);
            cx.notify();
            return;
        }
        self.usage_activity[provider.index()] = observation.activity;
        if let Some(error) = observation.report.error.as_ref() {
            self.usage_reports[provider.index()].error = Some(error.clone());
            cx.notify();
            return;
        }
        self.usage_reports[provider.index()] = observation.report;
        let now = crate::usage::now();
        let days = crate::usage::summary::combine_series(
            self.usage_reports
                .iter()
                .map(|report| report.month_daily(now)),
        );
        let previous = self.state.usage_ledger.clone();
        self.state
            .usage_ledger
            .record(&days, crate::usage::summary::local_date(now));
        if self.state.usage_ledger != previous {
            let _ = self.state.save();
        }
        cx.notify();
    }

    fn complete_usage_query(
        &mut self,
        provider: ProviderHandle,
        generation: u64,
        result: Result<Vec<Allowance>, Failure>,
        cx: &mut Context<Self>,
    ) {
        // Always release the quota request lock, including requests started before a
        // roots change.
        self.usage[provider.index()].finish(result);
        if self.usage_generation != generation {
            self.refresh_usage(cx);
        }
        cx.notify();
    }

    pub(crate) fn set_usage_roots(&mut self, provider_id: &str, raw: &str, cx: &mut Context<Self>) {
        let Some(provider) = self.usage_providers.iter().copied().find(|provider| {
            provider.id() == provider_id && provider.source().supports_custom_roots()
        }) else {
            return;
        };
        let raw = raw.trim();
        if self
            .state
            .usage_roots
            .get(provider_id)
            .map(String::as_str)
            .unwrap_or("")
            == raw
        {
            return;
        }
        if raw.is_empty() {
            self.state.usage_roots.remove(provider_id);
        } else {
            self.state
                .usage_roots
                .insert(provider_id.to_owned(), raw.to_owned());
        }
        // In-flight requests carry the old generation; each releases its own slot on completion.
        self.usage_generation = self.usage_generation.wrapping_add(1);
        provider.source().configure_scan_roots(raw);
        let _ = self.state.save();
        self.refresh_usage(cx);
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
        let quotas: Vec<_> = self
            .usage_providers
            .iter()
            .map(|provider| {
                (
                    provider.id(),
                    provider.name(),
                    footer::headline(&self.usage[provider.index()], now),
                )
            })
            .collect();
        let quota = self
            .usage_providers
            .iter()
            .flat_map(|provider| {
                footer::quota_details(provider.name(), &self.usage[provider.index()], now)
            })
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
                    .overflow_hidden()
                    .items_center()
                    .gap(px(3.))
                    .text_size(px(if self.rail { 9. } else { 12. }))
                    .whitespace_nowrap()
                    .children(
                        quotas
                            .into_iter()
                            .enumerate()
                            .map(|(index, (id, name, value))| {
                                let dot = value
                                    .remaining
                                    .map(|n| pace_color(n, value.elapsed, false, cx.theme()));
                                h_flex()
                                    .flex_none()
                                    .gap(px(if self.rail { 2. } else { 3. }))
                                    .when(index > 0 && !self.rail, |d| {
                                        d.child(
                                            div()
                                                .text_color(cx.theme().sidebar_foreground)
                                                .child("·"),
                                        )
                                    })
                                    .when_some(dot.filter(|_| !self.rail), |d, color| {
                                        // Pace hue stays out of the text so light themes keep sidebar contrast.
                                        d.items_center().child(
                                            div()
                                                .id(format!("usage-footer-pace-{id}"))
                                                .size(px(6.))
                                                .flex_none()
                                                .rounded_full()
                                                .bg(color)
                                                .test_support(),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(format!("usage-footer-quota-{id}"))
                                            .text_color(cx.theme().sidebar_foreground)
                                            .child(if self.rail {
                                                value.text
                                            } else {
                                                format!("{name} {}", value.text)
                                            })
                                            .test_support(),
                                    )
                            }),
                    )
                    .when(!self.rail, |d| {
                        d.child(
                            div()
                                .text_color(cx.theme().sidebar_foreground)
                                .child("left"),
                        )
                    })
                    .test_support(),
            )
            .into_any_element()
    }

    pub(super) fn usage_summary(&self, now: f64) -> crate::usage::summary::combined::Summary {
        crate::usage::summary::combined::Summary::from_sources(
            self.usage_providers
                .iter()
                .map(|provider| crate::usage::summary::combined::Source {
                    id: provider.id(),
                    report: &self.usage_reports[provider.index()],
                }),
            now,
        )
    }
    fn usage_view_now(&self) -> f64 {
        #[cfg(test)]
        if let Some(now) = self
            .usage_panel
            .as_ref()
            .and_then(|panel| panel.reference_time)
        {
            return now;
        }
        crate::usage::now()
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
        let recap = panel.recap.is_some();
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
            .when(recap, |d| d.child(self.render_usage_recap(cx)))
            .when(!recap, |d| {
                d.child(
                    h_flex()
                        .justify_between()
                        .items_center()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Usage"))
                        .child(
                            Button::new("usage-open-recap")
                                .ghost()
                                .xsmall()
                                .label("Recap")
                                .on_click(cx.listener(|s, _, _, cx| s.open_usage_recap(cx))),
                        )
                        .child(
                            Button::new("refresh-usage")
                                .ghost()
                                .xsmall()
                                .accessibility_label("Refresh usage")
                                .tooltip("Refresh usage")
                                .icon(IconName::RefreshCw)
                                .on_click(cx.listener(|s, _, _, cx| s.refresh_usage(cx))),
                        ),
                )
            })
            .when(!recap, |d| {
                let summary = self.usage_summary(self.usage_view_now());
                d.child(self.render_usage_totals(&summary, cx))
            })
            .when(!recap, |d| d.child(self.render_usage_provider_tabs(cx)))
            .when(!recap, |d| {
                let selected = self.selected_usage_provider();
                d.children(
                    self.usage_providers
                        .iter()
                        .copied()
                        .filter(|provider| selected.is_none_or(|id| provider.id() == id))
                        .map(|provider| {
                            let i = provider.index();
                            section(
                                provider,
                                &self.usage[i],
                                &self.usage_reports[i],
                                panel.details,
                                cx,
                            )
                        }),
                )
            })
            .when(!recap, |d| {
                d.child(
                    Button::new("usage-toggle-details")
                        .ghost()
                        .xsmall()
                        .label(if panel.details {
                            "Hide details"
                        } else {
                            "Details"
                        })
                        .toggled(panel.details)
                        .icon(if panel.details {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .accessibility_label(if panel.details {
                            "Hide usage details"
                        } else {
                            "Show usage details"
                        })
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(panel) = s.usage_panel.as_mut() {
                                panel.details = !panel.details;
                            }
                            cx.notify();
                        })),
                )
            })
            .when(!recap && panel.details, |d| {
                let now = self.usage_view_now();
                let summary = self.usage_summary(now);
                let series: Vec<_> = self
                    .usage_providers
                    .iter()
                    .map(|p| crate::usage::summary::ProviderSeries {
                        id: p.id().into(),
                        name: p.name().into(),
                        days: Some(self.usage_reports[p.index()].month_daily(now)),
                    })
                    .collect();
                let registry: Vec<_> = self
                    .usage_providers
                    .iter()
                    .map(|p| p.id().to_owned())
                    .collect();
                d.child(self.render_month_trend(
                    &summary.month_daily,
                    &series,
                    &registry,
                    crate::usage::summary::local_date(now),
                    cx,
                ))
            })
            .when(!recap && panel.details, |d| {
                let selected = self.selected_usage_provider();
                d.children(
                    self.usage_providers
                        .iter()
                        .copied()
                        .filter(|provider| selected.is_none_or(|id| provider.id() == id))
                        .filter(|provider| {
                            let report = &self.usage_reports[provider.index()];
                            !report.entries.is_empty()
                        })
                        .map(|provider| {
                            v_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("{} token details", provider.name())),
                                )
                                .child(accounting(
                                    provider,
                                    &self.usage_reports[provider.index()],
                                    cx,
                                ))
                        }),
                )
            })
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

fn section(
    provider: ProviderHandle,
    snapshot: &Snapshot,
    report: &crate::usage::logs::Report,
    details: bool,
    cx: &App,
) -> impl IntoElement {
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
            let elapsed =
                crate::usage::pace::elapsed(row.resets_at, row.window, crate::usage::now());
            let pace = crate::usage::pace::marker(elapsed, true).map(|p| p * 100.);
            let mut reset = resets(row.resets_at);
            if let Some(p) = pace {
                reset.push_str(&even_pace(p));
            }
            if let Some(remaining) = row.remaining.filter(|_| !snapshot.outdated) {
                let tier = crate::usage::pace::gauge(100. - remaining, elapsed, 80., 95.);
                reset.push_str(" · ");
                reset.push_str(tier.label());
                if let Some(elapsed) = elapsed.filter(|p| *p >= 0.1) {
                    let delta = crate::usage::pace::rounded_delta(100. - remaining, elapsed);
                    if let Some(text) = crate::usage::pace::delta_text(delta) {
                        reset.push_str(&format!(" ({text})"));
                    }
                }
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
                                    .bg(pace_color(n, elapsed, snapshot.outdated, theme))
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
                        .child(if details {
                            reset
                        } else {
                            short_reset(row.resets_at, crate::usage::now())
                        }),
                )
                .test_support()
        }))
        .when(report.error.is_some(), |d| {
            let message = if report.entries.is_empty() {
                "Local usage unavailable; refresh failed"
            } else {
                "Local usage refresh failed; showing previous totals"
            };
            d.child(
                div()
                    .id(format!("usage-scan-error-{}", provider.id()))
                    .text_xs()
                    .text_color(theme.danger)
                    .role(Role::Label)
                    .aria_label(message)
                    .child(message)
                    .test_support(),
            )
        })
        .when(details, |d| {
            d.child(div().text_xs().text_color(theme.muted_foreground).child(
                snapshot.checked_at.map_or_else(
                    || "Last checked: never".into(),
                    |at| format!("Last checked: {}", local_time(at)),
                ),
            ))
        })
        .when(details && snapshot.outdated, |d| {
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
fn quota_marker(
    provider: ProviderHandle,
    index: usize,
    remaining: f64,
    theme: &Theme,
) -> AnyElement {
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

fn accounting(
    provider: ProviderHandle,
    report: &crate::usage::logs::Report,
    cx: &App,
) -> AnyElement {
    let bucket = &report.totals;
    v_flex()
        .id(format!("usage-accounting-{}", provider.id()))
        .gap_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(format!(
            "Tokens including cache ({WEEKS} weeks): {}",
            tokens(bucket.total())
        ))
        .child(format!(
            "Input {} · Output {}",
            tokens(bucket.input),
            tokens(bucket.output)
        ))
        .child(format!(
            "Cache writes {} · Cache reads {}",
            tokens(bucket.cache_write),
            tokens(bucket.cache_read)
        ))
        .test_support()
        .into_any_element()
}

pub(super) fn pace_color(
    remaining: f64,
    elapsed: Option<f64>,
    outdated: bool,
    theme: &Theme,
) -> Hsla {
    use crate::usage::pace::{Tier, gauge};
    if outdated {
        return faint(theme);
    }
    match gauge(100. - remaining, elapsed, 80., 95.) {
        Tier::WayUnder => rgb(0x3b82f6).into(),
        Tier::Under => rgb(0x06b6d4).into(),
        Tier::OnPace => theme.success,
        Tier::SlightlyOver => rgb(0xeab308).into(),
        Tier::Over => theme.warning,
        Tier::WayOver => theme.danger,
    }
}

fn short_reset(at: Option<f64>, now: f64) -> String {
    let Some(seconds) = at.filter(|at| at.is_finite()).map(|at| at - now) else {
        return "Reset time unknown".into();
    };
    if seconds <= 0. {
        "Reset due".into()
    } else if seconds < 60. {
        "Resets in <1m".into()
    } else if seconds < 3600. {
        format!("Resets in {}m", (seconds / 60.).ceil() as u64)
    } else if seconds < 86400. {
        format!("Resets in {}h", (seconds / 3600.).ceil() as u64)
    } else {
        format!("Resets in {}d", (seconds / 86400.).ceil() as u64)
    }
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

/// Token counts: "950", "412k", "38.2M".
fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => format!("{:.0}k", n as f64 / 1e3),
        999_500..999_950_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use super::{local_time, tokens};

    #[test]
    fn short_reset_handles_missing_due_and_relative_times() {
        assert_eq!(super::short_reset(None, 100.), "Reset time unknown");
        assert_eq!(
            super::short_reset(Some(f64::NAN), 100.),
            "Reset time unknown"
        );
        assert_eq!(super::short_reset(Some(99.), 100.), "Reset due");
        assert_eq!(super::short_reset(Some(120.), 100.), "Resets in <1m");
        assert_eq!(super::short_reset(Some(161.), 100.), "Resets in 2m");
        assert_eq!(super::short_reset(Some(7300.), 100.), "Resets in 2h");
        assert_eq!(super::short_reset(Some(172900.), 100.), "Resets in 2d");
    }

    #[test]
    fn reset_and_pace_text() {
        assert_eq!(super::resets(None), "Reset time unknown");
        assert!(super::resets(Some(0.)).starts_with("Resets "));
        assert_eq!(super::even_pace(39.6), " · Even pace: 40% left");
        assert!(!local_time(1e9).is_empty());
    }

    #[test]
    fn token_labels() {
        assert_eq!(tokens(412_345), "412k");
        assert_eq!(tokens(38_200_000), "38.2M");
        assert_eq!(tokens(999_999), "1.0M");
    }
}

#[cfg(all(test, feature = "test-support"))]
mod lifecycle_tests {
    use super::*;
    use crate::app::ui_tests::{UI_LOCK, boot, test_home};
    use crate::usage::activity::local_day;
    use core::prelude::v1::test;
    use gpui_kit::test::TestWindowExt;

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn stale_quota_completion_after_roots_change_releases_query_lock(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let old_generation = s.usage_generation;
                s.set_usage_roots("claude", "/tmp/shark-stale-quota-fixture", cx);
                assert_ne!(s.usage_generation, old_generation);
                let queries = s.usage_queries[Provider::Claude.index()];
                let ready = || {
                    Ok(vec![Allowance {
                        label: "5 hours".into(),
                        remaining: Some(50.),
                        resets_at: Some(1800000000.),
                        window: Some(18000.),
                    }])
                };
                s.complete_usage_query(Provider::Claude.into(), old_generation, ready(), cx);
                assert_eq!(
                    s.usage_queries[Provider::Claude.index()],
                    queries + 1,
                    "stale request must release loading lock for a new query"
                );
                assert_eq!(s.usage[Provider::Claude.index()].status, Status::Loading);
                s.complete_usage_query(Provider::Claude.into(), s.usage_generation, ready(), cx);
                assert_eq!(s.usage[Provider::Claude.index()].status, Status::Ready);
                assert_eq!(
                    s.usage_queries[Provider::Claude.index()],
                    queries + 1,
                    "fresh completion does not start another query"
                );
            });
        })
        .unwrap();
    }
    struct TestTool(&'static str, &'static str);
    impl crate::usage::providers::UsageProvider for TestTool {
        fn id(&self) -> &str {
            self.0
        }
        fn name(&self) -> &str {
            self.1
        }
        fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure> {
            Err(Failure::Unsupported)
        }
        fn activity(&self, _: f64) -> Daily {
            Daily {
                first_day: 20000,
                tokens: vec![100],
            }
        }
    }

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn registered_future_tool_automatically_joins_polling_and_ui(cx: &mut TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut registry = crate::usage::providers::Registry::default();
        registry.register(TestTool("claude", "Claude")).unwrap();
        registry.register(TestTool("codex", "Codex")).unwrap();
        registry
            .register(TestTool("future-tool", "Claude"))
            .unwrap();
        let registry = Box::leak(Box::new(registry));
        let providers: Vec<_> = registry.handles().collect();
        let future = providers[2];
        assert_eq!(future.index(), 2);
        assert_eq!(future.id(), "future-tool");
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            shark.update(cx, |s, cx| {
                s.usage_providers = providers;
                s.usage = vec![Snapshot::default(); 3];
                s.usage_activity = vec![Daily::default(); 3];
                s.usage_reports = (0..3)
                    .map(|_| crate::usage::logs::Report::from_daily(Daily::default()))
                    .collect();
                s.usage_queries = vec![0; 3];
                s.usage_scanning = vec![false; 3];
                s.refresh_usage(cx);
                assert_eq!(s.usage_queries, [1, 1, 1]);
                assert!(
                    s.usage
                        .iter()
                        .all(|snapshot| snapshot.status == Status::Loading)
                );
                s.complete_usage_query(
                    future,
                    s.usage_generation,
                    Ok(vec![Allowance {
                        label: "5 hours".into(),
                        remaining: Some(50.),
                        resets_at: Some(1800000000.),
                        window: Some(18000.),
                    }]),
                    cx,
                );
                s.show_usage(w, cx);
            });
            w.render_frame(cx);
            assert!(w.try_find("usage-future-tool").is_some());
            assert!(w.try_find("usage-trigger-future-tool").is_none());
            assert!(w.find("usage-footer-quota-future-tool").visible());
        })
        .unwrap();
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn first_usage_refresh_persists_ledger_and_prunes_only_older_years(
        cx: &mut TestAppContext,
    ) {
        use chrono::Datelike;
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let now = crate::usage::now();
                let today = crate::usage::summary::local_date(now);
                let previous_year = format!("{}-01-01", today.year() - 1);
                let older_year = format!("{}-12-31", today.year() - 2);
                s.state
                    .usage_ledger
                    .tokens_by_day
                    .insert(previous_year.clone(), 500);
                s.state
                    .usage_ledger
                    .tokens_by_day
                    .insert(older_year.clone(), 700);
                s.state.usage_ledger.covered_since = Some(older_year.clone());
                let observation = crate::usage::providers::Observation {
                    activity: Daily {
                        first_day: local_day(now),
                        tokens: vec![100],
                    },
                    report: crate::usage::logs::Report::from_entries(
                        vec![crate::usage::logs::Entry {
                            id: "ledger-first".into(),
                            date: now,
                            input: 100,
                            cache_read: 50,
                            ..Default::default()
                        }],
                        now,
                    ),
                };
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    s.usage_generation,
                    observation,
                    cx,
                );
                assert_eq!(s.state.usage_ledger.tokens(today), 150);
                assert_eq!(s.state.usage_ledger.tokens_by_day[&previous_year], 500);
                assert!(!s.state.usage_ledger.tokens_by_day.contains_key(&older_year));
                assert_eq!(s.state.usage_ledger.covered_since, Some(previous_year));
                assert_eq!(State::load().usage_ledger, s.state.usage_ledger);
            });
        })
        .unwrap();
    }

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn native_quota_gauges_use_remaining_width_and_pace_threshold_colors(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            let theme = cx.theme();
            assert_eq!(pace_color(50., None, false, theme), theme.success);
            assert_eq!(pace_color(20., None, false, theme), theme.warning);
            assert_eq!(pace_color(5., None, false, theme), theme.danger);
            assert_eq!(pace_color(5., Some(0.5), true, theme), faint(theme));
            assert_eq!(crate::usage::pace::marker(Some(0.25), true), Some(0.75));
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
                "bar fills remaining20%, consistently with its label"
            );
        })
        .unwrap();
    }

    fn observation(display: u64, inclusive: u64) -> crate::usage::providers::Observation {
        crate::usage::providers::Observation {
            activity: Daily {
                first_day: 20000,
                tokens: vec![display],
            },
            report: crate::usage::logs::Report::from_daily(Daily {
                first_day: 20000,
                tokens: vec![inclusive],
            }),
        }
    }

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn reports_use_inclusive_tokens_and_stale_scans_are_rejected_after_roots_change(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let provider: ProviderHandle = Provider::Claude.into();
                s.complete_usage_activity(provider, s.usage_generation, observation(100, 200), cx);
                s.complete_usage_activity(provider, s.usage_generation, observation(110, 250), cx);
                assert_eq!(
                    s.usage_activity[0].tokens,
                    [110],
                    "existing heatmap keeps its original definition"
                );
                assert_eq!(s.usage_reports[0].daily.tokens, [250]);
                let stale = s.usage_generation;
                s.usage_scanning[0] = true;
                s.set_usage_roots("claude", "/tmp/shark-stale-scan-fixture", cx);
                s.complete_usage_activity(provider, stale, observation(500, 1000), cx);
                assert_eq!(
                    s.usage_reports[0].daily.tokens,
                    [250],
                    "stale pre-change scan cannot replace the report"
                );
                assert!(
                    !s.usage_scanning[0],
                    "stale completion releases its scan slot"
                );
                s.complete_usage_activity(provider, s.usage_generation, observation(120, 300), cx);
                assert_eq!(s.usage_reports[0].daily.tokens, [300]);
            });
        })
        .unwrap();
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn changing_custom_roots_persists_and_rejects_pending_scan(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let old = s.usage_generation;
                s.usage_scanning[0] = true;
                s.set_usage_roots("claude", "  /tmp/shark-custom-logs  ", cx);
                assert_ne!(s.usage_generation, old);
                assert_eq!(
                    s.state.usage_roots.get("claude").map(String::as_str),
                    Some("/tmp/shark-custom-logs")
                );
                assert_eq!(State::load().usage_roots, s.state.usage_roots);
                let before = s.usage_reports[0].daily.tokens.clone();
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    old,
                    observation(1000, 1000),
                    cx,
                );
                assert_eq!(s.usage_reports[0].daily.tokens, before);
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    s.usage_generation,
                    observation(1000, 1000),
                    cx,
                );
                assert_eq!(s.usage_reports[0].daily.tokens, [1000]);
                let current = s.usage_generation;
                s.set_usage_roots("claude", "/tmp/shark-custom-logs", cx);
                s.set_usage_roots("missing", "/tmp/ignored", cx);
                assert_eq!(s.usage_generation, current);
                s.set_usage_roots("claude", "", cx);
                assert!(!s.state.usage_roots.contains_key("claude"));
            });
        })
        .unwrap();
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "usage_report_tests.rs"]
mod report_tests;
