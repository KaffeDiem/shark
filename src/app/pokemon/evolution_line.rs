//! Bounded evolution rows: hidden branches stay mysterious and only owned species link to the dex.
use super::*;
use crate::pokemon::evolution::{LineContent, LineItem, LineState};
pub(super) fn row_width(count: usize, thumb: f32, names: bool) -> f32 {
    if count == 0 {
        return 0.;
    }
    count as f32 * (thumb + if names { 6. } else { 0. }) + (count - 1) as f32 * (thumb * 0.25 + 4.)
}
fn needs_scroll(count: usize, thumb: f32, names: bool, max: f32) -> bool {
    max.is_finite() && max > 0. && row_width(count, thumb, names) > max
}
fn affordance(offset: f32, content: f32, view: f32) -> (bool, bool) {
    let end = (content - view).max(0.);
    (end > 0. && offset > 0.5, end > 0. && offset < end - 0.5)
}
fn page_target(
    forward: bool,
    offset: f32,
    count: usize,
    thumb: f32,
    names: bool,
    view: f32,
) -> usize {
    if count == 0 {
        return 0;
    }
    let stride = thumb + if names { 6. } else { 0. } + thumb * 0.25 + 4.;
    let first = (offset.max(0.) / stride).round() as usize;
    let visible = (view / stride).floor().max(1.) as usize;
    if forward {
        first.saturating_add(visible).min(count - 1)
    } else {
        first.saturating_sub(visible)
    }
}
fn link_target(node: &LineItem, owned: &BTreeSet<u16>) -> Option<u16> {
    match node.content {
        LineContent::Species(id) if owned.contains(&id) => Some(id),
        _ => None,
    }
}
impl Shark {
    pub(super) fn render_pokemon_evolution_line(
        &self,
        mon: &Mon,
        home: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let active = self
            .state
            .pokemon
            .active
            .as_ref()
            .and_then(|m| m.profile.as_ref())
            .zip(mon.profile.as_ref())
            .is_some_and(|(a, b)| a.instance_id == b.instance_id);
        let nodes = if active {
            self.state.pokemon.line_nodes()
        } else {
            crate::pokemon::evolution::realized_line_items(
                &mon.reached_species().collect::<Vec<_>>(),
                mon.stage,
            )
        };
        self.render_evolution_nodes(
            mon,
            &nodes,
            if home { 40. } else { 56. },
            !home,
            312.,
            home,
            cx,
        )
    }
    fn render_evolution_nodes(
        &self,
        mon: &Mon,
        nodes: &[LineItem],
        thumb: f32,
        names: bool,
        max_width: f32,
        home: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let prefix = if home {
            "pokemon-home-evolution"
        } else {
            "pokemon-detail-evolution"
        };
        let width = row_width(nodes.len(), thumb, names);
        let scrolling = needs_scroll(nodes.len(), thumb, names, max_width);
        let view = if scrolling { max_width } else { width };
        let column = thumb + if names { 6. } else { 0. };
        let scroll = if home {
            self.pokemon_ui.evolution_home_scroll.clone()
        } else {
            self.pokemon_ui.evolution_detail_scroll.clone()
        };
        let owned = self
            .state
            .pokemon
            .collection
            .iter()
            .chain(self.state.pokemon.active.iter())
            .flat_map(Mon::reached_species)
            .collect::<BTreeSet<_>>();
        let mut row = h_flex()
            .id(format!("{prefix}-row"))
            .w(px(width))
            .min_w(px(width))
            .flex_none()
            .gap(px(2.))
            .items_start();
        for (index, node) in nodes.iter().enumerate() {
            if index > 0 {
                row = row.child(
                    div()
                        .w(px(thumb * 0.25))
                        .flex_none()
                        .h(px(thumb))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child("→"),
                );
            }
            let label = match node.content {
                LineContent::Species(id) => mon.line.name(id),
                LineContent::Mystery => crate::pokemon::format::Language::En
                    .unknown_next_evolution()
                    .into(),
            };
            let art = match node.content {
                LineContent::Species(id) => self
                    .pokemon_ui
                    .thumbnails
                    .get(&mon_sprite_identity(mon, id))
                    .map(|path| {
                        super::images::sized(path, thumb)
                            .id(format!("{prefix}-sprite-{index}"))
                            .into_any_element()
                    })
                    .unwrap_or_else(|| {
                        div()
                            .size(px(thumb))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .child(format!("#{id}"))
                            .into_any_element()
                    }),
                LineContent::Mystery => div()
                    .id(format!("{prefix}-mystery-{index}"))
                    .role(Role::Image)
                    .aria_label(crate::pokemon::format::Language::En.unknown_next_evolution())
                    .size(px(thumb))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_lg()
                    .child("?")
                    .into_any_element(),
            };
            let cell = v_flex()
                .w(px(column))
                .min_w(px(column))
                .flex_none()
                .items_center()
                .gap_1()
                .child(art)
                .when(names, |d| {
                    d.child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_xs()
                            .child(label.clone()),
                    )
                });
            let target = link_target(node, &owned);
            row = row.child(if let Some(id) = target {
                Button::new(format!("{prefix}-link-{index}"))
                    .ghost()
                    .p_0()
                    .h_auto()
                    .w(px(column))
                    .min_w(px(column))
                    .flex_none()
                    .accessibility_label(format!("Open {label} in Pokédex"))
                    .on_click(cx.listener(move |s, _, _, cx| s.select_pokemon(id, cx)))
                    .child(cell)
                    .into_any_element()
            } else {
                div()
                    .id(format!("{prefix}-node-{index}"))
                    .w(px(column))
                    .min_w(px(column))
                    .flex_none()
                    .when(node.state == LineState::Future, |d| d.opacity(0.6))
                    .child(cell)
                    .test_support()
                    .into_any_element()
            });
        }
        let content = div()
            .id(prefix)
            .w(px(view))
            .min_w_0()
            .max_w_full()
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            .child(row.test_support())
            .test_support();
        let (back, forward) = affordance(-f32::from(scroll.offset().x), width, view);
        let mut wrapper = v_flex()
            .id(format!("{prefix}-container"))
            .w(px(view))
            .min_w_0()
            .max_w_full()
            .gap_1()
            .child(content);
        if scrolling {
            let count = nodes.len();
            wrapper = wrapper.child(
                h_flex().justify_between().children(
                    [(false, back), (true, forward)]
                        .into_iter()
                        .map(|(next, visible)| {
                            let scroll = scroll.clone();
                            Button::new(format!(
                                "{prefix}-{}",
                                if next { "next" } else { "previous" }
                            ))
                            .ghost()
                            .xsmall()
                            .icon(if next {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronLeft
                            })
                            .disabled(!visible)
                            .accessibility_label(if next {
                                "Show later evolutions"
                            } else {
                                "Show earlier evolutions"
                            })
                            .on_click(cx.listener(
                                move |_, _, window, cx| {
                                    let target = page_target(
                                        next,
                                        -f32::from(scroll.offset().x),
                                        count,
                                        thumb,
                                        names,
                                        view,
                                    );
                                    let stride =
                                        thumb + if names { 6. } else { 0. } + thumb * 0.25 + 4.;
                                    scroll.set_offset(point(
                                        px(-(target as f32 * stride).min((width - view).max(0.))),
                                        px(0.),
                                    ));
                                    window.refresh();
                                    cx.notify();
                                },
                            ))
                        }),
                ),
            );
        }
        wrapper.test_support().into_any_element()
    }
}
#[cfg(all(test, feature = "test-support"))]
mod tests;
