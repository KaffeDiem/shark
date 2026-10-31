use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use core::prelude::v1::test;
fn names() -> crate::pokemon::EvoLine {
    crate::pokemon::EvoLine {
        base_id: 1,
        tree: crate::pokemon::EvoNode {
            species_id: 1,
            children: vec![crate::pokemon::EvoNode {
                species_id: 2,
                children: vec![],
            }],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([
            (
                1,
                BTreeMap::from([
                    ("en".into(), "Bulbasaur".into()),
                    ("ko".into(), "이상해씨".into()),
                    ("it".into(), "Bulbasaur".into()),
                ]),
            ),
            (
                2,
                BTreeMap::from([
                    ("en".into(), "Ivysaur".into()),
                    ("ko".into(), "이상해풀".into()),
                    ("it".into(), "Ivysaur".into()),
                ]),
            ),
        ]),
    }
}
fn legacy() -> Companion {
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    state.hatch_with_disguise_roll(names(), false);
    let mut mon = state.active.take().unwrap();
    mon.stage = 1;
    mon.tokens = 42;
    mon.shiny = true;
    mon.nature = "Brave".into();
    mon.caught_at = 123;
    mon.names_version = None;
    for names in mon.line.names.values_mut() {
        names.retain(|code, _| code == "en");
    }
    mon.profile.as_mut().unwrap().instance_id = "first".into();
    let mut second = mon.clone();
    second.profile.as_mut().unwrap().instance_id = "second".into();
    state.collection = vec![mon, second];
    state.lifetime_tokens = 1234567;
    state.inventory.candies = 3;
    state
}
// upstream: DexNameMigrationTests.swift::testLegacyNamedEntriesRefreshAllLanguagesOnceAndPreserveProgress
// upstream: DexNameMigrationTests.swift::testSimultaneouslyMountedCatchRowsShareOneRequest
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn legacy_name_upgrade_coalesces_by_base_and_preserves_individuals(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = legacy();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            let before = s.state.pokemon.clone();
            assert_eq!(before.collection[0].names_version, None);
            assert_eq!(
                before.collection[0].line.localized_name(1, "ko"),
                "Bulbasaur"
            );
            s.request_dex_names(cx);
            s.request_dex_names(cx);
            assert_eq!(s.pokemon_ui.name_pending, BTreeSet::from([1]));
            s.complete_pokemon_names(1, s.pokemon_ui.generation, Ok(names()), cx);
            assert!(s.pokemon_ui.name_pending.is_empty());
            assert_eq!(s.state.pokemon.collection.len(), 2);
            for (after, before) in s
                .state
                .pokemon
                .collection
                .iter()
                .zip(before.collection.iter())
            {
                assert!(!after.needs_names_refresh());
                assert_eq!(after.line.localized_name(1, "ko"), "이상해씨");
                assert_eq!(after.line.localized_name(2, "it"), "Ivysaur");
                assert_eq!(after.caught_at, before.caught_at);
                assert_eq!(after.shiny, before.shiny);
                assert_eq!(after.nature, before.nature);
                assert_eq!(after.tokens, before.tokens);
                assert_eq!(after.stage, before.stage);
                assert_eq!(after.path, before.path);
                assert_eq!(
                    serde_json::to_value(&after.profile).unwrap(),
                    serde_json::to_value(&before.profile).unwrap()
                );
            }
            assert_eq!(s.state.pokemon.lifetime_tokens, 1234567);
            assert_eq!(s.state.pokemon.inventory.candies, 3);
            s.request_dex_names(cx);
            assert!(s.pokemon_ui.name_pending.is_empty());
            let saved = State::load();
            assert!(
                saved
                    .pokemon
                    .collection
                    .iter()
                    .all(|mon| !mon.needs_names_refresh())
            );
            assert_eq!(
                saved.pokemon.collection[0].line.localized_name(1, "pt"),
                "Bulbasaur"
            );
        })
    })
    .unwrap();
}
// upstream: DexNameMigrationTests.swift::testOfflineLegacyCacheRemainsVisibleAndRetriesAfterRecovery
// upstream: CompanionTests.swift::testBackfillRetriesAfterAnOfflineAttempt
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn offline_name_refresh_preserves_fallback_and_retries_on_navigation(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = legacy();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.request_dex_names(cx);
            s.complete_pokemon_names(1, s.pokemon_ui.generation, Err("offline".into()), cx);
            assert_eq!(
                s.state.pokemon.collection[0].line.localized_name(2, "ko"),
                "Ivysaur"
            );
            assert!(s.state.pokemon.collection[0].needs_names_refresh());
            assert!(s.pokemon_ui.name_pending.is_empty());
            s.load_dex_assets(cx);
            assert_eq!(s.pokemon_ui.name_pending, BTreeSet::from([1]));
            s.complete_pokemon_names(1, s.pokemon_ui.generation, Ok(names()), cx);
            assert!(!s.state.pokemon.collection[0].needs_names_refresh());
            assert_eq!(
                s.state.pokemon.collection[0].line.localized_name(2, "ko"),
                "이상해풀"
            );
        })
    })
    .unwrap();
}
// upstream: DexNameMigrationTests.swift::testPartialResponsePreservesOldNamesWithoutMarkingMigrationComplete
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn partial_name_response_preserves_old_species_and_remains_retryable(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = legacy();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            let mut partial = names();
            partial.names.remove(&2);
            s.request_dex_names(cx);
            s.complete_pokemon_names(1, s.pokemon_ui.generation, Ok(partial), cx);
            let mon = &s.state.pokemon.collection[0];
            assert_eq!(mon.line.localized_name(1, "ko"), "이상해씨");
            assert_eq!(mon.line.localized_name(2, "ko"), "Ivysaur");
            assert!(mon.needs_names_refresh());
            assert!(s.pokemon_ui.name_pending.is_empty());
            s.load_dex_assets(cx);
            assert_eq!(s.pokemon_ui.name_pending, BTreeSet::from([1]));
            s.complete_pokemon_names(1, s.pokemon_ui.generation, Ok(names()), cx);
            assert!(!s.state.pokemon.collection[0].needs_names_refresh());
            assert_eq!(
                s.state.pokemon.collection[0].line.names[&2]["it"],
                "Ivysaur"
            );
        })
    })
    .unwrap();
}
// upstream: CompanionTests.swift::testBackfillFillsNamesForEntriesSavedBeforeNamesExisted
// upstream: CompanionTests.swift::testDexResolveChainNamesFetchesWhenUnstored
// upstream: CompanionTests.swift::testDexResolveChainNamesBackfillsLegacyEntry
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn missing_chain_names_are_backfilled_and_saved(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = legacy();
    for mon in &mut state.pokemon.collection {
        mon.line.names.clear();
    }
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            assert_eq!(
                s.state
                    .pokemon
                    .dex_species()
                    .iter()
                    .map(|cell| cell.name.as_str())
                    .collect::<Vec<_>>(),
                ["#1", "#2"]
            );
            s.request_dex_names(cx);
            assert_eq!(s.pokemon_ui.name_pending, BTreeSet::from([1]));
            s.complete_pokemon_names(1, s.pokemon_ui.generation, Ok(names()), cx);
            assert_eq!(
                s.state
                    .pokemon
                    .dex_species()
                    .iter()
                    .map(|cell| cell.name.as_str())
                    .collect::<Vec<_>>(),
                ["Bulbasaur", "Ivysaur"]
            );
            assert_eq!(
                State::load().pokemon.collection[0].line.names[&2]["ko"],
                "이상해풀"
            );
        })
    })
    .unwrap();
}
// upstream: CompanionTests.swift::testBackfillDoesNotFetchWhenNamesAreAlreadyStored
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn complete_versioned_names_skip_network_backfill(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = legacy();
    state.pokemon.merge_line_names(&names());
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.request_dex_names(cx);
            assert!(s.pokemon_ui.name_pending.is_empty());
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stale_name_refresh_cannot_mutate_imported_progress(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = legacy();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.request_dex_names(cx);
            let old = s.pokemon_ui.generation;
            s.apply_pokemon_save(legacy(), cx);
            let before = serde_json::to_value(&s.state.pokemon).unwrap();
            s.complete_pokemon_names(1, old, Ok(names()), cx);
            assert_eq!(serde_json::to_value(&s.state.pokemon).unwrap(), before);
            assert!(s.pokemon_ui.name_pending.is_empty());
        })
    })
    .unwrap();
}
