//! A form chooses the appearance family; the individual picker then selects only that family.
use super::*;
pub(super) fn form_columns(count: usize) -> usize {
    if count > 20 {
        7
    } else if count > 8 {
        6
    } else {
        count.min(4)
    }
}
impl Shark {
    pub(super) fn displayed_pokemon_form(
        &self,
        species: u16,
    ) -> Option<crate::pokemon::forms::Form> {
        let forms = self.state.pokemon.form_species(species);
        let wanted = crate::pokemon::forms::resolved(species, self.pokemon_ui.detail_form.as_ref());
        if forms.iter().any(|entry| entry.form == wanted) {
            wanted
        } else {
            forms.first().and_then(|entry| entry.form.clone())
        }
    }
    pub(super) fn detail_individuals(&self, species: u16) -> Vec<&Mon> {
        self.state
            .pokemon
            .individuals_for_form(species, self.displayed_pokemon_form(species).as_ref())
    }
    pub(super) fn selected_pokemon_individual(&self, species: u16) -> Option<&Mon> {
        let individuals = self.detail_individuals(species);
        individuals
            .get(
                self.pokemon_ui
                    .individual
                    .min(individuals.len().saturating_sub(1)),
            )
            .copied()
            .or_else(|| {
                self.state
                    .pokemon
                    .appearance_individuals(species, self.displayed_pokemon_form(species).as_ref())
                    .first()
                    .copied()
            })
    }
    pub(super) fn select_pokemon_appearance(
        &mut self,
        species: u16,
        form: Option<crate::pokemon::forms::Form>,
        shiny: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        self.pokemon_ui
            .evolution_detail_scroll
            .set_offset(point(px(0.), px(0.)));
        self.pokemon_ui.detail_form = form;
        let individuals = self.detail_individuals(species);
        self.pokemon_ui.individual = individuals
            .iter()
            .position(|m| m.displays_shiny() == shiny.unwrap_or(false))
            .unwrap_or(0);
        if let Some(mon) = self.selected_pokemon_individual(species) {
            self.load_pokemon_thumbnail(mon_sprite_identity(mon, species), cx);
        }
        self.sync_profile_names(true, cx);
        cx.notify();
    }
    pub(super) fn render_pokemon_appearance_picker(
        &self,
        species: u16,
        mon: &Mon,
        cx: &Context<Self>,
    ) -> AnyElement {
        let all_forms = crate::pokemon::forms::forms(species);
        let selected = self.displayed_pokemon_form(species);
        let owned = self.state.pokemon.form_species(species);
        let columns = form_columns(all_forms.len()).max(1);
        let width = (300. - (columns as f32 - 1.) * 4.) / columns as f32;
        let individuals = self.detail_individuals(species);
        let normal = individuals.iter().any(|m| !m.displays_shiny());
        let shiny = individuals.iter().any(|m| m.displays_shiny());
        v_flex()
            .gap_2()
            .when(!all_forms.is_empty(), |d| {
                d.child(
                    div()
                        .id("pokemon-form-picker")
                        .flex()
                        .flex_wrap()
                        .gap(px(4.))
                        .test_support()
                        .children(all_forms.into_iter().map(|form| {
                            let owned_form = owned
                                .iter()
                                .find(|entry| entry.form.as_ref() == Some(&form));
                            let label = self
                                .selected_form_label(species, Some(&form))
                                .unwrap_or_else(|| form.0.clone());
                            let caption = if owned_form.is_some_and(|entry| entry.raising) {
                                format!(
                                    "{label} · {}",
                                    crate::pokemon::format::Language::En.dex_raising()
                                )
                            } else {
                                label.clone()
                            };
                            let key = (
                                species,
                                owned_form
                                    .is_some_and(|entry| !entry.normal_owned && entry.shiny_owned),
                                Some(form.clone()),
                            );
                            let art = self
                                .pokemon_ui
                                .thumbnails
                                .get(&key)
                                .map(|path| images::sized(path, 28.).into_any_element())
                                .unwrap_or_else(|| {
                                    div()
                                        .size(px(28.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(if species == 201 {
                                            form.symbol()
                                        } else {
                                            "✦".into()
                                        })
                                        .into_any_element()
                                });
                            Button::new(format!("pokemon-form-{}", form.0))
                                .ghost()
                                .h_auto()
                                .w(px(width))
                                .p_1()
                                .disabled(owned_form.is_none())
                                .selected(selected.as_ref() == Some(&form))
                                .accessibility_label(format!(
                                    "{caption} form{}",
                                    if owned_form.is_none() {
                                        ", not collected"
                                    } else {
                                        ""
                                    }
                                ))
                                .child(
                                    v_flex().w_full().items_center().child(art).child(
                                        div()
                                            .text_xs()
                                            .w_full()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(caption),
                                    ),
                                )
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.select_pokemon_appearance(
                                        species,
                                        Some(form.clone()),
                                        None,
                                        cx,
                                    )
                                }))
                        })),
                )
            })
            .when(normal && shiny, |d| {
                d.child(
                    h_flex().gap_1().children(
                        [(false, "Normal"), (true, "Shiny")].into_iter().map(
                            |(is_shiny, label)| {
                                let form = selected.clone();
                                Button::new(if is_shiny {
                                    "pokemon-color-shiny"
                                } else {
                                    "pokemon-color-normal"
                                })
                                .ghost()
                                .xsmall()
                                .label(label)
                                .selected(mon.displays_shiny() == is_shiny)
                                .on_click(cx.listener(
                                    move |s, _, _, cx| {
                                        s.select_pokemon_appearance(
                                            species,
                                            form.clone(),
                                            Some(is_shiny),
                                            cx,
                                        )
                                    },
                                ))
                            },
                        ),
                    ),
                )
            })
            .when(individuals.len() > 1, |d| {
                d.child(
                    h_flex()
                        .id("pokemon-individual-picker")
                        .gap_1()
                        .flex_wrap()
                        .children(individuals.iter().enumerate().map(|(index, individual)| {
                            let label = format!(
                                "Individual {}{}",
                                index + 1,
                                if individual.displays_shiny() {
                                    ", shiny"
                                } else {
                                    ""
                                }
                            );
                            Button::new(format!("pokemon-individual-{index}"))
                                .ghost()
                                .xsmall()
                                .label((index + 1).to_string())
                                .accessibility_label(label)
                                .selected(index == self.pokemon_ui.individual)
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.pokemon_ui
                                        .evolution_detail_scroll
                                        .set_offset(point(px(0.), px(0.)));
                                    s.pokemon_ui.individual = index;
                                    if let Some(mon) = s.selected_pokemon_individual(species) {
                                        s.load_pokemon_thumbnail(
                                            mon_sprite_identity(mon, species),
                                            cx,
                                        );
                                    }
                                    s.sync_profile_names(true, cx);
                                    cx.notify();
                                }))
                        }))
                        .test_support(),
                )
            })
            .into_any_element()
    }
    pub(super) fn form_metadata_note(
        &self,
        species: u16,
        form: Option<&crate::pokemon::forms::Form>,
    ) -> Option<&'static str> {
        (self.pokemon_ui.details.contains_key(&species)
            && !crate::pokemon::forms::is_default(species, form)
            && crate::pokemon::forms::resolved(species, form).is_some())
        .then_some("Data uses the default form")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    // upstream: PokemonFormTests.swift::testFormPickerKeepsSevenColumnsForUnownAndWidensNamedForms
    #[test]
    fn form_grid_geometry_matches_label_density() {
        assert_eq!(form_columns(28), 7);
        assert_eq!(form_columns(18), 6);
        assert_eq!(form_columns(6), 4);
        assert_eq!(form_columns(2), 2);
    }
}
