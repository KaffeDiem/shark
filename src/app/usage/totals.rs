use super::*;
use crate::usage::format::compact;
use crate::usage::summary::combined::Summary;
impl Shark {
    pub(super) fn render_usage_totals(&self, summary: &Summary, cx: &Context<Self>) -> AnyElement {
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
                    let text = format!("{title}: {} tokens", compact(bucket.total()));
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
                        .test_support()
                }),
            )
            .test_support()
            .into_any_element()
    }
}
#[cfg(test)]
mod tests;
