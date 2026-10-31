//! Native appearance regressions and actual Metal pixels share deterministic runtime-only art.
use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::pokemon::{Balance, EvoLine, EvoNode, Rarity, forms::Form};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
use std::sync::Arc;
fn mon(species: u16, shiny: bool, form: Option<Form>) -> Mon {
    let mut p = Companion::default();
    p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    p.hatch(EvoLine {
        base_id: species,
        tree: EvoNode {
            species_id: species,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(
            species,
            BTreeMap::from([(
                "en".into(),
                if species == 492 {
                    "Shaymin".into()
                } else {
                    "Pikachu".into()
                },
            )]),
        )]),
    });
    let mut m = p.active.unwrap();
    m.shiny = shiny;
    m.form = form;
    m
}
fn details(species: u16) -> profile::Details {
    profile::Details {
        species_id: species,
        types: vec!["grass".into()],
        gender_rate: -1,
        base_stats: BTreeMap::new(),
        abilities: vec![],
        moves: vec![],
    }
}
#[test]
fn all_unown_letters_and_colors_have_distinct_subject_identities() {
    let mut identities: BTreeSet<SpriteIdentity> = BTreeSet::new();
    for form in crate::pokemon::forms::forms(201) {
        for shiny in [false, true] {
            let identity = mon_sprite_identity(&mon(201, shiny, Some(form.clone())), 201);
            assert!(identities.insert(identity));
        }
    }
    assert_eq!(identities.len(), 56);
}
#[test]
fn legacy_a_is_canonical_and_unrelated_species_ignore_unown_letters() {
    let legacy = mon_sprite_identity(&mon(201, false, None), 201);
    let explicit = mon_sprite_identity(&mon(201, false, Some(Form("a".into()))), 201);
    assert_eq!(legacy, explicit);
    for form in crate::pokemon::forms::forms(201) {
        assert_eq!(
            mon_sprite_identity(&mon(25, false, Some(form)), 25),
            (25, false, None)
        );
    }
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn form_notes_follow_loaded_metadata_and_the_displayed_individual(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon.active = Some(mon(492, false, Some(Form("sky".into()))));
    state
        .pokemon
        .collection
        .push(mon(492, false, Some(Form("land".into()))));
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        assert!(w.try_find("pokemon-home-form-metadata-note").is_none());
        shark.update(cx, |s, cx| {
            s.pokemon_ui.details.insert(492, details(492));
            cx.notify();
        });
        w.render_frame(cx);
        assert!(w.find("pokemon-home-form-metadata-note").visible());
        w.click("pokemon-home-open-details", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-form-metadata-note").visible());
        w.click("pokemon-form-land", cx);
        w.render_frame(cx);
        assert!(w.try_find("pokemon-form-metadata-note").is_none());
        w.click("pokemon-form-sky", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-form-metadata-note").visible());
        shark.update(cx, |s, cx| {
            s.pokemon_ui.details.remove(&492);
            cx.notify();
        });
        w.render_frame(cx);
        assert!(w.try_find("pokemon-form-metadata-note").is_none());
        w.click("pokemon-tab-Home", cx);
        w.render_frame(cx);
        assert!(w.try_find("pokemon-home-form-metadata-note").is_none());
        shark.update(cx, |s, cx| {
            s.pokemon_ui.details.insert(492, details(492));
            s.state.pokemon.active.as_mut().unwrap().form = Some(Form("land".into()));
            cx.notify();
        });
        w.render_frame(cx);
        assert!(w.try_find("pokemon-home-form-metadata-note").is_none());
        shark.update(cx, |s, cx| {
            s.state.pokemon.active = None;
            cx.notify();
        });
        w.render_frame(cx);
        assert!(w.try_find("pokemon-home-form-metadata-note").is_none());
    })
    .unwrap();
}
fn png(directory: &Path, shiny: bool) -> Arc<crate::pokemon::sprites::Asset> {
    let mut bytes = std::io::Cursor::new(vec![]);
    image::RgbaImage::from_pixel(
        12,
        20,
        image::Rgba(if shiny {
            [0, 0, 255, 255]
        } else {
            [255, 0, 0, 255]
        }),
    )
    .write_to(&mut bytes, image::ImageFormat::Png)
    .unwrap();
    crate::pokemon::sprites::load_with(
        directory,
        crate::pokemon::sprites::Request::pokemon(201, false, shiny, Some(&Form("b".into()))),
        |_| Ok(bytes.get_ref().clone()),
    )
    .unwrap()
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn static_unown_never_uses_a_normal_cache_entry_for_a_shiny_subject(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let normal = png(&home.join("unown-static"), false);
    let shiny = png(&home.join("unown-static"), true);
    let mut state = State::default();
    state.pokemon.active = Some(mon(201, true, Some(Form("b".into()))));
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            let normal_key = (201, false, Some(Form("b".into())));
            let shiny_key = (201, true, Some(Form("b".into())));
            s.cancel_pokemon_sprite();
            s.pokemon_ui.sprite = None;
            s.pokemon_ui.sprite_key = None;
            s.pokemon_ui
                .thumbnails
                .insert(normal_key.clone(), normal.path.clone());
            s.load_pokemon_sprite(cx);
            assert_eq!(s.pokemon_ui.sprite_key, Some(shiny_key.clone()));
            assert!(
                s.pokemon_ui.sprite.is_none(),
                "normal cache must not flash under shiny selection"
            );
            s.complete_pokemon_sprite(
                shiny_key.clone(),
                s.pokemon_ui.generation,
                s.pokemon_ui.sprite_request,
                Ok(shiny.path.clone()),
                cx,
            );
            assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&shiny.path));
            let shiny_image = s.pokemon_ui.footer_render.clone().unwrap();
            s.state.pokemon.active.as_mut().unwrap().shiny = false;
            s.load_pokemon_sprite(cx);
            assert_eq!(s.pokemon_ui.sprite.as_ref(), Some(&normal.path));
            assert!(!Arc::ptr_eq(
                &shiny_image,
                s.pokemon_ui.footer_render.as_ref().unwrap()
            ));
            s.pokemon_ui.thumbnails.remove(&shiny_key);
            s.state.pokemon.active.as_mut().unwrap().shiny = true;
            s.load_pokemon_sprite(cx);
            assert_eq!(s.pokemon_ui.sprite_key, Some(shiny_key.clone()));
            assert!(s.pokemon_ui.sprite.is_none());
            s.complete_pokemon_sprite(
                shiny_key,
                s.pokemon_ui.generation,
                s.pokemon_ui.sprite_request,
                Ok(shiny.path.clone()),
                cx,
            );
            assert!(Arc::ptr_eq(
                &shiny_image,
                s.pokemon_ui.footer_render.as_ref().unwrap()
            ));
        });
    })
    .unwrap();
}
