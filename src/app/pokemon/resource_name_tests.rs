use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
fn state() -> State {
    let mut state = State::default();
    state.pokemon.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    state.pokemon.hatch_with_disguise_roll(
        crate::pokemon::EvoLine {
            base_id: 492,
            tree: crate::pokemon::EvoNode {
                species_id: 492,
                children: vec![],
            },
            rarity: Rarity::Legendary,
            names: BTreeMap::from([(492, BTreeMap::from([("en".into(), "Shaymin".into())]))]),
        },
        false,
    );
    let mon = state.pokemon.active.as_mut().unwrap();
    mon.form = Some(crate::pokemon::forms::Form("sky".into()));
    let profile = mon.profile.as_mut().unwrap();
    profile.ability_name = Some("serene-grace".into());
    profile.ability_slot = Some(1);
    profile.moves = vec![profile::KnownMove {
        name: "seed-flare".into(),
        learned_at_level: 1,
    }];
    state
}
fn metadata() -> profile::Details {
    profile::Details {
        species_id: 492,
        types: vec!["grass".into(), "flying".into()],
        gender_rate: -1,
        base_stats: BTreeMap::new(),
        abilities: vec![profile::Ability {
            name: "serene-grace".into(),
            slot: 1,
            is_hidden: false,
        }],
        moves: vec![profile::Move {
            name: "seed-flare".into(),
            learn_methods: vec![profile::LearnMethod {
                method: "level-up".into(),
                level: 1,
            }],
        }],
    }
}
fn publish(s: &mut Shark, kind: NameKind, name: &str, label: &str, cx: &mut Context<Shark>) {
    let resource = NameResource::new(kind, name);
    let view = *s.pokemon_ui.resource_pending.get(&resource).unwrap();
    s.complete_resource_names(
        resource,
        s.pokemon_ui.generation,
        view,
        Ok(BTreeMap::from([
            ("en".into(), label.into()),
            ("ko".into(), "번역".into()),
        ])),
        cx,
    );
}
// Native English rendering retains Shark's interface language; multilingual resolution is covered by the shared pure labels.
// upstream: PokemonNameLocalizationTests.swift::testMountedLabelLoadsTranslationsAndReopensWithoutEnglishFrame
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn mounted_labels_update_and_reopen_from_snapshot_without_placeholder(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, state(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.show_pokemon(w, cx);
            s.select_pokemon(492, cx);
            s.complete_pokemon_details(492, s.pokemon_ui.generation, Ok(metadata()), cx);
        });
        w.render_frame(cx);
        assert_eq!(w.find("pokemon-ability-name").label(), Some("Ability: …"));
        assert_eq!(w.find("pokemon-move-names").label(), Some("Moves: …"));
        assert_eq!(w.find("pokemon-type-grass").label(), Some("…"));
        shark.update(cx, |s, cx| {
            publish(s, NameKind::Ability, "serene-grace", "Serene Grace", cx);
            publish(s, NameKind::Move, "seed-flare", "Seed Flare", cx);
            publish(s, NameKind::Type, "grass", "Grass", cx);
            publish(s, NameKind::Type, "flying", "Flying", cx);
            publish(s, NameKind::PokemonForm, "10064", "Sky Forme", cx);
        });
        w.render_frame(cx);
        assert_eq!(
            w.find("pokemon-ability-name").label(),
            Some("Ability: Serene Grace")
        );
        assert_eq!(
            w.find("pokemon-move-names").label(),
            Some("Moves: Seed Flare")
        );
        assert_eq!(w.find("pokemon-type-grass").label(), Some("Grass"));
        assert_eq!(w.find("pokemon-form-name").label(), Some("Form: Sky Forme"));
        assert!(w.find("pokemon-ability-name").bounds().size.width > px(0.));
        w.click("pokemon-dex-back", cx);
        shark.update(cx, |s, cx| s.select_pokemon(492, cx));
        w.render_frame(cx);
        assert_eq!(
            w.find("pokemon-ability-name").label(),
            Some("Ability: Serene Grace")
        );
        assert_eq!(
            w.find("pokemon-move-names").label(),
            Some("Moves: Seed Flare")
        );
        let mon = shark.read(cx).state.pokemon.active.as_ref().unwrap();
        assert_eq!(
            mon.profile.as_ref().unwrap().ability_name.as_deref(),
            Some("serene-grace")
        );
        assert_eq!(mon.profile.as_ref().unwrap().moves[0].name, "seed-flare");
        assert_eq!(
            shark.read(cx).pokemon_ui.details[&492].types,
            ["grass", "flying"]
        );
    })
    .unwrap();
}
// upstream: PokemonNameLocalizationTests.swift::testCancelledLoadDoesNotPublishSnapshot
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn cancelled_view_does_not_publish_late_name_snapshot(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, state(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.show_pokemon(w, cx);
            s.select_pokemon(492, cx);
            s.complete_pokemon_details(492, s.pokemon_ui.generation, Ok(metadata()), cx);
            let resource = NameResource::new(NameKind::Move, "seed-flare");
            let view = s.pokemon_ui.resource_pending[&resource];
            let generation = s.pokemon_ui.generation;
            s.close_usage(w, cx);
            assert!(s.usage_panel.is_none());
            s.complete_resource_names(
                resource.clone(),
                generation,
                view,
                Ok(BTreeMap::from([("en".into(), "Seed Flare".into())])),
                cx,
            );
            assert!(!s.pokemon_ui.resource_names.contains_key(&resource));
            assert!(!s.pokemon_ui.resource_pending.contains_key(&resource));
        })
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn failed_metadata_labels_retry_on_remount_and_old_callbacks_cannot_unlock_new_requests(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, state(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.select_pokemon(492, cx);
            s.complete_pokemon_details(492, s.pokemon_ui.generation, Ok(metadata()), cx);
            let resource = NameResource::new(NameKind::Move, "seed-flare");
            let view = s.pokemon_ui.resource_pending[&resource];
            let generation = s.pokemon_ui.generation;
            s.complete_resource_names(
                resource.clone(),
                generation,
                view,
                Err("offline".into()),
                cx,
            );
            assert_eq!(s.resource_label(NameKind::Move, "seed-flare"), "Seed Flare");
            assert!(!s.pokemon_ui.resource_names.contains_key(&resource));
            s.sync_profile_names(true, cx);
            assert!(s.pokemon_ui.resource_pending.contains_key(&resource));
            let current = s.pokemon_ui.resource_pending[&resource];
            s.complete_resource_names(
                resource.clone(),
                generation,
                view,
                Ok(BTreeMap::from([("en".into(), "stale".into())])),
                cx,
            );
            assert_eq!(s.pokemon_ui.resource_pending[&resource], current);
            assert!(!s.pokemon_ui.resource_names.contains_key(&resource));
            s.complete_resource_names(
                resource.clone(),
                generation,
                current,
                Ok(BTreeMap::from([("en".into(), "Seed Flare".into())])),
                cx,
            );
            assert_eq!(s.resource_label(NameKind::Move, "seed-flare"), "Seed Flare");
        })
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn details_arrival_does_not_erase_an_earlier_move_name_failure(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, state(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            s.select_pokemon(492, cx);
            let resource = NameResource::new(NameKind::Move, "seed-flare");
            let view = s.pokemon_ui.resource_pending[&resource];
            s.complete_resource_names(
                resource.clone(),
                s.pokemon_ui.generation,
                view,
                Err("offline".into()),
                cx,
            );
            assert_eq!(s.resource_label(NameKind::Move, "seed-flare"), "Seed Flare");
            s.complete_pokemon_details(492, s.pokemon_ui.generation, Ok(metadata()), cx);
            assert_eq!(s.resource_label(NameKind::Move, "seed-flare"), "Seed Flare");
            assert!(s.pokemon_ui.resource_failed.contains(&resource));
            assert!(!s.pokemon_ui.resource_pending.contains_key(&resource));
        })
    })
    .unwrap();
}
