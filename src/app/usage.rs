//! Account usage belongs to the window, independent of the selected workspace.
use super::*;
#[cfg(test)]
use crate::usage::Provider;
use crate::usage::activity::{self, Daily, WEEKS};
use crate::usage::providers::ProviderHandle;
use crate::usage::{Allowance, Failure, Snapshot, Status};
use gpui_kit::component::FocusTrapElement as _;
use gpui_kit::component::plot::shape::{Arc, ArcData};
use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle, NSString, NSTimeZone};
use std::f32::consts::TAU;
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
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    companion: bool,
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
        let providers: Vec<_> = self
            .usage_providers
            .iter()
            .copied()
            .filter(|p| !self.usage[p.index()].allowances.is_empty())
            .collect();
        let fallback = providers.is_empty();
        let theme = cx.theme();
        let hover = side_hover(theme);
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
                        let now = crate::usage::now();
                        let limiting = snapshot.limiting();
                        let remaining = limiting.and_then(|row| row.remaining);
                        let pace = limiting.and_then(|row| row.pace(now));
                        let description = ring_description(provider, snapshot);
                        let left = left_label(limiting, now);
                        let ring = ring(
                            remaining.unwrap_or(0.),
                            pace,
                            fill_color(remaining, snapshot.outdated, theme),
                            theme.sidebar,
                            theme,
                        );
                        // Expanded: a side row (ring, name, what's left). Rail: the ring over its name.
                        let trigger = if self.rail {
                            v_flex()
                                .w(px(56.))
                                .py_1p5()
                                .gap_1()
                                .items_center()
                                .rounded(theme.radius)
                                .text_xs()
                                .text_color(theme.sidebar_foreground)
                                .child(ring)
                                .child(provider.name())
                        } else {
                            side_row(theme)
                                .mx_0()
                                .text_color(theme.sidebar_foreground)
                                .child(ring)
                                .child(div().flex_1().child(provider.name()))
                                .child(
                                    div()
                                        .text_xs()
                                        .when_some(
                                            label_color(remaining, snapshot.outdated, theme),
                                            |d, c| d.text_color(c),
                                        )
                                        .child(left),
                                )
                        };
                        let (snapshot, daily) = (
                            snapshot.clone(),
                            self.usage_activity[provider.index()].clone(),
                        );
                        HoverCard::new(format!("usage-card-{}", provider.id()))
                            .anchor(Anchor::BottomLeft)
                            .trigger(
                                trigger
                                    .id(format!("usage-trigger-{}", provider.id()))
                                    .role(Role::Button)
                                    .aria_label(description)
                                    .hover(move |d| d.bg(hover))
                                    .on_click(cx.listener(|s, _, w, cx| s.show_usage(w, cx)))
                                    .test_support(),
                            )
                            .content(move |_, _, cx| card(provider, &snapshot, &daily, cx))
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

    pub(super) fn usage_summary(&self, now: f64) -> crate::usage::summary::combined::Summary {
        crate::usage::summary::combined::Summary::from_sources(
            self.usage_providers
                .iter()
                .map(|provider| crate::usage::summary::combined::Source {
                    id: provider.id(),
                    name: provider.name(),
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
                "Pokémon companion"
            } else {
                "Account usage"
            })
            .aria_description(if companion {
                "Raise Pokémon with coding usage. Tab to move between controls. Escape to close."
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
            .when(companion && !recap, |d| {
                d.child(self.render_pokemon_panel(cx))
            })
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
                                .label("Refresh")
                                .icon(IconName::RefreshCw)
                                .on_click(cx.listener(|s, _, _, cx| s.refresh_usage(cx))),
                        ),
                )
            })
            .when(!pokemon && !recap, |d| {
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
                d.child(self.render_usage_totals(&summary, cx))
                    .child(self.render_month_trend(
                        &summary.month_daily,
                        &series,
                        &registry,
                        crate::usage::summary::local_date(now),
                        shows_cost,
                        cx,
                    ))
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
                            section(
                                provider,
                                &self.usage[i],
                                &self.usage_activity[i],
                                &self.usage_reports[i],
                                cx,
                            )
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

/// The calendar is here too, so its daily values are reachable without a pointer.
fn section(
    provider: ProviderHandle,
    snapshot: &Snapshot,
    daily: &Daily,
    report: &crate::usage::logs::Report,
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
        .gap_2()
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
                reset.push_str(tier.label(crate::pokemon::format::Language::En));
                if let Some(elapsed) = elapsed.filter(|p| *p >= 0.1) {
                    let delta = crate::usage::pace::rounded_delta(100. - remaining, elapsed);
                    if let Some(text) =
                        crate::usage::pace::delta_text(delta, crate::pokemon::format::Language::En)
                    {
                        reset.push_str(&format!(" ({text})"));
                    }
                }
            }
            v_flex()
                .id(format!("usage-{}-{ix}", provider.id()))
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
                        .child(reset),
                )
                .test_support()
        }))
        .child(
            div()
                .text_xs()
                .child(heatmap(provider, daily, crate::usage::now(), theme)),
        )
        .when(!report.entries.is_empty() || report.error.is_some(), |d| {
            d.child(companion_accounting(provider, report, cx))
        })
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
    if report.entries.is_empty() && report.error.is_some() {
        return div()
            .id(format!("usage-scan-error-{}", provider.id()))
            .text_xs()
            .aria_label("Local usage unavailable; refresh failed")
            .child("Local usage unavailable; refresh failed")
            .test_support()
            .into_any_element();
    }
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
        .when(report.error.is_some(), |d| {
            d.child(
                div()
                    .id(format!("usage-scan-error-{}", provider.id()))
                    .aria_label("Local usage refresh failed; showing previous totals")
                    .child("Local usage refresh failed; showing previous totals")
                    .test_support(),
            )
        })
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

