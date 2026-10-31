use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use core::prelude::v1::test;

fn line(id: u16) -> crate::pokemon::EvoLine {
    crate::pokemon::EvoLine {
        base_id: id,
        tree: crate::pokemon::EvoNode {
            species_id: id,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::new(),
    }
}
fn ready() -> Companion {
    let mut state = Companion::default();
    state.egg_tokens = state.progress().1;
    state
}
fn hatched(id: u16) -> Companion {
    let mut state = ready();
    state.hatch_with_disguise_roll(line(id), false);
    state
}

// upstream: CompanionTests.swift::testFailedHatchRetriesAfterProviderRecovers
// upstream: CompanionTests.swift::testFailedHatchShowsDelayUntilNextRefresh
// upstream: CompanionTests.swift::testSamplerOfflineKeepsEgg
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_hatch_keeps_progress_releases_lock_and_recovers(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon = ready();
            let before = s.state.pokemon.egg_tokens;
            let generation = s.pokemon_ui.generation;
            s.pokemon_ui.hatch_pending = true;
            assert!(!s.pokemon_ui.hatch_retry_delayed);
            s.complete_pokemon_hatch(generation, None, false, Err("offline".into()), cx);
            assert!(!s.pokemon_ui.hatch_pending);
            assert!(s.pokemon_ui.hatch_retry_delayed);
            assert!(s.state.pokemon.active.is_none());
            assert_eq!(s.state.pokemon.egg_tokens, before);
            assert!(s.pokemon_ui.notice.as_ref().unwrap().contains("offline"));
            s.pokemon_ui.hatch_pending = true;
            s.complete_pokemon_hatch(generation, None, false, Ok(line(88)), cx);
            assert!(!s.pokemon_ui.hatch_pending);
            assert!(!s.pokemon_ui.hatch_retry_delayed);
            assert_eq!(s.state.pokemon.active.as_ref().unwrap().species_id(), 88);
            assert!(!s.pokemon_ui.notice.as_ref().unwrap().contains("offline"));
        })
    })
    .unwrap();
}

// upstream: CompanionTests.swift::testPrefetchFailureMarksEggThatBecameReadyDuringRequest
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn prefetch_failure_uses_current_hatch_readiness(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon = ready();
            s.state.pokemon.egg_tokens -= 1;
            s.pokemon_ui.hatch_pending = true;
            let generation = s.pokemon_ui.generation;
            s.state.pokemon.add_growth(1);
            s.complete_pokemon_hatch(generation, None, false, Err("offline".into()), cx);
            assert!(s.pokemon_ui.hatch_retry_delayed);
            assert!(s.state.pokemon.ready_to_hatch());
            assert!(!s.pokemon_ui.hatch_pending);
        })
    })
    .unwrap();
}

// upstream: CompanionTests.swift::testApplySaveResetsHatchRetryDelay
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn importing_fresh_egg_clears_old_retry_delay(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon = ready();
            s.complete_pokemon_hatch(
                s.pokemon_ui.generation,
                None,
                false,
                Err("offline".into()),
                cx,
            );
            assert!(s.pokemon_ui.hatch_retry_delayed);
            s.apply_pokemon_save(Companion::default(), cx);
            assert!(!s.pokemon_ui.hatch_retry_delayed);
            assert!(s.pokemon_ui.notice.is_none());
            assert_eq!(s.state.pokemon.egg_tokens, 0);
        })
    })
    .unwrap();
}

// Rust combines selection and line loading in one request; both stale source phases share this guard.
// upstream: CompanionTests.swift::testStaleSpeciesSelectionFailureDoesNotMarkImportedEggDelayed
// upstream: CompanionTests.swift::testStaleLineFailureDoesNotMarkImportedEggDelayed
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn stale_egg_failures_cannot_mark_imported_save_delayed(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            for prepared in [false, true] {
                s.state.pokemon = ready();
                if prepared {
                    s.state.pokemon.prepare_egg(line(25));
                }
                let old = s.pokemon_ui.generation;
                s.pokemon_ui.hatch_pending = true;
                s.apply_pokemon_save(Companion::default(), cx);
                let before = serde_json::to_value(&s.state.pokemon).unwrap();
                s.complete_pokemon_hatch(old, None, false, Err("stale failure".into()), cx);
                assert!(!s.pokemon_ui.hatch_retry_delayed);
                assert!(s.pokemon_ui.notice.is_none());
                assert_eq!(s.state.pokemon.egg_tokens, 0);
                assert_eq!(serde_json::to_value(&s.state.pokemon).unwrap(), before);
            }
        })
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn successful_prefetch_clears_obsolete_failure_before_hatch(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon.egg_tokens = 1;
            let generation = s.pokemon_ui.generation;
            s.complete_pokemon_hatch(generation, None, false, Err("offline".into()), cx);
            assert!(s.pokemon_ui.notice.is_some());
            assert!(!s.pokemon_ui.hatch_retry_delayed);
            s.complete_pokemon_hatch(generation, None, false, Ok(line(25)), cx);
            assert!(s.pokemon_ui.notice.is_none());
            assert!(!s.pokemon_ui.hatch_retry_delayed);
            assert!(s.state.pokemon.active.is_none());
            assert!(s.state.pokemon.pending_line.is_some());
        })
    })
    .unwrap();
}

