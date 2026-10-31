use super::*;
use gpui_kit::component::Selectable as _;
impl Shark {
    pub(super) fn selected_usage_provider(&self) -> Option<&str> {
        self.usage_panel
            .as_ref()
            .and_then(|panel| panel.selected_provider.as_deref())
            .filter(|id| self.usage_providers.iter().any(|p| p.id() == *id))
    }
    pub(super) fn render_usage_provider_tabs(&self, cx: &Context<Self>) -> AnyElement {
        let selected = self.selected_usage_provider();
        v_flex()
            .gap_1()
            .min_w_0()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Account details"),
            )
            .child(
                h_flex()
                    .id("usage-provider-tabs")
                    .role(Role::Group)
                    .aria_label("Filter account details")
                    .w_full()
                    .min_w_0()
                    .h(px(28.))
                    .overflow_x_scroll()
                    .gap_1()
                    .child(
                        Button::new("usage-provider-all")
                            .ghost()
                            .small()
                            .flex_none()
                            .label("All")
                            .selected(selected.is_none())
                            .on_click(cx.listener(|s, _, _, cx| {
                                if let Some(panel) = s.usage_panel.as_mut() {
                                    panel.selected_provider = None;
                                }
                                cx.notify();
                            })),
                    )
                    .children(self.usage_providers.iter().copied().map(|provider| {
                        Button::new(format!("usage-provider-tab-{}", provider.id()))
                            .ghost()
                            .small()
                            .flex_none()
                            .max_w(px(280.))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .label(provider.name())
                            .accessibility_label(format!(
                                "Show {} account details",
                                provider.name()
                            ))
                            .selected(selected == Some(provider.id()))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if let Some(panel) = s.usage_panel.as_mut() {
                                    panel.selected_provider = Some(provider.id().to_owned());
                                }
                                cx.notify();
                            }))
                    }))
                    .test_support(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
