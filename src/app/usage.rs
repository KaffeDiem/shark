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
#[cfg(all(test, feature = "test-support"))]
mod pixels;
mod provider_tabs;
mod recap;
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
    companion: bool,
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
        self.open_usage_panel(false, window, cx);
    }

    pub(super) fn show_companion_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_usage_panel(true, window, cx);
    }

    fn open_usage_panel(&mut self, companion: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.pokemon_ui.tab = pokemon::Page::Usage;
        self.cancel_profile_names();
        if let Some(panel) = &mut self.usage_panel {
            panel.companion = companion;
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
            companion,
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
        self.cancel_profile_names();
        self.restore_focus(panel.previous_focus, window, cx);
        self.pokemon_ui.growth_draft = None;
        self.pokemon_ui.shop_draft = None;
        self.pokemon_ui.selected_species = None;
        self.cancel_profile_names();
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
                let generation = self.pokemon_ui.generation;
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
            if self.pokemon_ui.scanning[provider.index()] {
                continue;
            }
            self.pokemon_ui.scanning[provider.index()] = true;
            let generation = self.pokemon_ui.generation;
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
        self.pokemon_ui.scanning[provider.index()] = false;
        // An invalidated scan must not enter the shared report set: the next provider's
        // successful scan records that entire set in the persistent recap ledger.
        if self.pokemon_ui.generation != generation {
            self.refresh_usage(cx);
            cx.notify();
            return;
        }
        self.usage_activity[provider.index()] = observation.activity;
        if let Some(error) = observation.companion.error.as_ref() {
            self.usage_reports[provider.index()].error = Some(error.clone());
            cx.notify();
            return;
        }
        let companion_daily = observation.companion.daily.clone();
        self.usage_reports[provider.index()] = observation.companion;
        {
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
            self.observe_pokemon_usage_daily(provider, &companion_daily, cx);
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
        // Always release the quota request lock, including requests started before an
        // import or enable toggle. Only a fresh request may mutate companion rewards.
        self.usage[provider.index()].finish(result);
        if self.pokemon_ui.generation == generation {
            self.observe_pokemon_limits(provider);
        } else {
            self.refresh_usage(cx);
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
        div()
            .id("usage-footer")
            .relative()
            .flex_none()
            .p_2()
            .child(
                v_flex()
                    .when(self.rail, |d| d.items_center().gap_1())
                    .when(!self.state.pokemon_disabled, |d| {
                        d.child(self.render_pokemon_footer(cx))
                    })
                    .when(self.state.pokemon_disabled, |d| {
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
                    }),
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

    pub(super) fn usage_summary(&self, now: f64) -> crate::usage::summary::combined::Summary {
        crate::usage::summary::combined::Summary::from_sources(
            self.usage_providers
                .iter()
                .map(|provider| crate::usage::summary::combined::Source {
                    id: provider.id(),
                    report: &self.usage_reports[provider.index()],
                    reports_cost: provider.source().reports_cost(),
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
        let companion = !self.state.pokemon_disabled && panel.companion;
        let pokemon = companion && self.pokemon_ui.tab != pokemon::Page::Usage;
        let recap = panel.recap.is_some();
        let content = menu::chrome(cx.theme())
            .id("usage-panel")
            .role(Role::Dialog)
            .aria_label(if companion {
                "AI usage and Pokémon"
            } else {
                "Account usage"
            })
            .aria_description(if companion {
                "AI model usage and Pokémon progress. Tab to move between controls. Escape to close."
            } else {
                "Subscription allowances. Enter to refresh. Escape to close."
            })
            .track_focus(&panel.focus)
            .w(px(344.).min(viewport.width - px(16.)))
            .max_h((viewport.height - bottom - px(8.)).max(px(0.)))
            .overflow_y_scroll()
            .p_4()
            .gap_4()
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .when(recap, |d| d.child(self.render_usage_recap(cx)))
            .when(!pokemon && !recap, |d| {
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
            .when(!pokemon && !recap, |d| {
                let summary = self.usage_summary(self.usage_view_now());
                d.child(self.render_usage_totals(&summary, cx))
            })
            .when(!pokemon && !recap, |d| {
                d.child(self.render_usage_provider_tabs(cx))
            })
            .when(!pokemon && !recap, |d| {
                let selected = self.selected_usage_provider();
                d.children(
                    self.usage_providers
                        .iter()
                        .copied()
                        .filter(|provider| selected.is_none_or(|id| provider.id() == id))
                        .map(|provider| {
                            let i = provider.index();
                            section(provider, &self.usage[i], &self.usage_reports[i], panel.details, cx)
                        }),
                )
            })
            .when(companion && !recap, |d| {
                d.child(self.render_pokemon_panel(cx))
            })
            .when(!pokemon && !recap, |d| {
                d.child(
                    Button::new("usage-toggle-details")
                        .ghost()
                        .xsmall()
                        .label(if panel.details { "Hide details" } else { "Details" })
                        .toggled(panel.details)
                        .icon(if panel.details { IconName::ChevronUp } else { IconName::ChevronDown })
                        .accessibility_label(if panel.details { "Hide usage details" } else { "Show usage details" })
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some(panel) = s.usage_panel.as_mut() {
                                panel.details = !panel.details;
                            }
                            cx.notify();
                        })),
                )
            })
            .when(!pokemon && !recap && panel.details, |d| {
                let now = self.usage_view_now();
                let summary = self.usage_summary(now);
                let series: Vec<_> = self
                    .usage_providers
                    .iter()
                    .map(|p| crate::usage::summary::ProviderSeries {
                        id: p.id().into(),
                        name: p.name().into(),
                        days: Some(self.usage_reports[p.index()].month_daily(now)),
                        reports_cost: p.source().reports_cost(),
                    })
                    .collect();
                let registry: Vec<_> = self
                    .usage_providers
                    .iter()
                    .map(|p| p.id().to_owned())
                    .collect();
                let shows_cost = self.shows_usage_cost(&summary);
                d.child(self.render_month_trend(
                        &summary.month_daily,
                        &series,
                        &registry,
                        crate::usage::summary::local_date(now),
                        shows_cost,
                        cx,
                    ))
            })
            .when(!pokemon && !recap && panel.details, |d| {
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
                                .child(companion_accounting(
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
            // Root dialogs paint at priority 10; companion confirmations must stay above this popover.
            .with_priority(5)
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

fn companion_accounting(
    provider: ProviderHandle,
    report: &crate::usage::logs::Report,
    cx: &App,
) -> AnyElement {
    let bucket = &report.totals;
    let coverage = bucket.coverage;
    let note = match (coverage.reported, coverage.estimated, coverage.unknown) {
        (_, _, true) if coverage.reported || coverage.estimated => " · partial coverage",
        (_, _, true) => "",
        (_, true, false) => " · includes estimates",
        (true, false, false) => " · reported",
        _ => "",
    };
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
        .when(provider.source().reports_cost(), |d| {
            d.child(
                div()
                    .id(format!("usage-cost-{}", provider.id()))
                    .aria_label(format!("Token cost: {}{note}", bucket.cost_text(false)))
                    .child(format!("Token cost: {}{note}", bucket.cost_text(false)))
                    .test_support(),
            )
        })
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
    async fn stale_quota_completion_after_toggle_cannot_award_candy(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let old_generation = s.pokemon_ui.generation;
                s.state.pokemon.observe_limit(
                    "claude:session",
                    Some("1800000000"),
                    50.,
                    false,
                    true,
                );
                s.set_pokemon_enabled(false, cx);
                s.set_pokemon_enabled(true, cx);
                let queries = s.usage_queries[Provider::Claude.index()];
                s.complete_usage_query(
                    Provider::Claude.into(),
                    old_generation,
                    Ok(vec![Allowance {
                        label: "5 hours".into(),
                        remaining: Some(0.),
                        resets_at: Some(1800000000.),
                        window: Some(18000.),
                    }]),
                    cx,
                );
                assert_eq!(s.state.pokemon.inventory.candies, 0);
                assert!(!s.state.pokemon.limit_baselines["claude:session"].claimed);
                assert_eq!(
                    s.usage_queries[Provider::Claude.index()],
                    queries + 1,
                    "stale request must release loading lock for a new query"
                );
                assert_eq!(s.usage[Provider::Claude.index()].status, Status::Loading);
                let fresh = s.pokemon_ui.generation;
                s.complete_usage_query(
                    Provider::Claude.into(),
                    fresh,
                    Ok(vec![Allowance {
                        label: "5 hours".into(),
                        remaining: Some(0.),
                        resets_at: Some(1800000000.),
                        window: Some(18000.),
                    }]),
                    cx,
                );
                assert_eq!(s.state.pokemon.inventory.candies, 1);
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
    async fn registered_future_tool_automatically_joins_polling_ui_and_companion(
        cx: &mut TestAppContext,
    ) {
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
                s.pokemon_ui.scanning = vec![false; 3];
                s.refresh_usage(cx);
                assert_eq!(s.usage_queries, [1, 1, 1]);
                assert!(
                    s.usage
                        .iter()
                        .all(|snapshot| snapshot.status == Status::Loading)
                );
                s.complete_usage_query(
                    future,
                    s.pokemon_ui.generation,
                    Ok(vec![Allowance {
                        label: "5 hours".into(),
                        remaining: Some(50.),
                        resets_at: Some(1800000000.),
                        window: Some(18000.),
                    }]),
                    cx,
                );
                assert!(
                    s.state
                        .pokemon
                        .limit_baselines
                        .contains_key("future-tool:session")
                );
                s.usage_activity[future.index()] = future.source().activity(0.);
                s.observe_pokemon_usage(future, cx);
                s.usage_activity[future.index()].tokens[0] = 150;
                s.observe_pokemon_usage(future, cx);
                assert_eq!(s.state.pokemon.lifetime_tokens, 50);
                assert!(s.state.pokemon.baselines.contains_key("future-tool"));
                s.apply_pokemon_save(s.state.pokemon.clone(), cx);
                assert_eq!(
                    s.pokemon_ui.scanning.len(),
                    3,
                    "imports preserve dynamic provider scan slots"
                );
                s.show_usage(w, cx);
            });
            w.render_frame(cx);
            assert!(w.try_find("usage-future-tool").is_some());
            assert!(w.try_find("usage-trigger-future-tool").is_none());
            assert!(w.find("pokemon-footer").visible());
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
                    companion: crate::usage::logs::Report::from_entries(
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
                    s.pokemon_ui.generation,
                    observation,
                    cx,
                );
                assert_eq!(s.state.usage_ledger.tokens(today), 150);
                assert_eq!(s.state.usage_ledger.tokens_by_day[&previous_year], 500);
                assert!(!s.state.usage_ledger.tokens_by_day.contains_key(&older_year));
                assert_eq!(s.state.usage_ledger.covered_since, Some(previous_year));
                assert_eq!(State::load().usage_ledger, s.state.usage_ledger);
                assert_eq!(
                    s.state.pokemon.lifetime_tokens, 0,
                    "first observation establishes growth baseline, independently of usage history"
                );
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
            companion: crate::usage::logs::Report::from_daily(Daily {
                first_day: 20000,
                tokens: vec![inclusive],
            }),
        }
    }

    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn companion_uses_inclusive_tokens_and_stale_scans_cannot_seed_toggle_baseline(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                let provider: ProviderHandle = Provider::Claude.into();
                s.complete_usage_activity(
                    provider,
                    s.pokemon_ui.generation,
                    observation(100, 200),
                    cx,
                );
                s.complete_usage_activity(
                    provider,
                    s.pokemon_ui.generation,
                    observation(110, 250),
                    cx,
                );
                assert_eq!(
                    s.usage_activity[0].tokens,
                    [110],
                    "existing heatmap keeps its original definition"
                );
                assert_eq!(s.usage_reports[0].daily.tokens, [250]);
                assert_eq!(
                    s.state.pokemon.lifetime_tokens, 50,
                    "growth observes inclusive companion counters"
                );
                let stale = s.pokemon_ui.generation;
                s.pokemon_ui.scanning[0] = true;
                s.set_pokemon_enabled(false, cx);
                s.set_pokemon_enabled(true, cx);
                s.complete_usage_activity(provider, stale, observation(500, 1000), cx);
                assert!(
                    s.state.pokemon.baselines.is_empty(),
                    "stale pre-toggle scan cannot seed a new baseline"
                );
                assert!(
                    !s.pokemon_ui.scanning[0],
                    "stale completion releases its scan slot"
                );
                s.complete_usage_activity(
                    provider,
                    s.pokemon_ui.generation,
                    observation(120, 300),
                    cx,
                );
                assert_eq!(s.state.pokemon.lifetime_tokens, 50);
                s.complete_usage_activity(
                    provider,
                    s.pokemon_ui.generation,
                    observation(125, 350),
                    cx,
                );
                assert_eq!(s.state.pokemon.lifetime_tokens, 100);
            });
        })
        .unwrap();
    }
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn changing_custom_roots_rebases_only_that_source_and_rejects_pending_scan(
        cx: &mut TestAppContext,
    ) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let (window, shark) = boot(cx, State::default(), WindowOptions::default());
        cx.update_window(window, |_, _, cx| {
            shark.update(cx, |s, cx| {
                for provider in ["claude", "codex"] {
                    s.state.pokemon.observe_usage(provider, "20000", 100, true);
                }
                let old = s.pokemon_ui.generation;
                s.pokemon_ui.scanning[0] = true;
                s.set_usage_roots("claude", "  /tmp/shark-custom-logs  ", cx);
                assert_ne!(s.pokemon_ui.generation, old);
                assert_eq!(
                    s.state.usage_roots.get("claude").map(String::as_str),
                    Some("/tmp/shark-custom-logs")
                );
                assert_eq!(State::load().usage_roots, s.state.usage_roots);
                assert!(!s.state.pokemon.baselines.contains_key("claude"));
                assert_eq!(s.state.pokemon.baselines["codex"].total, 100);
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    old,
                    observation(1000, 1000),
                    cx,
                );
                assert!(!s.state.pokemon.baselines.contains_key("claude"));
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    s.pokemon_ui.generation,
                    observation(1000, 1000),
                    cx,
                );
                assert_eq!(
                    s.state.pokemon.lifetime_tokens, 0,
                    "historical folders cannot grant currency"
                );
                s.complete_usage_activity(
                    Provider::Claude.into(),
                    s.pokemon_ui.generation,
                    observation(1010, 1010),
                    cx,
                );
                assert_eq!(s.state.pokemon.lifetime_tokens, 10);
                let current = s.pokemon_ui.generation;
                s.set_usage_roots("claude", "/tmp/shark-custom-logs", cx);
                s.set_usage_roots("missing", "/tmp/ignored", cx);
                assert_eq!(s.pokemon_ui.generation, current);
                s.set_usage_roots("claude", "", cx);
                assert!(!s.state.usage_roots.contains_key("claude"));
                assert!(!s.state.pokemon.baselines.contains_key("claude"));
            });
        })
        .unwrap();
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "usage_cost_tests.rs"]
mod cost_tests;
