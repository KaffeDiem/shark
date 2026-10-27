//! Workspace feature launchers. Each feature opens its content in an artifact tab.
use super::*;

impl Shark {
    pub(super) fn right_rail_shown(&self) -> bool {
        self.active.is_some() && !self.home_shown()
    }

    pub(super) fn render_features(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (n, viewed) = (self.diff_files.len(), self.viewed_now.len());
        let tip: SharedString = match n {
            _ if self.diff_error.is_some() => {
                "Differ — Show diff (cmd-shift-d) · could not load".into()
            }
            _ if !self.diff_loaded => "Differ — Show diff (cmd-shift-d)".into(),
            0 => "Differ — Show diff (cmd-shift-d) · no changes".into(),
            n => format!("Differ — Show diff (cmd-shift-d) · {viewed}/{n} viewed").into(),
        };
        let badge = match () {
            _ if self.diff_error.is_some() => Some(theme.warning_foreground),
            _ if self.diff_loaded && viewed < n => Some(theme.primary),
            _ => None,
        };
        v_flex()
            .id("features-rail")
            .aria_label("Features")
            .key_context("Features")
            .track_focus(&self.features_focus)
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.show(Tab::Diff, w, cx)))
            .w(px(72.))
            .h_full()
            .flex_none()
            .items_center()
            .child(
                div()
                    .id("features-header")
                    .h(px(44.))
                    .w_full()
                    .flex_none()
                    .map(|d| self.draggable_chrome("features-header", d)),
            )
            .child(
                v_flex().pt(px(if self.rail { 8. } else { 0. })).child(
                    sidebar::rail_button(
                        div().id("feature-differ").relative().flex().items_center(),
                    )
                    .role(Role::Button)
                    .aria_label("Differ — Show diff (cmd-shift-d)")
                    .text_color(theme.muted_foreground)
                    .map(|d| {
                        cursor_row(
                            d,
                            self.tab == Tab::Diff,
                            true,
                            self.features_focus.is_focused(window),
                            theme,
                        )
                    })
                    .child(Icon::new(IconName::FileDiff).with_size(px(16.)))
                    .when_some(badge, |d, c| d.child(chats::dot(6., theme).bg(c)))
                    .tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx))
                    .on_click(cx.listener(|this, _, w, cx| this.show(Tab::Diff, w, cx)))
                    .test_support(),
                ),
            )
            .test_support()
    }
}
