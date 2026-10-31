use super::*;
use crate::pokemon::format::{Language, compact};
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
        v_flex()
            .id("usage-combined-totals")
            .gap_1()
            .text_xs()
            .w_full()
            .child(
                div()
                    .id("usage-today-total")
                    .role(Role::Label)
                    .aria_label(format!("Today: {} tokens", compact(summary.today.total())))
                    .child(format!("Today: {} tokens", compact(summary.today.total())))
                    .test_support(),
            )
            .children(
                [
                    ("week", "This week", &summary.week),
                    ("month", "This month", &summary.month),
                ]
                .into_iter()
                .map(|(id, title, bucket)| {
                    let text = format!(
                        "{title}: {}{}",
                        compact(bucket.total()),
                        if shows_cost {
                            format!(" · {}", bucket.cost_text(false))
                        } else {
                            String::new()
                        }
                    );
                    div()
                        .id(format!("usage-period-{id}"))
                        .min_w_0()
                        .w_full()
                        .whitespace_nowrap()
                        .truncate()
                        .role(Role::Label)
                        .aria_label(text.clone())
                        .aria_description(if shows_cost {
                            bucket.cost_explanation(Language::En)
                        } else {
                            ""
                        })
                        .child(text)
                        .test_support()
                }),
            )
            .text_color(cx.theme().muted_foreground)
            .test_support()
            .into_any_element()
    }
}
#[cfg(test)]
mod tests;
