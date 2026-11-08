//! One identity renderer for every agent surface.
use super::*;
use crate::state::{Agent, AgentIcon};

#[derive(IntoElement)]
pub(crate) struct AgentIconView(AgentIcon);

pub(crate) fn agent_icon(icon: &AgentIcon) -> AgentIconView {
    AgentIconView(icon.clone())
}

impl RenderOnce for AgentIconView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let content = match self.0 {
            AgentIcon::Symbol { name, tint } => Icon::new(super::icon_picker::symbol(&name))
                .with_size(px(16.))
                .when_some(
                    tint.as_deref()
                        .and_then(|t| super::icon_picker::tint(t, cx.theme())),
                    |i, color| i.text_color(color),
                )
                .into_any_element(),
            AgentIcon::Emoji { value } => div()
                .font_family("Apple Color Emoji")
                .text_size(px(14.))
                .line_height(px(16.))
                .child(value)
                .into_any_element(),
            AgentIcon::Image { file } => match crate::agent_icon::path(&file) {
                Some(path) => img(path)
                    .size(px(16.))
                    .object_fit(ObjectFit::Cover)
                    .rounded_full()
                    .border(px(0.5))
                    .border_color(white().opacity(0.1))
                    .with_fallback(|| {
                        Icon::new(IconName::Bot)
                            .with_size(px(16.))
                            .into_any_element()
                    })
                    .into_any_element(),
                None => Icon::new(IconName::Bot)
                    .with_size(px(16.))
                    .into_any_element(),
            },
        };
        div()
            .size(px(16.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(content)
    }
}

pub(crate) fn command(agent: &Agent) -> CommandItem {
    let icon = agent.icon.clone();
    let name = agent.name.clone();
    CommandItem::new().label(name.clone()).child(move |_, _| {
        h_flex()
            .gap(px(10.))
            .child(agent_icon(&icon))
            .child(name.clone())
    })
}