/// What's left: `sidebar_foreground` like the rest of the navigation, `warning` / `danger`
/// fills when low. Never `primary`, which is reserved for focus, running agents and the
/// primary button.
fn fill_color(remaining: Option<f64>, outdated: bool, theme: &Theme) -> Hsla {
    match remaining {
        _ if outdated => faint(theme),
        Some(n) if n <= 5. => theme.danger,
        Some(n) if n <= 20. => theme.warning,
        _ => theme.sidebar_foreground,
    }
}

fn pace_color(remaining: f64, elapsed: Option<f64>, outdated: bool, theme: &Theme) -> Hsla {
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

/// The footer label's text: the brighter `*_foreground` at `fill_color`'s thresholds.
fn label_color(remaining: Option<f64>, outdated: bool, theme: &Theme) -> Option<Hsla> {
    match remaining {
        _ if outdated => None,
        Some(n) if n <= 5. => Some(theme.danger_foreground),
        Some(n) if n <= 20. => Some(theme.warning_foreground),
        _ => None,
    }
}

/// "62% left · 5h"; when low, the reset instead: "18% left · resets 3d".
fn left_label(row: Option<&Allowance>, now: f64) -> String {
    let Some((row, n)) = row.and_then(|row| Some((row, row.remaining?))) else {
        return "Unknown".into();
    };
    match row.resets_at {
        Some(at) if n <= 20. => format!("{}% left · resets {}", n.ceil(), countdown(at - now)),
        _ => format!("{}% left · {}", n.ceil(), short_window(&row.label)),
    }
}

/// "5 hours" → "5h", "Weekly" → "week", "gpt-5 · Weekly" → "gpt-5 week", "Fable" as is.
fn short_window(label: &str) -> String {
    label
        .split(" · ")
        .map(short_part)
        .collect::<Vec<_>>()
        .join(" ")
}

fn short_part(label: &str) -> String {
    match label.split_once(' ') {
        _ if label == "Weekly" => "week".into(),
        Some((n, "hour" | "hours")) => format!("{n}h"),
        Some((n, "days")) => format!("{n}d"),
        Some((n, "minutes")) => format!("{n}m"),
        _ if label.chars().count() > 10 => {
            format!("{}…", label.chars().take(9).collect::<String>())
        }
        _ => label.into(),
    }
}

/// A 16px ring of what's left, clockwise from the top, on a track tinted like the fill so an
/// empty red ring still reads as red. A tick crossing it, knocked out of the band in the row
/// background `bg`, marks what would be left at an even pace: an arc past the tick is ahead
/// of the limit.
fn ring(left: f64, pace: Option<f64>, fill: Hsla, bg: Hsla, theme: &Theme) -> impl IntoElement {
    let (track, tick) = (fill.opacity(0.25), theme.sidebar_foreground);
    let (left, pace) = (left as f32, pace.map(|p| p as f32));
    canvas(
        |_, _, _| {},
        move |b, _, w, _| {
            let band = Arc::new().inner_radius(4.).outer_radius(7.5);
            band.paint(&ArcData::new(&(), 0, 100., 0., TAU), track, &b, w);
            if left > 0. {
                let end = left / 100. * TAU;
                band.paint(&ArcData::new(&(), 1, left, 0., end), fill, &b, w);
            }
            if let Some(p) = pace {
                let (sin, cos) = (p / 100. * TAU).sin_cos();
                let at = |r: f32| b.center() + point(px(r * sin), px(-r * cos));
                // The knockout stays within the band, so a hovered row's background can't show it.
                for (width, from, to, color) in [(4.5, 4., 7.5, bg), (1.5, 2.5, 8., tick)] {
                    let mut line = PathBuilder::stroke(px(width));
                    line.move_to(at(from));
                    line.line_to(at(to));
                    if let Ok(path) = line.build() {
                        w.paint_path(path, color);
                    }
                }
            }
        },
    )
    .size(px(16.))
    .flex_none()
}

/// One allowance: "5 hours · 62% left · resets in 2h 14m (Thu 11:00 PM) · even pace 40%".
fn limit_line(row: &Allowance, now: f64) -> String {
    let mut text = format!(
        "{} · {}",
        row.label,
        row.remaining.map_or_else(
            || "Remaining unknown".into(),
            |n| format!("{}% left", n.ceil())
        )
    );
    if let Some(at) = row.resets_at {
        text.push_str(&format!(
            " · resets in {} ({})",
            countdown(at - now),
            short_time(at)
        ));
    }
    if let Some(p) = row.pace(now) {
        text.push_str(&format!(" · even pace {}%", p.round()));
    }
    text
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

fn ring_description(provider: ProviderHandle, snapshot: &Snapshot) -> String {
    let mut text = match snapshot.limiting() {
        Some(row) => format!(
            "{} · {} · {}% remaining · {}",
            provider.name(),
            row.label,
            row.remaining.unwrap().ceil(),
            resets(row.resets_at)
        ),
        None => format!("{} · Remaining unknown", provider.name()),
    };
    if let Some(p) = snapshot
        .limiting()
        .and_then(|row| row.pace(crate::usage::now()))
    {
        text.push_str(&even_pace(p));
    }
    text.push_str(&status_note(snapshot));
    text
}

fn status_note(snapshot: &Snapshot) -> String {
    let mut text = String::new();
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
        text.push_str(&format!(" · Last checked {}", local_time(at)));
    }
    text
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

/// Weekday and time, e.g. "Thu 11:00 PM", in the user's locale.
fn short_time(seconds: f64) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str("EEEjmm"));
    formatter.setTimeZone(Some(&NSTimeZone::localTimeZone()));
    formatter
        .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(seconds))
        .to_string()
}

