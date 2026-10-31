use super::super::*;
fn state(rarity: Rarity, shiny: bool) -> Companion {
    let mut s = super::economy::hatched();
    let mon = s.active.as_mut().unwrap();
    mon.line.rarity = rarity;
    mon.shiny = shiny;
    s
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionIdentifiesLegendaryNonShiny
#[test]
fn legendary_nonshiny_is_valuable() {
    let s = state(Rarity::Legendary, false);
    assert!(!s.active.as_ref().unwrap().displays_shiny());
    assert!(s.is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionIdentifiesShinyNonLegendary
#[test]
fn shiny_common_is_valuable() {
    let s = state(Rarity::Common, true);
    assert!(s.active.as_ref().unwrap().displays_shiny());
    assert!(s.is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionIdentifiesShinyLegendary
#[test]
fn shiny_legendary_is_valuable() {
    assert!(state(Rarity::Legendary, true).is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionExcludesCommonNonShiny
#[test]
fn normal_common_is_not_valuable() {
    assert!(!state(Rarity::Common, false).is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionExcludesUncommonNonShiny
#[test]
fn normal_uncommon_is_not_valuable() {
    assert!(!state(Rarity::Uncommon, false).is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionExcludesRareNonShinyToAvoidRerollFatigue
#[test]
fn normal_rare_is_not_valuable() {
    assert!(!state(Rarity::Rare, false).is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testHighValueCompanionIsFalseWhenNoActive
#[test]
fn egg_is_not_valuable() {
    assert!(!Companion::default().is_high_value_companion());
}
// upstream: PreciousCompanionGuardTests.swift::testDisguisedDittoDoesNotLeakHighValueStatusBeforeReveal
#[test]
fn disguise_never_leaks_hidden_shiny_identity() {
    let mut s = state(Rarity::Common, true);
    let mon = s.active.as_mut().unwrap();
    mon.ditto_disguise = Some(1);
    mon.ditto_revealed = false;
    assert!(!mon.displays_shiny());
    assert!(!s.is_high_value_companion());
    let mon = s.active.as_mut().unwrap();
    mon.ditto_revealed = true;
    mon.line.rarity = Rarity::Rare;
    assert!(mon.displays_shiny());
    assert!(s.is_high_value_companion());
}
