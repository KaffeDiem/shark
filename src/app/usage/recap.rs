//! A native, coverage-aware usage recap. It shares the persisted ledger with the month chart.
use super::*;
use crate::pokemon::format::{Language, compact, grouped};
use crate::pokemon::{Mon, forms};
use crate::usage::summary::{Graduation, Meter, Period, Recap, Scope, local_date};
use chrono::{Datelike, Days};
use gpui_kit::component::{Disableable as _, Selectable as _};
use objc2_foundation::NSCalendar;
use trend::DateText;

fn mon_id(mon: &Mon) -> String {
    mon.profile
        .as_ref()
        .map(|p| p.instance_id.clone())
        .unwrap_or_else(|| format!("{}-{}-{}", mon.line.base_id, mon.caught_at, mon.nature))
}
pub(super) fn graduate_name(mon: &Mon) -> String {
    forms::display_name(
        &mon.line.name(mon.species_id()),
        mon.species_id(),
        mon.form.as_ref(),
        None,
    )
}
pub(super) fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Week => "Week",
        Scope::Month => "Month",
        Scope::Year => "Year",
    }
}
pub(super) fn current_label(scope: Scope, language: Language) -> &'static str {
    let index = language as usize;
    match scope {
        Scope::Week => [
            "이번 주",
            "This week",
            "今週",
            "Esta semana",
            "Cette semaine",
            "Esta semana",
            "Diese Woche",
            "Эта неделя",
        ][index],
        Scope::Month => [
            "이번 달",
            "This month",
            "今月",
            "Este mes",
            "Ce mois-ci",
            "Este mês",
            "Dieser Monat",
            "Этот месяц",
        ][index],
        Scope::Year => [
            "올해",
            "This year",
            "今年",
            "Este año",
            "Cette année",
            "Este ano",
            "Dieses Jahr",
            "Этот год",
        ][index],
    }
}
pub(super) fn meter_label(meter: &Meter, scope: Scope, language: Language) -> String {
    let labels = DateText::new(
        language,
        if scope == Scope::Year {
            "MMMMy"
        } else {
            "EEEEdMMM"
        },
    );
    let missing = [
        "데이터 없음",
        "no data",
        "データなし",
        "sin datos",
        "aucune donnée",
        "sem dados",
        "keine Daten",
        "нет данных",
    ][language as usize];
    format!(
        "{}, {}",
        labels.stamp(meter.date),
        if meter.has_data {
            grouped(meter.tokens, ',')
        } else {
            missing.into()
        }
    )
}
fn period_label(period: Period) -> String {
    match period.scope {
        Scope::Year => period.start.year().to_string(),
        Scope::Month => DateText::new(Language::En, "MMMMy").stamp(period.start),
        Scope::Week => format!(
            "{} – {}",
            DateText::new(Language::En, "dMMM").stamp(period.start),
            DateText::new(Language::En, "dMMMy").stamp(period.end - Days::new(1))
        ),
    }
}
fn bucket_tick(meter: &Meter, scope: Scope) -> String {
    match scope {
        Scope::Week => DateText::new(Language::En, "EEE")
            .stamp(meter.date)
            .chars()
            .take(2)
            .collect(),
        Scope::Month => {
            if [1, 8, 15, 22, 29].contains(&meter.date.day()) {
                meter.date.day().to_string()
            } else {
                String::new()
            }
        }
        Scope::Year => DateText::new(Language::En, "MMMMM").stamp(meter.date),
    }
}

