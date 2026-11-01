//! Versioned name backfill only updates descriptive metadata, never an individual's progress.
use super::{Companion, EvoLine, Mon, evolution::normalized_names};
pub const VERSION: u32 = 1;
impl Mon {
    pub fn needs_names_refresh(&self) -> bool {
        self.names_version != Some(VERSION)
            || self.reached_species().any(|id| {
                self.line
                    .names
                    .get(&id)
                    .is_none_or(|names| normalized_names(names).is_empty())
            })
    }
}
impl Companion {
    pub fn merge_line_names(&mut self, line: &EvoLine) {
        for mon in self
            .collection
            .iter_mut()
            .chain(self.active.iter_mut())
            .filter(|mon| mon.line.base_id == line.base_id)
        {
            // Completeness describes this response, not a mixture of old and new rows.
            let complete = mon.reached_species().all(|id| {
                line.names
                    .get(&id)
                    .is_some_and(|names| !normalized_names(names).is_empty())
            });
            for (&id, names) in &line.names {
                if mon.line.tree.node(id).is_some() {
                    mon.line
                        .names
                        .entry(id)
                        .or_default()
                        .extend(normalized_names(names));
                }
            }
            mon.names_version = complete.then_some(VERSION);
        }
    }
}
