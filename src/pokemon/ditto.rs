//! Ditto's rare disguise appears ordinary until the first evolution threshold.
use super::{Companion, Event, EvoLine, Rarity};
pub const DENOMINATOR: u64 = 128;
pub fn disguise_hit(rarity: Rarity, forms: usize, roll: u64) -> bool {
    rarity == Rarity::Common && forms >= 2 && roll % DENOMINATOR == 0
}
impl Companion {
    pub fn ready_to_reveal(&self) -> bool {
        self.active.as_ref().is_some_and(|m| {
            m.ditto_disguise.is_some()
                && !m.ditto_revealed
                && m.tokens >= m.threshold(self.growth_difficulty)
        })
    }
    pub fn reveal_ditto(&mut self, line: EvoLine) -> Vec<Event> {
        if !self.ready_to_reveal()
            || line.base_id != 132
            || line.tree.species_id != 132
            || !line.tree.children.is_empty()
        {
            return vec![];
        }
        let mon = self.active.as_mut().unwrap();
        let previous_rarity = mon.line.rarity;
        let carry = mon
            .tokens
            .saturating_sub(mon.threshold(self.growth_difficulty));
        mon.line = line;
        mon.path = vec![132];
        mon.stage = 0;
        mon.tokens = carry;
        mon.ditto_revealed = true;
        mon.form = None;
        mon.growth_boost = self
            .collection
            .iter()
            .any(|m| m.graduated && m.line.base_id == 132);
        if let Some(profile) = &mut mon.profile {
            profile.rebase_species(previous_rarity, mon.line.rarity);
        }
        if let Some(id) = self.representative {
            if !self
                .collection
                .iter()
                .chain(self.active.iter())
                .any(|m| m.reached_species().any(|s| s == id))
            {
                self.representative = None;
                self.representative_form = None;
                self.representative_shiny = None;
            }
        }
        let mut events = vec![Event::Evolved(self.title())];
        events.extend(self.add_growth(0));
        events
    }
}
