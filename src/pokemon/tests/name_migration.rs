use super::super::*;
// upstream: DexNameMigrationTests.swift::testNewNamesSkipMigrationButEmptyAndMissingSpeciesRetry
#[test]
fn complete_names_version_requires_every_reached_species() {
    let mut state = super::economy::hatched();
    state.add_growth(125_000_000);
    let mon = state.active.as_mut().unwrap();
    assert!(!mon.needs_names_refresh());
    let restored: Mon = serde_json::from_value(serde_json::to_value(&mon).unwrap()).unwrap();
    assert!(!restored.needs_names_refresh());
    mon.line.names.clear();
    assert!(mon.needs_names_refresh());
    mon.line
        .names
        .insert(1, BTreeMap::from([("en".into(), "Bulbasaur".into())]));
    assert!(mon.needs_names_refresh());
    mon.line
        .names
        .insert(2, BTreeMap::from([("en".into(), "Ivysaur".into())]));
    mon.names_version = None;
    assert!(mon.needs_names_refresh());
}
