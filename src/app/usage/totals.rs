use super::*;
use crate::pokemon::format::compact;
use crate::usage::summary::combined::Summary;
impl Shark {
    pub(super) fn shows_usage_cost(&self, summary: &Summary) -> bool {
        summary.providers.iter().any(|active| {
            self.usage_providers
                .iter()
                .any(|provider| provider.id() == active.id && provider.source().reports_cost())
        })
    }
    pub(super) fn render_usage_totals(&self, summary: &Summary, cx: &Context<Self>) -> AnyElement {
        let shows_cost = self.shows_usage_cost(summary);
        h_flex()
            .id("usage-combined-totals")
            .gap_2()
            .w_full()
            .children(
                [
                    ("usage-today-total", "Today", "Today", &summary.today),
                    ("usage-period-week", "Week", "This week", &summary.week),
                    ("usage-period-month", "Month", "This month", &summary.month),
                ]
                .into_iter()
                .map(|(id, label, title, bucket)| {
                    let text = format!(
                        "{title}: {} tokens{}",
                        compact(bucket.total()),
                        if shows_cost {
                            format!(" · {}", bucket.cost_text(false))
                        } else {
                            String::new()
                        }
                    );
                    v_flex()
                        .id(id)
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .p_2()
                        .rounded_lg()
                        .bg(cx.theme().muted)
                        .role(Role::Label)
                        .aria_label(text)
                        .aria_description(if shows_cost {
                            bucket.cost_explanation()
                        } else {
                            ""
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(compact(bucket.total())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("tokens"),
                        )
                        .when(shows_cost, |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(bucket.cost_text(false)),
                            )
                        })
                        .test_support()
                }),
            )
            .test_support()
            .into_any_element()
    }
}
#[cfg(test)]
mod tests;
