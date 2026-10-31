//! Transient companion presentation; never part of the transferable save.
use crate::pokemon::{Companion, Event};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mood {
    Egg,
    LevelUp,
    Idle,
    Working,
    Focus,
    Tired,
    Sleep,
}
impl Mood {
    pub fn label(self) -> &'static str {
        match self {
            Self::Egg => "Waiting to hatch",
            Self::LevelUp => "Growing!",
            Self::Idle => "Taking a breather",
            Self::Working => "Coding companion",
            Self::Focus => "Focused coding",
            Self::Tired => "Time for a break",
            Self::Sleep => "Resting today",
        }
    }
}
#[derive(Default)]
pub(super) struct Celebration {
    pub until: f64,
    pub message: Option<String>,
    pub shiny: bool,
    pub notice: Option<String>,
}
impl Celebration {
    pub fn observe(&mut self, events: &[Event], companion: &Companion, now: f64) {
        if events.is_empty() {
            return;
        }
        if companion.active.is_none() {
            *self = Self::default();
            return;
        }
        let lang = crate::pokemon::format::Language::En;
        let hatched = events.iter().find_map(|e| {
            if let Event::Hatched(n) = e {
                Some(n)
            } else {
                None
            }
        });
        let evolved = events.iter().rev().find_map(|e| {
            if let Event::Evolved(n) = e {
                Some(n)
            } else {
                None
            }
        });
        if let Some(name) = hatched {
            self.message = Some(lang.hatch(name));
            self.shiny = companion
                .active
                .as_ref()
                .is_some_and(|m| m.displays_shiny());
        } else if let Some(name) = evolved {
            self.message = Some(lang.evolved(name));
            self.shiny = false;
        } else {
            return;
        }
        self.until = now + 4.;
    }
    pub fn expire(&mut self, now: f64) {
        if now >= self.until {
            *self = Self::default();
        }
    }
}
pub(super) fn mood(
    active: bool,
    celebration: &Celebration,
    now: f64,
    has_usage: bool,
    today: u64,
    burn_per_minute: f64,
    limit_warning: bool,
) -> Mood {
    if !active {
        return Mood::Egg;
    }
    if celebration.message.is_some() && now < celebration.until {
        return Mood::LevelUp;
    }
    if limit_warning {
        return Mood::Tired;
    }
    if !has_usage || today == 0 {
        return Mood::Sleep;
    }
    if burn_per_minute >= 100_000. {
        Mood::Focus
    } else if burn_per_minute > 1_000. {
        Mood::Working
    } else {
        Mood::Idle
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pokemon::{Balance, EvoLine, EvoNode, Rarity};
    use core::prelude::v1::test;
    use std::collections::BTreeMap;
    fn hatched() -> Companion {
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(EvoLine {
            base_id: 1,
            tree: EvoNode {
                species_id: 1,
                children: vec![],
            },
            rarity: Rarity::Common,
            names: BTreeMap::new(),
        });
        p
    }
    // upstream: CompanionDisplayStateTests.swift::testEggWhenNoUsageData
    #[test]
    fn egg_without_usage() {
        assert_eq!(
            mood(false, &Celebration::default(), 0., false, 0, 0., false),
            Mood::Egg
        );
    }
    // upstream: CompanionDisplayStateTests.swift::testLevelUpDuringEventWindow
    // upstream: CompanionDisplayStateTests.swift::testWorkingAfterEventExpires
    #[test]
    fn event_then_working() {
        let mut c = Celebration::default();
        c.observe(&[Event::Hatched("Bulbasaur".into())], &hatched(), 100.);
        assert_eq!(
            mood(true, &c, 101., true, 100, 10_000., false),
            Mood::LevelUp
        );
        assert_eq!(
            mood(true, &c, 160., true, 100, 10_000., false),
            Mood::Working
        );
    }
    // upstream: CompanionDisplayStateTests.swift::testFocusOnHighBurn
    // upstream: CompanionDisplayStateTests.swift::testTiredWhenLimitWarning
    // upstream: CompanionDisplayStateTests.swift::testSleepWhenZeroUsageToday
    #[test]
    fn focus_tired_sleep_priorities() {
        let c = Celebration::default();
        assert_eq!(mood(true, &c, 0., true, 100, 400_000., false), Mood::Focus);
        assert_eq!(mood(true, &c, 0., true, 100, 10_000., true), Mood::Tired);
        assert_eq!(mood(true, &c, 0., true, 0, 0., false), Mood::Sleep);
        assert_eq!(mood(true, &c, 0., false, 100, 10_000., false), Mood::Sleep);
    }
    // upstream: CompanionDisplayStateTests.swift::testEvolveStatusSurvivesUpdateWithinEventWindow
    #[test]
    fn empty_update_preserves_evolution_until_expiry() {
        let mut c = Celebration::default();
        let p = hatched();
        c.observe(&[Event::Evolved("Ivysaur".into())], &p, 100.);
        let message = c.message.clone();
        c.observe(&[], &p, 101.);
        assert_eq!(c.message, message);
        assert_eq!(
            mood(true, &c, 102., true, 100, 10_000., false),
            Mood::LevelUp
        );
        c.expire(105.);
        assert!(c.message.is_none());
        assert_eq!(
            mood(true, &c, 105., true, 100, 10_000., false),
            Mood::Working
        );
    }
    #[test]
    fn shiny_hatch_beats_overflow_evolution_but_no_instant_graduate_celebration() {
        let mut p = hatched();
        p.active.as_mut().unwrap().shiny = true;
        let mut c = Celebration::default();
        c.observe(
            &[
                Event::Hatched("Bulbasaur".into()),
                Event::Evolved("Ivysaur".into()),
            ],
            &p,
            100.,
        );
        assert!(c.shiny);
        assert_eq!(
            c.message,
            Some(crate::pokemon::format::Language::En.hatch("Bulbasaur"))
        );
        p.active = None;
        c.observe(
            &[
                Event::Hatched("Bulbasaur".into()),
                Event::Graduated("Venusaur".into()),
            ],
            &p,
            101.,
        );
        assert!(c.message.is_none());
    }
}