/// Time until a reset: "3d", "2h 14m", "5m".
fn countdown(seconds: f64) -> String {
    let m = (seconds.max(0.) / 60.).ceil() as u64;
    match (m / 1440, m / 60 % 24, m % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, _, _) => format!("{d}d"),
    }
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

/// Formats [`local_day`]s for display from a localized template: `"EEEMMMd"` → "Tue, Oct 28".
fn day_labels(template: &str) -> impl Fn(i64) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
    formatter.setTimeZone(Some(&NSTimeZone::timeZoneForSecondsFromGMT(0)));
    move |day| {
        formatter
            .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(day as f64 * 86400.))
            .to_string()
    }
}

/// (year, month 1-12, day 1-31) of a [`local_day`] in the Gregorian calendar, whatever the locale.
fn civil(day: i64) -> (i64, u32, u32) {
    // Hinnant's civil_from_days.
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (
        (doy - (153 * mp + 2) / 5 + 1) as u32,
        (if mp < 10 { mp + 3 } else { mp - 9 }) as u32,
    );
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const CELL: f32 = 11.;
const GAP: f32 = 3.;

/// Heatmap level 0–4: 0 for no tokens, else the quartile of non-zero days (GitHub's cut).
fn levels(tokens: &[u64]) -> impl Fn(u64) -> usize {
    let mut busy: Vec<u64> = tokens.iter().copied().filter(|n| *n > 0).collect();
    busy.sort_unstable();
    // Cut below the maximum so the busiest day always reaches level 4.
    let top = busy.len().saturating_sub(1);
    let cut = |q: usize| busy[..top].get(top * q / 4).copied().unwrap_or(0);
    let cuts = [cut(1), cut(2), cut(3)];
    move |n| match n {
        0 => 0,
        n => 1 + cuts.iter().filter(|c| n > **c).count(),
    }
}

fn level_color(level: usize, theme: &Theme) -> Hsla {
    match level {
        0 => theme.muted,
        n => theme.foreground.opacity([0.25, 0.45, 0.7, 1.][n - 1]),
    }
}

fn cell(level: usize, theme: &Theme) -> Div {
    div()
        .size(px(CELL))
        .flex_none()
        .rounded(px(2.))
        .bg(level_color(level, theme))
}

/// The footer hover card: each allowance, then a GitHub-style calendar of daily tokens.
fn card(
    provider: ProviderHandle,
    snapshot: &Snapshot,
    daily: &Daily,
    cx: &App,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let now = crate::usage::now();
    let muted = theme.muted_foreground;
    v_flex()
        .id(format!("usage-card-{}", provider.id()))
        .w(px(304.))
        .gap_2()
        .text_xs()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(provider.name()),
        )
        .children(
            snapshot
                .allowances
                .iter()
                .map(|row| div().text_color(muted).child(limit_line(row, now))),
        )
        .children(
            Some(status_note(snapshot).trim_start_matches(" · ").to_string())
                .filter(|t| !t.is_empty())
                .map(|t| div().text_color(muted).child(t)),
        )
        .child(heatmap(provider, daily, now, theme))
        .test_support()
}

