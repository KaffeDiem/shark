//! Exercise the app-to-chat bridge with actual caught individuals and decoded assets.
use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::chat::ui_tests::{activity_snapshot, set_activity_tasks};
use crate::state::{ChatMeta, Workspace};
use core::prelude::v1::test;

fn caught(species: u16) -> Mon {
    let mut p = Companion::default();
    p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    p.hatch(crate::pokemon::EvoLine {
        base_id: species,
        tree: crate::pokemon::EvoNode {
            species_id: species,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(
            species,
            BTreeMap::from([("en".into(), format!("Species {species}"))]),
        )]),
    });
    let mut mon = p.active.unwrap();
    mon.shiny = false;
    mon.profile.as_mut().unwrap().instance_id = format!("caught-{species}");
    mon
}

fn tasks(count: usize) -> Vec<crate::agent::Task> {
    (0..count)
        .map(|i| crate::agent::Task {
            id: format!("task-{i}"),
            kind: "local_agent".into(),
            description: format!("Agent {i}"),
        })
        .collect()
}

fn chat(s: &mut Shark, count: usize, window: &mut Window, cx: &mut Context<Shark>) -> Entity<Chat> {
    let meta = ChatMeta {
        id: "activity-integration".into(),
        ..Default::default()
    };
    let entity = cx.new(|cx| Chat::new(Workspace::default(), meta.clone(), false, window, cx));
    entity.update(cx, |chat, cx| set_activity_tasks(chat, tasks(count), cx));
    s.chats.insert(meta.id, entity.clone());
    entity
}

fn seed_portraits(s: &mut Shark, directory: &std::path::Path) {
    std::fs::create_dir_all(directory).unwrap();
    for pokemon in crate::chat::activity::roster(&s.state.pokemon) {
        let path = directory.join(format!("{}.png", pokemon.appearance.0));
        image::RgbaImage::from_pixel(3, 4, image::Rgba([80, 160, 220, 255]))
            .save(&path)
            .unwrap();
        let asset = crate::pokemon::sprites::cached(&path).expect("validated test portrait");
        s.pokemon_ui
            .thumbnails
            .insert(pokemon.appearance, asset.path.clone());
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn collected_and_active_portraits_reach_overflow_hover_tasks(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = State::default();
    state.pokemon.collection = (1..=6).map(caught).collect();
    state.pokemon.active = Some(caught(7));
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, window, cx| {
        shark.update(cx, |s, cx| {
            let chat = chat(s, 7, window, cx);
            seed_portraits(s, &home.join("activity-art"));
            s.refresh_chat_pokemon(cx);
            let (snapshot, hopping) = activity_snapshot(chat.read(cx));
            assert_eq!(snapshot.len(), 7);
            assert!(snapshot.iter().all(|(_, _, portrait)| *portrait));
            let species: BTreeSet<_> = snapshot
                .iter()
                .map(|(_, appearance, _)| appearance.0)
                .collect();
            assert_eq!(species, (1..=7).collect());
            assert!(hopping);
            assert_eq!(chat.read(cx).background, tasks(7));
        })
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn disabling_and_importing_refresh_existing_chat_assignments(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = test_home();
    let mut state = State::default();
    state.pokemon.collection = vec![caught(25)];
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, window, cx| {
        shark.update(cx, |s, cx| {
            let chat = chat(s, 1, window, cx);
            seed_portraits(s, &home.join("activity-art"));
            s.refresh_chat_pokemon(cx);
            assert!(activity_snapshot(chat.read(cx)).1);
            s.set_pokemon_enabled(false, cx);
            s.refresh_chat_pokemon(cx);
            assert_eq!(activity_snapshot(chat.read(cx)), (vec![], false));
            assert_eq!(chat.read(cx).background, tasks(1));
            s.set_pokemon_enabled(true, cx);
            seed_portraits(s, &home.join("activity-art"));
            s.refresh_chat_pokemon(cx);
            assert_eq!(activity_snapshot(chat.read(cx)).0[0].1.0, 25);
            let mut imported = Companion::default();
            imported.collection = vec![caught(4)];
            s.apply_pokemon_save(imported, cx);
            seed_portraits(s, &home.join("activity-art"));
            s.refresh_chat_pokemon(cx);
            let (snapshot, hopping) = activity_snapshot(chat.read(cx));
            assert_eq!(snapshot, vec![("task-0".into(), (4, false, None), true)]);
            assert!(hopping);
            s.apply_pokemon_save(Companion::default(), cx);
            s.refresh_chat_pokemon(cx);
            assert_eq!(activity_snapshot(chat.read(cx)), (vec![], false));
        })
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn absent_portrait_schedules_loading_without_losing_background_tasks(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon.collection = vec![caught(25)];
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, window, cx| {
        shark.update(cx, |s, cx| {
            let chat = chat(s, 1, window, cx);
            s.pokemon_ui.thumbnails.clear();
            s.refresh_chat_pokemon(cx);
            assert!(s.pokemon_ui.thumbnail_pending.contains(&(25, false, None)));
            assert_eq!(
                activity_snapshot(chat.read(cx)),
                (vec![("task-0".into(), (25, false, None), false)], false)
            );
            assert_eq!(chat.read(cx).background, tasks(1));
        })
    })
    .unwrap();
}
