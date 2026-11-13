//! The Azure DevOps work items (PBIs and their tasks) a branch is for, under its PR.
use super::*;

impl GitFeature {
    /// The branch's PBIs, each with its tasks: state, who has it, and a link.
    pub(super) fn work_items_view(&self, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let items = match &self.work_items {
            Ok(items) if items.is_empty() => return None,
            Ok(items) => items,
            Err(e) => {
                return Some(
                    div()
                        .id("git-work-items-error")
                        .test_support()
                        .text_sm()
                        .text_color(muted)
                        .child(format!("Work items unavailable: {e}"))
                        .into_any_element(),
                );
            }
        };
        let line = |w: &crate::azure::WorkItem, top: bool| {
            let url = w.url.clone();
            let who = match w.assigned.is_empty() {
                true => "Unassigned".to_string(),
                false => w.assigned.clone(),
            };
            h_flex()
                .gap_2()
                .min_w_0()
                .text_size(px(if top { 14. } else { 13. }))
                .child(
                    Icon::new(match () {
                        _ if top => IconName::ClipboardList,
                        _ if w.done() => IconName::CircleCheck,
                        _ => IconName::Circle,
                    })
                    .with_size(px(14.))
                    .flex_none()
                    .text_color(if !top && w.done() {
                        theme.success_foreground
                    } else {
                        muted
                    }),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(format!("{} {}", w.kind, w.id)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(top, |d| d.font_weight(FontWeight::MEDIUM))
                        .child(w.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_xs()
                        .text_color(if w.done() {
                            theme.success_foreground
                        } else {
                            theme.foreground
                        })
                        .child(w.state.clone()),
                )
                .child(div().flex_none().text_xs().text_color(muted).child(who))
                .child(
                    Button::new(SharedString::from(format!("git-work-item-{}", w.id)))
                        .icon(IconName::ExternalLink)
                        .tooltip("Open in Azure DevOps")
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
        };
        Some(
            v_flex()
                .id("git-work-items")
                .test_support()
                .w_full()
                .gap_3()
                .text_left()
                .child(div().text_xs().text_color(muted).child("Work items"))
                .children(items.iter().map(|w| {
                    let done = w.children.iter().filter(|c| c.done()).count();
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.secondary)
                        .child(line(w, true))
                        .when(!w.children.is_empty(), |d| {
                            d.child(
                                div()
                                    .pl_6()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{done} of {} done", w.children.len())),
                            )
                        })
                        .children(
                            w.children
                                .iter()
                                .map(|c| div().pl_6().child(line(c, false))),
                        )
                }))
                .into_any_element(),
        )
    }
}
