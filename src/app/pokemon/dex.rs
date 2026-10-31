//! Query and paging rules shared by the Pokédex grid and catch log.
use crate::pokemon::{Companion, Mon, Rarity};
use gpui_kit::TouchPhase;
use objc2_foundation::NSString;
use std::collections::BTreeMap;

pub(super) const PAGE_SIZE: usize = 16;
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Sort {
    #[default]
    NumberAsc,
    NumberDesc,
    NameAsc,
    NameDesc,
    RarityDesc,
    RecentFirst,
    OldestFirst,
}
impl Sort {
    pub fn next(self, log: bool) -> Self {
        match (log, self) {
            (true, Self::RecentFirst) => Self::OldestFirst,
            (true, Self::OldestFirst) => Self::NumberAsc,
            (_, Self::NumberAsc) => Self::NumberDesc,
            (false, Self::NumberDesc) => Self::NameAsc,
            (true, Self::NumberDesc) => Self::NameAsc,
            (_, Self::NameAsc) => Self::NameDesc,
            (_, Self::NameDesc) => Self::RarityDesc,
            (true, _) => Self::RecentFirst,
            _ => Self::NumberAsc,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::NumberAsc => "Number ↑",
            Self::NumberDesc => "Number ↓",
            Self::NameAsc => "Name A–Z",
            Self::NameDesc => "Name Z–A",
            Self::RarityDesc => "Rarity ↓",
            Self::RecentFirst => "Newest first",
            Self::OldestFirst => "Oldest first",
        }
    }
}

