//! Collection projections never count an unreached planned evolution as owned.
use super::{
    Companion, Mon, Rarity,
    forms::{self, Form},
};
use std::collections::BTreeSet;
#[derive(Clone, Debug, PartialEq)]
pub struct DexSpecies {
    pub id: u16,
    pub name: String,
    pub rarity: Rarity,
    pub shiny: bool,
    pub raising: bool,
    pub normal_owned: bool,
    pub shiny_owned: bool,
    pub count: usize,
    pub form: Option<Form>,
}
impl Companion {
    pub fn owns_species(&self, id: u16, form: Option<&Form>) -> bool {
        self.collection.iter().chain(self.active.iter()).any(|m| {
            m.reached_species().any(|s| s == id)
                && form.is_none_or(|f| forms::resolved(id, m.form.as_ref()).as_ref() == Some(f))
        })
    }
    pub fn collected_line_forms(&self, base: u16) -> BTreeSet<Form> {
        forms::line_forms(base)
            .into_iter()
            .filter(|f| {
                forms::form_species(base).iter().all(|&id| {
                    forms::resolved(id, Some(f)).as_ref() != Some(f)
                        || self.owns_species(id, Some(f))
                })
            })
            .collect()
    }
    pub fn appearance_individuals(&self, id: u16, form: Option<&Form>) -> Vec<&Mon> {
        self.active
            .iter()
            .chain(self.collection.iter().rev())
            .filter(|m| {
                m.reached_species().any(|s| s == id)
                    && form.is_none_or(|f| forms::resolved(id, m.form.as_ref()).as_ref() == Some(f))
            })
            .collect()
    }
    pub fn form_species(&self, id: u16) -> Vec<DexSpecies> {
        forms::forms(id)
            .into_iter()
            .filter_map(|form| {
                let owned = self.appearance_individuals(id, Some(&form));
                if owned.is_empty() {
                    return None;
                }
                let shiny = owned.iter().any(|m| m.displays_shiny());
                Some(DexSpecies {
                    id,
                    name: owned[0].line.name(id),
                    rarity: owned[0].line.rarity,
                    shiny,
                    raising: self.active.as_ref().is_some_and(|m| {
                        m.species_id() == id
                            && forms::resolved(id, m.form.as_ref()).as_ref() == Some(&form)
                    }),
                    normal_owned: owned.iter().any(|m| !m.displays_shiny()),
                    shiny_owned: shiny,
                    count: owned.len(),
                    form: Some(form),
                })
            })
            .collect()
    }
}

impl Companion {
    /// The current individual stays first; archived catches follow their recorded time.
    pub fn catch_log(&self) -> Vec<&Mon> {
        let mut archived: Vec<_> = self.collection.iter().rev().collect();
        archived.sort_by(|a, b| b.caught_at.cmp(&a.caught_at));
        self.active.iter().chain(archived).collect()
    }
}
