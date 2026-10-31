//! Only rows intersecting the viewport construct sprite widgets, even for a large catch history.
use super::*;
impl Shark {
    pub(super) fn pokemon_catch_log(&self, cx: &Context<Self>) -> AnyElement {
        let entries = dex::catches(
            &self.state.pokemon,
            &self.dex_query(cx),
            self.pokemon_ui.rarity,
            self.pokemon_ui.shiny_only,
            self.pokemon_ui.catch_sort.unwrap_or(dex::Sort::RecentFirst),
        );
        if entries.is_empty() {
            return div()
                .child("No catches match your filters.")
                .into_any_element();
        }
        div()
            .h(px(280.))
            .w_full()
            .overflow_hidden()
            .child(
                uniform_list(
                    "pokemon-catch-log",
                    entries.len(),
                    cx.processor(move |s, range: std::ops::Range<usize>, _, cx| {
                        s.pokemon_ui
                            .catch_layout_count
                            .set(s.pokemon_ui.catch_layout_count.get() + range.len());
                        s.pokemon_ui.catch_visible = range
                            .clone()
                            .filter_map(|i| entries.get(i))
                            .map(|(_, m)| mon_sprite_identity(m, m.species_id()))
                            .collect();
                        s.fill_dex_assets(cx);
                        range
                            .filter_map(|i| entries.get(i))
                            .map(|(index, mon)| s.pokemon_catch_row(*index, mon, cx))
                            .collect()
                    }),
                )
                .track_scroll(&self.pokemon_ui.catch_scroll)
                .size_full()
                .test_support(),
            )
            .into_any_element()
    }
    fn pokemon_catch_row(&self, index: usize, mon: &Mon, cx: &Context<Self>) -> AnyElement {
        let species = mon.species_id();
        let identity = mon.profile.as_ref().map(|p| p.instance_id.clone());
        let form = mon.form.clone();
        let shiny = mon.displays_shiny();
        let art = self
            .pokemon_ui
            .thumbnails
            .get(&mon_sprite_identity(mon, species))
            .and_then(|path| images::sized(path, 40.))
            .map(|img| {
                img.id(format!("pokemon-catch-sprite-{index}"))
                    .into_any_element()
            })
            .unwrap_or_else(|| div().child(format!("#{species}")).into_any_element());
        Button::new(format!("pokemon-catch-{index}"))
            .ghost()
            .h(px(56.))
            .w_full()
            .p_1()
            .accessibility_label(format!(
                "Open {} catch details{}",
                mon.name(),
                if shiny { ", shiny" } else { "" }
            ))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .size(px(40.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(art),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .flex_1()
                            .items_start()
                            .child(
                                div()
                                    .w_full()
                                    .text_ellipsis()
                                    .overflow_hidden()
                                    .child(mon.name()),
                            )
                            .child(div().text_xs().child(format!(
                                "{} · caught {}{}",
                                mon.line.rarity.label(),
                                mon.caught_at,
                                if shiny { " · Shiny" } else { "" }
                            ))),
                    ),
            )
            .on_click(cx.listener(move |s, _, _, cx| {
                s.select_pokemon(species, cx);
                s.select_pokemon_appearance(species, form.clone(), Some(shiny), cx);
                s.pokemon_ui.individual = s
                    .detail_individuals(species)
                    .iter()
                    .position(|m| m.profile.as_ref().map(|p| &p.instance_id) == identity.as_ref())
                    .unwrap_or(0);
                s.sync_profile_names(true, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}
#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::app::ui_tests::{UI_LOCK, boot, test_home};
    use core::prelude::v1::test;
    use gpui_kit::test::TestWindowExt;
    #[gpui_kit::test]
    #[allow(clippy::await_holding_lock)]
    async fn large_log_reopen_lays_out_only_visible_sprite_rows(cx: &mut TestAppContext) {
        let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_home();
        let mut state = State::default();
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(crate::pokemon::EvoLine {
            base_id: 25,
            tree: crate::pokemon::EvoNode {
                species_id: 25,
                children: vec![],
            },
            rarity: Rarity::Common,
            names: BTreeMap::new(),
        });
        let original = p.active.unwrap();
        for i in 0..300 {
            let mut mon = original.clone();
            mon.caught_at = i;
            mon.profile.as_mut().unwrap().instance_id = format!("log-{i}");
            state.pokemon.collection.push(mon);
        }
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            for _ in 0..3 {
                shark.update(cx, |s, _| s.pokemon_ui.catch_layout_count.set(0));
                w.render_frame(cx);
                w.click("pokemon-footer", cx);
                w.click("pokemon-tab-Pokédex", cx);
                w.click("pokemon-segment-log", cx);
                w.render_frame(cx);
                let count = shark.read(cx).pokemon_ui.catch_layout_count.get();
                assert!(count > 0 && count < 100, "visible row layouts {count}");
                assert!(w.try_find("pokemon-catch-299").is_none());
                w.press("escape", cx);
            }
        })
        .unwrap();
    }
}