/// Each month over the column holding its 1st; the first column's only if it has room.
fn month_marks(first: i64, today: i64) -> Vec<(i64, i64)> {
    let mut months: Vec<(i64, i64)> = vec![(0, first)];
    for day in first + 1..=today {
        if civil(day).2 == 1 {
            months.push(((day - first) / 7, day));
        }
    }
    if months.get(1).is_some_and(|(week, _)| *week < 3) {
        months.remove(0);
    }
    months
}

fn heatmap(provider: ProviderHandle, daily: &Daily, now: f64, theme: &Theme) -> AnyElement {
    let today = activity::local_day(now);
    // Columns run Monday to Sunday; the last one holds today.
    let first = today - (today + 3).rem_euclid(7) - (WEEKS - 1) * 7;
    let at = |day: i64| {
        usize::try_from(day - daily.first_day)
            .ok()
            .and_then(|i| daily.tokens.get(i).copied())
            .unwrap_or(0)
    };
    let shown: Vec<u64> = (first..=today).map(at).collect();
    let total = shown.iter().copied().fold(0u64, u64::saturating_add);
    let empty = match () {
        _ if daily.tokens.is_empty() => Some("Reading local logs…"),
        _ if total == 0 => Some("No local activity found"),
        _ => None,
    };
    if let Some(text) = empty {
        return div()
            .text_color(faint(theme))
            .child(text)
            .into_any_element();
    }
    let level = levels(&shown);
    let busiest = (first..=today).max_by_key(|d| (at(*d), -d)).unwrap();
    let step = CELL + GAP;
    let gutter = 28.;
    let months = month_marks(first, today);
    let (long, month) = (day_labels("EEEMMMd"), day_labels("MMM"));
    v_flex()
        .gap_2()
        .child(format!(
            "{} tokens in {WEEKS} weeks · busiest {}",
            tokens(total),
            long(busiest)
        ))
        .child(
            v_flex()
                .id(SharedString::from(format!(
                    "usage-heatmap-{}",
                    provider.id()
                )))
                .role(Role::Group)
                .aria_label(format!("{} tokens per day", provider.name()))
                .gap(px(GAP))
                .text_color(theme.muted_foreground)
                .child(div().relative().h(px(16.)).children(months.into_iter().map(
                    |(week, day)| {
                        div()
                            .absolute()
                            .left(px(gutter + week as f32 * step))
                            .child(month(day))
                    },
                )))
                .children((0..7i64).map(|row| {
                    h_flex()
                        .gap(px(GAP))
                        .h(px(CELL))
                        .child(
                            div()
                                .w(px(gutter - GAP))
                                .line_height(px(CELL))
                                .when(row % 2 == 0 && row < 6, |d| {
                                    d.child(["Mon", "Wed", "Fri"][row as usize / 2])
                                }),
                        )
                        .children((0..WEEKS).map(|week| {
                            let day = first + week * 7 + row;
                            if day > today {
                                return div().size(px(CELL)).flex_none().into_any_element();
                            }
                            let n = at(day);
                            let tip = format!(
                                "{} · {}",
                                long(day),
                                match n {
                                    0 => "No activity".into(),
                                    n => format!("{} tokens", tokens(n)),
                                }
                            );
                            cell(level(n), theme)
                                .id(SharedString::from(format!(
                                    "usage-day-{}-{day}",
                                    provider.id()
                                )))
                                .role(Role::Label)
                                .aria_label(tip.clone())
                                .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                                .into_any_element()
                        }))
                })),
        )
        .child(
            h_flex()
                .justify_end()
                .gap(px(GAP))
                .text_color(theme.muted_foreground)
                .child(div().mr_1().child("Less"))
                .children((0..5).map(|n| cell(n, theme)))
                .child(div().ml_1().child("More")),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        Allowance, Failure, Provider, Snapshot, Theme, fill_color, label_color, left_label, levels,
        limit_line, ring_description,
    };
    use super::{civil, countdown, local_time, month_marks, tokens};
    use gpui_kit::rgb;

    #[test]
    fn civil_dates_ignore_the_locale() {
        assert_eq!(civil(20390), (2025, 10, 29));
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11016), (2000, 2, 29));
    }

    #[test]
    fn month_marks_drop_a_leading_label_without_room() {
        // 20372 is Oct 11, 2025 and 20393 is Nov 1.
        assert_eq!(month_marks(20372, 20400), [(0, 20372), (3, 20393)]);
        assert_eq!(month_marks(20382, 20400), [(1, 20393)]);
        assert_eq!(month_marks(20390, 20392), [(0, 20390)]);
    }

    #[test]
    fn reset_and_pace_text() {
        assert_eq!(super::resets(None), "Reset time unknown");
        assert!(super::resets(Some(0.)).starts_with("Resets "));
        assert_eq!(super::even_pace(39.6), " · Even pace: 40% left");
        assert!(!local_time(1e9).is_empty());
    }

    #[test]
    fn usage_ring_thresholds_and_unknown_stale_labels() {
        let mut theme = Theme::default();
        theme.warning = rgb(0x654321).into();
        theme.danger = rgb(0xff0000).into();
        theme.sidebar_foreground = rgb(0x888888).into();
        for (remaining, expected) in [
            (Some(21.), theme.sidebar_foreground),
            (Some(20.), theme.warning),
            (Some(5.1), theme.warning),
            (Some(5.), theme.danger),
            (Some(0.), theme.danger),
            (None, theme.sidebar_foreground),
        ] {
            assert_eq!(fill_color(remaining, false, &theme), expected);
            assert_eq!(
                fill_color(remaining, true, &theme),
                crate::ui::faint(&theme)
            );
        }
        let mut snapshot = Snapshot::default();
        snapshot.finish(Ok(vec![
            crate::usage::Allowance {
                label: "Weekly".into(),
                remaining: Some(0.),
                resets_at: Some(0.),
                window: None,
            },
            crate::usage::Allowance {
                label: "Model".into(),
                remaining: None,
                resets_at: None,
                window: None,
            },
        ]));
        snapshot.finish(Err(Failure::Temporary));
        snapshot.begin();
        let text = ring_description(Provider::Claude.into(), &snapshot);
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
        assert!(ring_description(Provider::Claude.into(), &snapshot).contains("Remaining unknown"));

        let row = |label: &str, remaining: f64| Allowance {
            label: label.into(),
            remaining: Some(remaining),
            resets_at: Some(1000. + 2. * 3600. + 14. * 60.),
            window: Some(5. * 3600.),
        };
        for (label, remaining, expected) in [
            ("5 hours", 61.2, "62% left · 5h"),
            ("Weekly", 21., "21% left · week"),
            (
                "gpt-5.5-codex-max · Weekly",
                50.,
                "50% left · gpt-5.5-c… week",
            ),
            ("Fable", 30., "30% left · Fable"),
            ("5 hours", 4., "4% left · resets 2h 14m"),
        ] {
            assert_eq!(left_label(Some(&row(label, remaining)), 1000.), expected);
        }
        let mut weekly = row("Weekly", 18.);
        weekly.resets_at = Some(1000. + 3. * 86400. + 60.);
        assert_eq!(left_label(Some(&weekly), 1000.), "18% left · resets 3d");
        assert_eq!(left_label(None, 0.), "Unknown");
        assert_eq!(label_color(Some(21.), false, &theme), None);
        assert_eq!(
            label_color(Some(20.), false, &theme),
            Some(theme.warning_foreground)
        );
        assert_eq!(
            label_color(Some(5.), false, &theme),
            Some(theme.danger_foreground)
        );
        let line = limit_line(&row("5 hours", 61.2), 1000. + 3600.);
        assert!(
            line.starts_with("5 hours · 62% left · resets in 1h 14m ("),
            "{line}"
        );
        assert!(line.ends_with(") · even pace 25%"), "{line}");
        snapshot.finish(Ok(vec![row("5 hours", 61.234)]));
        let text = ring_description(Provider::Claude.into(), &snapshot);
        assert!(text.contains("· 62% remaining ·"), "{text}");
        assert_eq!(countdown(59.), "1m");
        assert_eq!(tokens(412_345), "412k");
        assert_eq!(tokens(38_200_000), "38.2M");
        assert_eq!(tokens(999_999), "1.0M");
        let level = levels(&[0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 6, 7, 8].map(&level),
            [0, 1, 1, 2, 2, 3, 3, 4, 4]
        );
        assert_eq!([0, 5, 9].map(levels(&[0, 5, 9])), [0, 1, 4]);
        assert_eq!(levels(&[7])(7), 4);
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
            assert!(w.try_find("usage-trigger-future-tool").is_some());
        })
        .unwrap();
    }
    // upstream: UsageRecapTests.swift::testTheFirstRefreshAlreadyFillsTheLedger
    // upstream: UsageRecapTests.swift::testARefreshKeepsLastYearAndDropsTheYearBefore
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

    // upstream: PaceTierTests.swift::testTheFallbackTiersMatchTheThresholdColors
    // upstream: LimitPaceTests.swift::testEveryPopoverLimitBarFollowsTheDisplayMode
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