fn matches(mon: &Mon, species: u16, query: &str) -> bool {
    let query = query.trim();
    query.is_empty()
        || species.to_string().contains(query.trim_start_matches('#'))
        || mon.line.names.get(&species).is_some_and(|names| {
            names.values().any(|name| {
                // Foundation's user-facing search folds both case and accents, including Unicode.
                // https://developer.apple.com/documentation/foundation/nsstring/localizedstandardcontains(_:)
                NSString::from_str(name).localizedStandardContainsString(&NSString::from_str(query))
            })
        })
}
fn accepts(mon: &Mon, rarity: Option<Rarity>, shiny: bool) -> bool {
    rarity.is_none_or(|r| r == mon.line.rarity) && (!shiny || mon.displays_shiny())
}
fn sort_entries(entries: &mut [(u16, Mon)], sort: Sort) {
    entries.sort_by(|(a, am), (b, bm)| {
        let rank = |r| match r {
            Rarity::Common => 0,
            Rarity::Uncommon => 1,
            Rarity::Rare => 2,
            Rarity::Legendary => 3,
        };
        match sort {
            Sort::NumberDesc => b.cmp(a),
            Sort::NameAsc => am
                .line
                .name(*a)
                .to_lowercase()
                .cmp(&bm.line.name(*b).to_lowercase()),
            Sort::NameDesc => bm
                .line
                .name(*b)
                .to_lowercase()
                .cmp(&am.line.name(*a).to_lowercase()),
            Sort::RarityDesc => rank(bm.line.rarity)
                .cmp(&rank(am.line.rarity))
                .then(a.cmp(b)),
            _ => a.cmp(b),
        }
    });
}
pub(super) fn species(
    p: &Companion,
    query: &str,
    rarity: Option<Rarity>,
    shiny: bool,
    sort: Sort,
) -> Vec<(u16, Mon)> {
    let mut entries = BTreeMap::<u16, Mon>::new();
    for mon in p
        .collection
        .iter()
        .chain(p.active.iter())
        .filter(|m| accepts(m, rarity, shiny))
    {
        for id in mon.reached_species().filter(|&id| matches(mon, id, query)) {
            entries
                .entry(id)
                .and_modify(|old| {
                    if mon.displays_shiny() {
                        *old = mon.clone()
                    }
                })
                .or_insert_with(|| mon.clone());
        }
    }
    let mut entries: Vec<_> = entries.into_iter().collect();
    sort_entries(&mut entries, sort);
    entries
}
pub(super) fn catches(
    p: &Companion,
    query: &str,
    rarity: Option<Rarity>,
    shiny: bool,
    sort: Sort,
) -> Vec<(usize, Mon)> {
    let mut entries: Vec<_> = p
        .catch_log()
        .into_iter()
        .enumerate()
        .filter(|(_, m)| {
            accepts(m, rarity, shiny) && m.reached_species().any(|s| matches(m, s, query))
        })
        .map(|(i, m)| (i, m.clone()))
        .collect();
    entries.sort_by(|(ai, a), (bi, b)| {
        let order = match sort {
            Sort::RecentFirst => ai.cmp(bi),
            Sort::OldestFirst => a.caught_at.cmp(&b.caught_at),
            Sort::NumberDesc => b.species_id().cmp(&a.species_id()),
            Sort::NameAsc => a.name().to_lowercase().cmp(&b.name().to_lowercase()),
            Sort::NameDesc => b.name().to_lowercase().cmp(&a.name().to_lowercase()),
            Sort::RarityDesc => (b.line.rarity as u8).cmp(&(a.line.rarity as u8)),
            _ => a.species_id().cmp(&b.species_id()),
        };
        order.then(ai.cmp(bi))
    });
    entries
}
pub(super) fn containing(id: Option<u16>, ids: impl IntoIterator<Item = u16>) -> usize {
    id.and_then(|id| ids.into_iter().position(|value| value == id))
        .unwrap_or(0)
        / PAGE_SIZE
}
pub(super) fn page_after(current: usize, pages: usize, step: i32) -> usize {
    current
        .saturating_add_signed(step as isize)
        .min(pages.saturating_sub(1))
}
#[derive(Default)]
pub(super) struct ScrollPager {
    accumulated: f32,
    fired: bool,
    gesture: bool,
    ended: bool,
}
impl ScrollPager {
    pub const THRESHOLD: f32 = 24.;
    pub fn step(&mut self, dy: f32, precise: bool, phase: TouchPhase) -> i32 {
        let direction = |d: f32| {
            if d < 0. {
                1
            } else if d > 0. {
                -1
            } else {
                0
            }
        };
        if !dy.is_finite() {
            return 0;
        }
        if !precise {
            return direction(dy);
        }
        match phase {
            TouchPhase::Started => {
                self.accumulated = 0.;
                self.fired = false;
                self.gesture = true;
                self.ended = false;
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.accumulated = 0.;
                self.gesture = false;
                self.ended = true;
                return 0;
            }
            _ => {}
        }
        if self.fired || self.ended {
            return 0;
        }
        self.accumulated += dy;
        if self.accumulated.abs() < Self::THRESHOLD {
            return 0;
        }
        let step = direction(self.accumulated);
        self.accumulated = 0.;
        self.fired = self.gesture;
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pokemon::{Balance, EvoLine, EvoNode};
    use core::prelude::v1::test;
    fn mon(ids: &[u16], names: &[(&str, &str)], shiny: bool, rarity: Rarity, at: u64) -> Mon {
        let tree = ids
            .iter()
            .rev()
            .fold(None, |child, id| {
                Some(EvoNode {
                    species_id: *id,
                    children: child.into_iter().collect(),
                })
            })
            .unwrap();
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(EvoLine {
            base_id: 25,
            tree: EvoNode {
                species_id: 25,
                children: vec![],
            },
            rarity,
            names: BTreeMap::new(),
        });
        let mut m = p.active.unwrap();
        m.line = EvoLine {
            base_id: ids[0],
            tree,
            rarity,
            names: BTreeMap::new(),
        };
        m.path = ids.to_vec();
        m.stage = ids.len() - 1;
        m.shiny = shiny;
        m.caught_at = at;
        for (id, (en, fr)) in ids.iter().zip(names.iter()) {
            m.line.names.insert(
                *id,
                BTreeMap::from([("en".into(), (*en).into()), ("fr".into(), (*fr).into())]),
            );
        }
        m
    }
    fn fixture() -> Companion {
        let mut p = Companion::default();
        p.collection = vec![
            mon(
                &[1, 2, 3],
                &[
                    ("Bulbasaur", "Bulbizarre"),
                    ("Ivysaur", "Herbizarre"),
                    ("Venusaur", "Florizarre"),
                ],
                false,
                Rarity::Common,
                100,
            ),
            mon(
                &[25, 26],
                &[("Pikachu", "Pikachu"), ("Raichu", "Raichu")],
                true,
                Rarity::Rare,
                200,
            ),
            mon(
                &[150],
                &[("Mewtwo", "Mewtwo")],
                false,
                Rarity::Legendary,
                300,
            ),
        ];
        p.collection[2]
            .line
            .names
            .get_mut(&150)
            .unwrap()
            .insert("ko".into(), "뮤츠".into());
        p
    }
    fn ids(p: &Companion, q: &str, r: Option<Rarity>, s: bool, sort: Sort) -> Vec<u16> {
        species(p, q, r, s, sort)
            .iter()
            .map(|(id, _)| *id)
            .collect()
    }
    #[test]
    fn search_numbers_all_localizations_and_missing() {
        let p = fixture();
        for (q, want) in [
            ("25", vec![25]),
            ("#150", vec![150]),
            ("15", vec![150]),
            ("Venusaur", vec![3]),
            ("pika", vec![25]),
            ("Bulbizarre", vec![1]),
            ("뮤츠", vec![150]),
            ("MissingNo", vec![]),
        ] {
            assert_eq!(ids(&p, q, None, false, Sort::NumberAsc), want, "{q}");
        }
        assert_eq!(
            ids(&p, "", None, false, Sort::NumberAsc),
            [1, 2, 3, 25, 26, 150]
        );
    }
    #[test]
    fn accent_search_matches_both_surfaces() {
        let mut p = fixture();
        p.collection.push(mon(
            &[669],
            &[("Flabébé", "Flabébé")],
            false,
            Rarity::Common,
            400,
        ));
        for q in ["flabebe", "FLABEBE", "Flabébé", "flabébe"] {
            assert_eq!(ids(&p, q, None, false, Sort::NumberAsc), [669]);
            assert_eq!(catches(&p, q, None, false, Sort::RecentFirst).len(), 1);
        }
    }
    #[test]
    fn shiny_rarity_and_combined_filters() {
        let p = fixture();
        assert_eq!(ids(&p, "", None, true, Sort::NumberAsc), [25, 26]);
        assert_eq!(
            ids(&p, "", Some(Rarity::Legendary), false, Sort::NumberAsc),
            [150]
        );
        assert_eq!(
            ids(&p, "", Some(Rarity::Common), false, Sort::NumberAsc),
            [1, 2, 3]
        );
        assert_eq!(
            ids(&p, "", Some(Rarity::Rare), true, Sort::NumberAsc),
            [25, 26]
        );
        assert!(ids(&p, "", Some(Rarity::Legendary), true, Sort::NumberAsc).is_empty());
    }
    #[test]
    fn grid_all_five_sort_orders() {
        let p = fixture();
        assert_eq!(
            ids(&p, "", None, false, Sort::NumberDesc),
            [150, 26, 25, 3, 2, 1]
        );
        assert_eq!(
            ids(&p, "", None, false, Sort::NameAsc),
            [1, 2, 150, 25, 26, 3]
        );
        assert_eq!(
            ids(&p, "", None, false, Sort::NameDesc),
            [3, 26, 25, 150, 2, 1]
        );
        assert_eq!(
            ids(&p, "", None, false, Sort::RarityDesc),
            [150, 25, 26, 1, 2, 3]
        );
    }
    #[test]
    fn catch_search_includes_chain_and_filters() {
        let p = fixture();
        for q in ["25", "Raichu"] {
            assert_eq!(
                catches(&p, q, None, false, Sort::RecentFirst)[0]
                    .1
                    .line
                    .base_id,
                25
            );
        }
        assert_eq!(catches(&p, "", None, true, Sort::RecentFirst).len(), 1);
        assert_eq!(
            catches(&p, "", Some(Rarity::Legendary), false, Sort::RecentFirst)[0]
                .1
                .species_id(),
            150
        );
    }
    #[test]
    fn catch_all_five_sort_orders() {
        let p = fixture();
        let by = |sort| {
            catches(&p, "", None, false, sort)
                .iter()
                .map(|(_, m)| m.species_id())
                .collect::<Vec<_>>()
        };
        assert_eq!(by(Sort::RecentFirst), [150, 26, 3]);
        assert_eq!(by(Sort::OldestFirst), [3, 26, 150]);
        assert_eq!(by(Sort::NumberAsc), [3, 26, 150]);
        assert_eq!(by(Sort::NumberDesc), [150, 26, 3]);
        assert_eq!(by(Sort::RarityDesc), [150, 26, 3]);
    }
    #[test]
    fn links_include_only_owned_reached_species() {
        let mut p = fixture();
        let mut raising = mon(
            &[4, 5, 6],
            &[("Charmander", ""), ("Charmeleon", ""), ("Charizard", "")],
            false,
            Rarity::Common,
            0,
        );
        raising.stage = 1;
        p.active = Some(raising);
        p.collection
            .push(mon(&[201], &[("Unown", "")], false, Rarity::Rare, 0));
        let all = ids(&p, "", None, false, Sort::NumberAsc);
        assert!(all.contains(&4) && all.contains(&5) && all.contains(&201));
        assert!(!all.contains(&6) && !all.contains(&999));
        p.active = None;
        assert!(p.active.as_ref().map(|m| m.species_id()).is_none());
    }
    #[test]
    fn page_changes_clamp() {
        assert_eq!(page_after(1, 3, 1), 2);
        assert_eq!(page_after(1, 3, -1), 0);
        for (c, n, s, w) in [(2, 3, 1, 2), (0, 3, -1, 0), (0, 1, 1, 0), (0, 0, 1, 0)] {
            assert_eq!(page_after(c, n, s), w);
        }
    }
    #[test]
    fn return_page_boundaries_and_missing() {
        for (id, w) in [(0, 0), (15, 0), (16, 1), (20, 1), (32, 2), (39, 2)] {
            assert_eq!(containing(Some(id), 0..40), w);
        }
        assert_eq!(containing(None, 0..40), 0);
        assert_eq!(containing(Some(99), 0..40), 0);
        assert_eq!(containing(Some(1), []), 0);
    }
    #[test]
    fn wheel_each_notch() {
        let mut p = ScrollPager::default();
        for (d, w) in [(-1., 1), (-1., 1), (3., -1), (0., 0)] {
            assert_eq!(p.step(d, false, TouchPhase::Moved), w);
        }
    }
    #[test]
    fn trackpad_resets_small_gestures() {
        let mut p = ScrollPager::default();
        assert_eq!(p.step(-6., true, TouchPhase::Started), 0);
        assert_eq!(p.step(-6., true, TouchPhase::Moved), 0);
        assert_eq!(p.step(0., true, TouchPhase::Ended), 0);
        assert_eq!(p.step(-12., true, TouchPhase::Started), 0);
    }
    #[test]
    fn trackpad_latches_and_rejects_ended_tail() {
        let mut p = ScrollPager::default();
        assert_eq!(p.step(-13., true, TouchPhase::Started), 0);
        assert_eq!(p.step(-13., true, TouchPhase::Moved), 1);
        for _ in 0..5 {
            assert_eq!(p.step(-48., true, TouchPhase::Moved), 0);
        }
        assert_eq!(p.step(0., true, TouchPhase::Ended), 0);
        for _ in 0..5 {
            assert_eq!(p.step(-72., true, TouchPhase::Moved), 0);
        }
        assert_eq!(p.step(0., true, TouchPhase::Ended), 0);
        assert_eq!(p.step(24., true, TouchPhase::Started), -1);
    }
}
