//! The month chart used by the native Usage popover. Future days are layout, never ledger data.
use super::*;
use crate::pokemon::format::compact;
use crate::usage::summary::{Day, ProviderSeries, dates};
use chrono::{Datelike, Local, Months, NaiveDate, TimeZone};
use objc2_foundation::{NSCalendar, NSLocale};
use std::collections::BTreeMap;

pub(super) const TRACK: f32 = 26.;
pub(super) const BASELINE: f32 = 1.5;
const SPACING: f32 = 1.5;
const PALETTE: [u32; 14] = [
    0xd97857, 0x5c9ef2, 0x73c785, 0xc78ceb, 0xf2c252, 0x59c7cc, 0xed82ad, 0x9ea3b3, 0xe0cc99,
    0xb3d161, 0x996b4d, 0x4d998c, 0xcc5966, 0x667acc,
];

#[derive(Clone, Debug)]
pub(super) struct Column {
    pub date: NaiveDate,
    pub tokens: u64,
    pub future: bool,
}
pub(super) fn columns(series: &[Day], today: NaiveDate) -> Vec<Column> {
    let start = today.with_day(1).unwrap();
    let mut totals = BTreeMap::<_, u64>::new();
    for day in series {
        let n = totals.entry(day.date).or_default();
        *n = n.saturating_add(day.bucket.total());
    }
    dates(start, start + Months::new(1))
        .map(|date| Column {
            date,
            tokens: if date > today {
                0
            } else {
                totals.get(&date).copied().unwrap_or(0)
            },
            future: date > today,
        })
        .collect()
}
pub(super) fn bar_height(tokens: u64, peak: u64) -> f32 {
    if peak == 0 || tokens == 0 {
        BASELINE
    } else {
        (TRACK * (tokens as f64 / peak as f64).min(1.) as f32).max(BASELINE)
    }
}
pub(super) fn axis_label(date: NaiveDate, today: NaiveDate) -> Option<String> {
    let day = date.day();
    if date == today {
        return Some(day.to_string());
    }
    ((day == 1 || day % 7 == 0) && day.abs_diff(today.day()) >= 3).then(|| day.to_string())
}
fn native_date(date: NaiveDate) -> Option<objc2::rc::Retained<NSDate>> {
    let seconds = Local
        .from_local_datetime(&date.and_hms_opt(12, 0, 0)?)
        .earliest()?
        .timestamp();
    Some(NSDate::dateWithTimeIntervalSince1970(seconds as f64))
}
pub(super) fn is_weekend(date: NaiveDate, locale: Option<&str>) -> bool {
    let current = NSCalendar::currentCalendar();
    let calendar = if locale.is_some() {
        NSCalendar::calendarWithIdentifier(&current.calendarIdentifier()).unwrap_or(current)
    } else {
        current
    };
    if let Some(locale) = locale {
        calendar.setLocale(Some(&NSLocale::localeWithLocaleIdentifier(
            &NSString::from_str(locale),
        )));
    }
    native_date(date).is_some_and(|date| calendar.isDateInWeekend(&date))
}
pub(super) struct DateText {
    formatter: objc2::rc::Retained<NSDateFormatter>,
}
impl DateText {
    pub fn new(template: &str) -> Self {
        let formatter = NSDateFormatter::new();
        formatter.setLocale(Some(&NSLocale::localeWithLocaleIdentifier(
            &NSString::from_str("en_US"),
        )));
        formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
        Self { formatter }
    }
    pub fn stamp(&self, date: NaiveDate) -> String {
        native_date(date)
            .map(|date| self.formatter.stringFromDate(&date).to_string())
            .unwrap_or_default()
    }
}
pub(super) fn ordered(providers: &[ProviderSeries]) -> Vec<&ProviderSeries> {
    let total = |p: &ProviderSeries| {
        p.days
            .as_ref()
            .into_iter()
            .flatten()
            .map(|d| d.bucket.total())
            .fold(0u64, u64::saturating_add)
    };
    let mut providers: Vec<_> = providers.iter().filter(|p| total(p) > 0).collect();
    providers.sort_by(|a, b| total(b).cmp(&total(a)).then_with(|| a.id.cmp(&b.id)));
    providers
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Segment {
    pub id: String,
    pub tokens: u64,
    pub height: f64,
}
pub(super) fn segments(date: NaiveDate, stack: &[&ProviderSeries], height: f64) -> Vec<Segment> {
    let parts: Vec<_> = stack
        .iter()
        .filter_map(|p| {
            p.days
                .as_ref()?
                .iter()
                .find(|d| d.date == date)
                .map(|d| (p.id.as_str(), d.bucket.total()))
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    let total: f64 = parts.iter().map(|(_, n)| *n as f64).sum();
    let mut placed = 0.;
    let mut running = 0.;
    parts
        .iter()
        .enumerate()
        .map(|(i, (id, n))| {
            running += *n as f64;
            let top = if i + 1 == parts.len() {
                height
            } else {
                height * (running / total)
            };
            let segment = Segment {
                id: (*id).into(),
                tokens: *n,
                height: (top - placed).max(0.),
            };
            placed = top;
            segment
        })
        .collect()
}
pub(super) fn color_slot(id: &str, registry: &[String], palette_count: usize) -> usize {
    registry.iter().position(|p| p == id).unwrap_or(0) % palette_count.max(1)
}
fn color(id: &str, registry: &[String]) -> Hsla {
    gpui_kit::rgb(PALETTE[color_slot(id, registry, PALETTE.len())]).into()
}
pub(super) fn readout(
    series: &[Day],
    today: NaiveDate,
    shows_cost: bool,
    labels: &DateText,
) -> String {
    let Some(day) = series.iter().find(|d| d.date == today) else {
        return String::new();
    };
    format!(
        "{} {}{}",
        labels.stamp(today),
        compact(day.bucket.total()),
        if shows_cost {
            format!(" {}", day.bucket.cost_text(true))
        } else {
            String::new()
        }
    )
}
pub(super) fn hover_text(
    date: NaiveDate,
    series: &[Day],
    stack: &[&ProviderSeries],
    shows_cost: bool,
    labels: &DateText,
) -> Option<String> {
    let day = series.iter().find(|d| d.date == date)?;
    let mut text = format!(
        "{} · {} tokens{}",
        labels.stamp(date),
        compact(day.bucket.total()),
        if shows_cost {
            format!(" · {}", day.bucket.cost_text(false))
        } else {
            String::new()
        }
    );
    if stack.len() > 1 {
        for p in stack {
            if let Some(day) = p
                .days
                .as_ref()
                .and_then(|ds| ds.iter().find(|d| d.date == date))
                .filter(|d| d.bucket.total() > 0)
            {
                text.push_str(&format!(
                    "\n{} · {}{}",
                    p.name,
                    compact(day.bucket.total()),
                    if shows_cost && p.reports_cost {
                        format!(" · {}", day.bucket.cost_text(false))
                    } else {
                        String::new()
                    }
                ));
            }
        }
    }
    Some(text)
}

impl Shark {
    pub(super) fn render_month_trend(
        &self,
        series: &[Day],
        providers: &[ProviderSeries],
        registry: &[String],
        today: NaiveDate,
        shows_cost: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let columns = columns(series, today);
        let peak = columns.iter().map(|c| c.tokens).max().unwrap_or(0);
        if peak == 0 {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let stack = ordered(providers);
        let stacked = stack.len() > 1;
        let labels = DateText::new("MdE");
        let caption = readout(series, today, shows_cost, &labels);
        let hovered = self.usage_panel.as_ref().and_then(|p| p.hovered_day);
        let tip = hovered.and_then(|date| hover_text(date, series, &stack, shows_cost, &labels));
        let bars = h_flex()
            .id("usage-trend-bars")
            .h(px(TRACK))
            .items_end()
            .gap(px(SPACING))
            .children(columns.iter().map(|column| {
                let date = column.date;
                let height = bar_height(column.tokens, peak);
                let parts = if stacked && column.tokens > 0 {
                    segments(date, &stack, height as f64)
                } else {
                    vec![]
                };
                let label = hover_text(date, series, &stack, shows_cost, &labels)
                    .unwrap_or_else(|| format!("{}, 0 tokens", labels.stamp(date)));
                div()
                    .id(format!("usage-trend-day-{date}"))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_end()
                    .when(!column.future, |d| {
                        d.role(Role::Label).aria_label(label).on_hover(cx.listener(
                            move |s, inside: &bool, _, cx| {
                                if let Some(panel) = s.usage_panel.as_mut() {
                                    panel.hovered_day = inside.then_some(date);
                                }
                                cx.notify();
                            },
                        ))
                    })
                    .when(!column.future, |d| {
                        d.child(if parts.is_empty() {
                            div()
                                .id(format!("usage-trend-bar-{date}"))
                                .w_full()
                                .h(px(height))
                                .rounded(px(1.))
                                .bg(if date == today {
                                    theme.primary
                                } else {
                                    theme.foreground.opacity(if column.tokens == 0 {
                                        0.18
                                    } else {
                                        0.45
                                    })
                                })
                                .test_support()
                                .into_any_element()
                        } else {
                            v_flex()
                                .id(format!("usage-trend-bar-{date}"))
                                .w_full()
                                .h(px(height))
                                .rounded(px(1.))
                                .overflow_hidden()
                                .children(parts.iter().rev().map(|part| {
                                    div()
                                        .id(format!("usage-trend-segment-{date}-{}", part.id))
                                        .w_full()
                                        .h(px(part.height as f32))
                                        .flex_none()
                                        .bg(color(&part.id, registry).opacity(
                                            if date == today || hovered == Some(date) {
                                                1.
                                            } else {
                                                0.55
                                            },
                                        ))
                                        .test_support()
                                }))
                                .test_support()
                                .into_any_element()
                        })
                    })
                    .test_support()
            }))
            .test_support();
        v_flex()
            .id("usage-trend")
            .gap(px(3.))
            .w_full()
            .child(
                h_flex()
                    .id("usage-trend-caption")
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .h(px(18.))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child("Daily"),
                    )
                    .child(
                        div()
                            .id("usage-trend-readout")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .role(Role::Label)
                            .aria_label(caption.clone())
                            .child(caption)
                            .test_support(),
                    )
                    .when(columns.iter().filter(|c| c.tokens > 0).count() > 1, |d| {
                        d.child(
                            div()
                                .id("usage-trend-peak")
                                .flex_none()
                                .text_color(theme.muted_foreground)
                                .child(format!("Peak {}", compact(peak)))
                                .test_support(),
                        )
                    })
                    .test_support(),
            )
            .child(div().relative().child(bars).when_some(tip, |d, tip| {
                d.child(
                    div()
                        .id("usage-trend-tooltip")
                        .absolute()
                        .bottom(px(TRACK + 4.))
                        .left_0()
                        .max_w_full()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .role(Role::Label)
                        .aria_label(tip.clone())
                        .child(tip)
                        .test_support(),
                )
            }))
            .child(
                h_flex()
                    .id("usage-trend-ticks")
                    .h(px(1.5))
                    .gap(px(SPACING))
                    .children(columns.iter().map(|column| {
                        div()
                            .id(format!("usage-trend-weekend-{}", column.date))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when(!column.future && is_weekend(column.date, None), |d| {
                                d.bg(theme.foreground.opacity(0.5))
                            })
                            .test_support()
                    }))
                    .test_support(),
            )
            .child(
                h_flex()
                    .id("usage-trend-axis")
                    .h(px(14.))
                    .gap(px(SPACING))
                    .children(columns.iter().map(|column| {
                        div().flex_1().min_w_0().relative().h_full().when_some(
                            axis_label(column.date, today),
                            |d, label| {
                                d.child(
                                    div()
                                        .id(format!("usage-trend-axis-{}", column.date))
                                        .absolute()
                                        .left_0()
                                        .text_size(px(9.))
                                        .line_height(px(14.))
                                        .whitespace_nowrap()
                                        .w(px(18.))
                                        .text_color(if column.date == today {
                                            theme.primary
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(label)
                                        .test_support(),
                                )
                            },
                        )
                    }))
                    .test_support(),
            )
            .when(stacked, |d| {
                d.child(
                    div()
                        .id("usage-trend-legend")
                        .flex()
                        .flex_wrap()
                        .gap_x_2()
                        .gap_y_1()
                        .children(stack.iter().map(|provider| {
                            h_flex()
                                .id(format!("usage-trend-legend-{}", provider.id))
                                .gap_1()
                                .items_center()
                                .text_size(px(10.))
                                .child(
                                    div()
                                        .size(px(6.))
                                        .rounded(px(1.))
                                        .bg(color(&provider.id, registry)),
                                )
                                .child(div().truncate().child(provider.name.clone()))
                                .test_support()
                        }))
                        .test_support(),
                )
            })
            .test_support()
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