// The persisted Rust tree eliminates suspended active-line loading; metadata callbacks enrich live state.
// upstream: CompanionTests.swift::testLineLoadPreservesUpdatesMadeWhileProviderIsSuspended
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn late_metadata_completion_preserves_live_growth_and_minted_nature(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon = hatched(25);
            s.state.pokemon.inventory.mints = 1;
            s.request_pokemon_details(25, cx);
            s.state.pokemon.add_growth(42);
            s.state.pokemon.use_mint().unwrap();
            let nature = s.state.pokemon.active.as_ref().unwrap().nature.clone();
            s.complete_pokemon_details(
                25,
                s.pokemon_ui.generation,
                Ok(profile::Details {
                    species_id: 25,
                    types: vec![],
                    gender_rate: -1,
                    base_stats: BTreeMap::new(),
                    abilities: vec![],
                    moves: vec![],
                }),
                cx,
            );
            assert_eq!(s.state.pokemon.active.as_ref().unwrap().tokens, 42);
            assert_eq!(s.state.pokemon.active.as_ref().unwrap().nature, nature);
            let persisted = State::load();
            assert_eq!(persisted.pokemon.active.as_ref().unwrap().tokens, 42);
            assert_eq!(persisted.pokemon.active.as_ref().unwrap().nature, nature);
        })
    })
    .unwrap();
}

// upstream: CompanionTests.swift::testShinyBurstSurvivesOverflowEvolve
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn shiny_hatch_celebration_survives_immediate_overflow_evolution(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon.egg_tokens = 135_000_000;
            s.state.pokemon.random_seed = (0..20_000)
                .find(|seed| {
                    let mut rng = *seed;
                    Companion::rolls_shiny(crate::pokemon::next_random(&mut rng), false)
                })
                .unwrap();
            let mut three = line(1);
            three.tree.children = vec![crate::pokemon::EvoNode {
                species_id: 2,
                children: vec![crate::pokemon::EvoNode {
                    species_id: 3,
                    children: vec![],
                }],
            }];
            s.complete_pokemon_hatch(s.pokemon_ui.generation, None, false, Ok(three), cx);
            let mon = s.state.pokemon.active.as_ref().unwrap();
            assert!(mon.shiny);
            assert_eq!(mon.stage, 1);
            assert!(s.pokemon_ui.celebration.shiny);
            assert!(
                s.pokemon_ui
                    .celebration
                    .message
                    .as_ref()
                    .unwrap()
                    .contains("hatched")
            );
            assert!(
                !s.pokemon_ui
                    .celebration
                    .message
                    .as_ref()
                    .unwrap()
                    .contains("evolved")
            );
        })
    })
    .unwrap();
}

// upstream: CompanionTests.swift::testHatchCelebrationSkippedOnInstantGraduate
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn instant_graduation_does_not_celebrate_a_departed_hatch(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon.egg_tokens = 800_000_000;
            s.complete_pokemon_hatch(s.pokemon_ui.generation, None, false, Ok(line(20)), cx);
            assert!(s.state.pokemon.active.is_none());
            assert_eq!(s.state.pokemon.collection.len(), 1);
            assert!(s.pokemon_ui.celebration.message.is_none());
            assert!(!s.pokemon_ui.celebration.shiny);
            assert_eq!(s.pokemon_ui.celebration.until, 0.);
        })
    })
    .unwrap();
}

// The complete persisted pre-roll survives a failed request instead of re-rolling its form.
// upstream: UnownTests.swift::testPendingLetterSurvivesFailedLineLoadAndHatchesOnRetry
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn prepared_form_survives_failure_save_and_retry(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.state.pokemon = ready();
            s.state.pokemon.egg_tokens += 123;
            s.state.pokemon.prepare_egg(line(201));
            s.state.pokemon.pending_form = Some(crate::pokemon::forms::Form("question".into()));
            s.complete_pokemon_hatch(
                s.pokemon_ui.generation,
                None,
                false,
                Err("offline".into()),
                cx,
            );
            assert!(s.state.pokemon.active.is_none());
            assert_eq!(s.state.pokemon.pending_line.as_ref().unwrap().base_id, 201);
            assert_eq!(s.state.pokemon.pending_form.as_ref().unwrap().0, "question");
            s.save_pokemon();
            let saved = State::load();
            assert_eq!(saved.pokemon.pending_form.as_ref().unwrap().0, "question");
            s.complete_pokemon_hatch(s.pokemon_ui.generation, None, false, Ok(line(201)), cx);
            let mon = s.state.pokemon.active.as_ref().unwrap();
            assert_eq!(mon.species_id(), 201);
            assert_eq!(mon.form.as_ref().unwrap().0, "question");
            assert_eq!(mon.tokens, 123);
            assert!(s.state.pokemon.pending_line.is_none());
            assert!(s.state.pokemon.pending_form.is_none());
        })
    })
    .unwrap();
}