impl Shark {
    fn recap_content(&self) -> (Recap, Vec<&Mon>) {
        let now = self.usage_view_now();
        let today = local_date(now);
        let (scope, offset) = self
            .usage_panel
            .as_ref()
            .and_then(|p| p.recap)
            .unwrap_or((Scope::Week, 0));
        let first = (NSCalendar::currentCalendar().firstWeekday() as u32 + 5) % 7;
        let period = Period::containing(scope, today, first).shifted(offset);
        let graduates: Vec<_> = self
            .state
            .pokemon
            .collection
            .iter()
            .filter(|m| m.graduated)
            .map(|m| Graduation {
                id: mon_id(m),
                date: local_date(m.caught_at as f64),
                released: m.released,
            })
            .collect();
        let recap = Recap::make(&self.state.usage_ledger, &graduates, period, today);
        let mons = recap
            .graduated
            .iter()
            .take(4)
            .filter_map(|g| {
                self.state
                    .pokemon
                    .collection
                    .iter()
                    .find(|mon| mon_id(mon) == g.id)
            })
            .collect();
        (recap, mons)
    }
    pub(in crate::app) fn warm_usage_recap(&mut self, cx: &mut Context<Self>) {
        if self.state.pokemon_disabled {
            return;
        }
        let (_, graduates) = self.recap_content();
        let mut mons: Vec<_> = graduates.into_iter().cloned().collect();
        if let Some(active) = &self.state.pokemon.active {
            mons.insert(0, active.clone());
        }
        self.load_recap_mons(&mons, cx);
    }
    pub(super) fn open_usage_recap(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.usage_panel.as_mut() {
            panel.recap = Some((Scope::Week, 0));
            panel.hovered_day = None;
        }
        self.warm_usage_recap(cx);
        cx.notify();
    }
    fn set_recap_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if let Some(panel) = self.usage_panel.as_mut() {
            panel.recap = Some((scope, 0));
        }
        self.warm_usage_recap(cx);
        cx.notify();
    }
    fn shift_recap(&mut self, delta: i32, cx: &mut Context<Self>) {
        let (recap, _) = self.recap_content();
        if delta < 0 && !recap.can_go_back {
            return;
        }
        if let Some(panel) = self.usage_panel.as_mut() {
            if let Some((scope, offset)) = panel.recap {
                panel.recap = Some((scope, offset.saturating_add(delta).min(0)));
            }
        }
        self.warm_usage_recap(cx);
        cx.notify();
    }
    pub(super) fn render_usage_recap(&self, cx: &Context<Self>) -> AnyElement {
        let (recap, graduates) = self.recap_content();
        let scope = recap.period.scope;
        let offset = self
            .usage_panel
            .as_ref()
            .and_then(|p| p.recap)
            .map_or(0, |p| p.1);
        let theme = cx.theme();
        let peak = recap
            .buckets
            .iter()
            .map(|b| b.tokens)
            .max()
            .unwrap_or(0)
            .max(1);
        v_flex()
            .id("usage-recap")
            .gap_3()
            .w_full()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        Button::new("usage-recap-back")
                            .ghost()
                            .xsmall()
                            .label("Back")
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(panel) = s.usage_panel.as_mut() {
                                    panel.recap = None;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .children(Scope::ALL.into_iter().map(|item| {
                                Button::new(format!("usage-recap-scope-{}", scope_name(item)))
                                    .ghost()
                                    .xsmall()
                                    .label(scope_name(item))
                                    .selected(scope == item)
                                    .on_click(
                                        cx.listener(move |s, _, _, cx| s.set_recap_scope(item, cx)),
                                    )
                            })),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        Button::new("usage-recap-previous")
                            .ghost()
                            .xsmall()
                            .label("Previous")
                            .disabled(!recap.can_go_back)
                            .on_click(cx.listener(|s, _, _, cx| s.shift_recap(-1, cx))),
                    )
                    .child(
                        Button::new("usage-recap-current")
                            .ghost()
                            .xsmall()
                            .label(current_label(scope, Language::En))
                            .disabled(offset == 0)
                            .on_click(cx.listener(move |s, _, _, cx| s.set_recap_scope(scope, cx))),
                    )
                    .child(
                        Button::new("usage-recap-next")
                            .ghost()
                            .xsmall()
                            .label("Next")
                            .disabled(offset >= 0)
                            .on_click(cx.listener(|s, _, _, cx| s.shift_recap(1, cx))),
                    ),
            )
            .child(
                v_flex()
                    .id("usage-recap-card")
                    .w_full()
                    .p_3()
                    .gap_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .child(
                        div()
                            .id("usage-recap-period")
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_xs()
                            .child(period_label(recap.period))
                            .test_support(),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .id("usage-recap-total")
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{} tokens", compact(recap.total)))
                                    .test_support(),
                            )
                            .when_some(recap.delta(), |d, delta| {
                                d.child(
                                    div()
                                        .id("usage-recap-delta")
                                        .text_xs()
                                        .child(format!(
                                            "{}{:.1}%",
                                            if delta >= 0. { "+" } else { "" },
                                            delta * 100.
                                        ))
                                        .test_support(),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .id("usage-recap-meters")
                            .items_end()
                            .gap(px(if scope == Scope::Month { 1.5 } else { 4. }))
                            .children(recap.buckets.iter().map(|meter| {
                                let label = meter_label(meter, scope, Language::En);
                                let lit = if meter.tokens > 0 && meter.has_data {
                                    ((meter.tokens as f64 / peak as f64) * 8.)
                                        .round()
                                        .clamp(1., 8.) as usize
                                } else {
                                    0
                                };
                                v_flex()
                                    .id(format!("usage-recap-meter-{}", meter.date))
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .role(Role::Label)
                                    .aria_label(label.clone())
                                    .tooltip(move |w, cx| Tooltip::new(label.clone()).build(w, cx))
                                    .children((0..8).map(|index| {
                                        div().w_full().h(px(4.)).bg(theme.foreground.opacity(
                                            if index >= 8 - lit {
                                                if meter.current { 1. } else { 0.6 }
                                            } else if meter.has_data {
                                                0.12
                                            } else {
                                                0.04
                                            },
                                        ))
                                    }))
                                    .child(
                                        div().relative().h(px(12.)).child(
                                            div()
                                                .id(format!("usage-recap-tick-{}", meter.date))
                                                .absolute()
                                                .left_0()
                                                .whitespace_nowrap()
                                                .w(px(18.))
                                                .text_size(px(8.))
                                                .line_height(px(12.))
                                                .child(bucket_tick(meter, scope))
                                                .test_support(),
                                        ),
                                    )
                                    .test_support()
                            }))
                            .test_support(),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .text_xs()
                            .child(format!(
                                "Best day: {} · Best streak: {} days",
                                recap
                                    .best_day
                                    .map_or_else(|| "—".into(), |(_, n)| compact(n)),
                                recap.best_streak
                            ))
                            .child(if self.state.pokemon_disabled {
                                format!("Active days: {}/{}", recap.active_days, recap.counted_days)
                            } else {
                                format!(
                                    "Active days: {}/{} · Graduates: {}",
                                    recap.active_days,
                                    recap.counted_days,
                                    recap.graduated.len()
                                )
                            }),
                    )
                    .when(!self.state.pokemon_disabled, |d| {
                        d.child(
                            div()
                                .id("usage-recap-graduates")
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .items_center()
                                .when_some(self.state.pokemon.active.as_ref(), |d, mon| {
                                    d.child(self.render_recap_mon(mon, 20.))
                                })
                                .when(graduates.is_empty(), |d| {
                                    d.child(div().text_xs().child("No graduates in this period"))
                                })
                                .children(graduates.into_iter().map(|mon| {
                                    let name = graduate_name(mon);
                                    h_flex()
                                        .id(format!("usage-recap-graduate-{}", mon_id(mon)))
                                        .gap_1()
                                        .items_center()
                                        .max_w(px(135.))
                                        .child(self.render_recap_mon(mon, 20.))
                                        .child(
                                            div().text_size(px(9.)).truncate().child(name.clone()),
                                        )
                                        .role(Role::Label)
                                        .aria_label(name)
                                        .test_support()
                                }))
                                .test_support(),
                        )
                    })
                    .test_support(),
            )
            .when_some(recap.best_day, |d, (date, n)| {
                d.child(div().text_xs().child(format!(
                    "Best day: {} · {} tokens",
                    DateText::new(Language::En, "EEEEdMMM").stamp(date),
                    grouped(n, ',')
                )))
            })
            .when(recap.in_progress && recap.delta().is_some(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Compared with the same elapsed days in the previous period"),
                )
            })
            .test_support()
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
