use super::*;

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_toggle_persists_without_deleting_progress(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        let original = serde_json::to_value(&state.pokemon).unwrap();
        assert!(!state.pokemon_disabled);
        let settings =
            cx.new(|cx| crate::settings::Settings::new(shark.downgrade(), &state, w, cx));
        settings.update(cx, |s, cx| s.set_pokemon(false, cx));
        assert!(shark.read(cx).state.pokemon_disabled);
        let saved = State::load();
        assert!(saved.pokemon_disabled);
        assert_eq!(serde_json::to_value(&saved.pokemon).unwrap(), original);
        settings.update(cx, |s, cx| s.set_pokemon(true, cx));
        assert!(!State::load().pokemon_disabled);
        assert_eq!(
            serde_json::to_value(&State::load().pokemon).unwrap(),
            original
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn settings_exposes_pokemon_switch_and_no_license_page(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        assert!(
            w.find("pokemon-enabled").visible(),
            "switch {:?}, viewport {:?}",
            w.find("pokemon-enabled").bounds(),
            w.viewport_size()
        );
        w.click("pokemon-enabled", cx);
        assert!(State::load().pokemon_disabled);
        assert!(w.try_find("LICENSE").is_none());
        assert!(w.try_find("pokemon-choose-navigation").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn about_dialog_holds_licenses_and_pokemon_attribution(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        crate::settings::about(w, cx);
        w.render_frame(cx);
        assert!(w.find("license-shark").visible());
        let initial = w.find("license-pokeapi").bounds().top();
        w.scroll("about", ScrollDelta::Pixels(point(px(0.), px(-1600.))), cx);
        w.render_frame(cx);
        assert!(w.find("license-pokeapi").visible());
        assert!(w.find("license-pokeapi").bounds().top() < initial);
        assert!(w.try_find("license-poketokenbar").is_some());
    })
    .unwrap();
}

#[test]
fn folder_entries_the_scan_drops_are_listed_as_ignored() {
    let home = std::path::Path::new("/Users/me");
    assert_eq!(
        crate::settings::ignored_roots("/a, rel,\n~/x, ./y, ,/, ~", home),
        ["rel", "./y", "/", "~"]
    );
}

#[test]
fn old_shark_state_enables_pokemon_and_creates_a_fresh_save() {
    let state: State = serde_json::from_str(r#"{"repos":[],"workspaces":[]}"#).unwrap();
    assert!(!state.pokemon_disabled);
    assert!(state.pokemon.active.is_none());
    assert!(state.pokemon.collection.is_empty());
    assert_eq!(state.pokemon.lifetime_tokens, 0);
    assert_eq!(state.pokemon.egg_tokens, 0);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_import_return_cancels_and_explicit_replace_keeps_settings(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon.lifetime_tokens = 999;
    state.pokemon_disabled = true;
    state.appearance.dark = "Sequoia".into();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        let view = cx.new(|cx| crate::settings::Settings::new(shark.downgrade(), &state, w, cx));
        let mut imported = crate::pokemon::Companion::default();
        imported.lifetime_tokens = 123;
        view.update(cx, |s, cx| {
            s.confirm_pokemon_restore(imported.clone(), w, cx)
        });
        w.render_frame(cx);
        w.press("enter", cx);
        assert_eq!(shark.read(cx).state.pokemon.lifetime_tokens, 999);
        view.update(cx, |s, cx| s.confirm_pokemon_restore(imported, w, cx));
        w.render_frame(cx);
        w.click("pokemon-import-replace", cx);
        assert_eq!(shark.read(cx).state.pokemon.lifetime_tokens, 123);
        assert_eq!(State::load().pokemon.lifetime_tokens, 123);
        assert!(shark.read(cx).state.pokemon_disabled);
        assert_eq!(shark.read(cx).state.appearance.dark, "Sequoia");
        assert!(
            State::pokemon_snapshots()
                .list()
                .iter()
                .any(|s| s.lifetime_tokens == 999)
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_source_folder_drafts_commit_only_the_selected_provider(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state
        .usage_roots
        .insert("codex".into(), "~/codex-existing".into());
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        state.save().unwrap();
        let settings =
            cx.new(|cx| crate::settings::Settings::new(shark.downgrade(), &state, w, cx));
        let claude = settings.read(cx).roots_input("claude");
        let codex = settings.read(cx).roots_input("codex");
        claude.update(cx, |input, cx| {
            input.set_value("~/claude-a, ~/claude-b", w, cx)
        });
        codex.update(cx, |input, cx| input.set_value("~/codex-draft", w, cx));
        assert!(!State::load().usage_roots.contains_key("claude"));
        assert_eq!(State::load().usage_roots["codex"], "~/codex-existing");
        settings.update(cx, |s, cx| s.save_roots(0, cx));
        assert_eq!(
            State::load().usage_roots["claude"],
            "~/claude-a, ~/claude-b"
        );
        assert_eq!(State::load().usage_roots["codex"], "~/codex-existing");
        settings.update(cx, |s, cx| s.save_roots(1, cx));
        assert_eq!(State::load().usage_roots["codex"], "~/codex-draft");
        claude.update(cx, |input, cx| input.set_value("  ", w, cx));
        settings.update(cx, |s, cx| s.save_roots(0, cx));
        assert!(!State::load().usage_roots.contains_key("claude"));
        assert_eq!(State::load().usage_roots["codex"], "~/codex-draft");
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        w.click("Token sources", cx);
        w.render_frame(cx);
        assert!(w.try_find("token-roots-save-claude").is_none());
        assert!(w.try_find("token-roots-save-codex").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_source_folders_are_saved_when_the_dialog_closes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let state = shark.read(cx).state.clone();
        let settings =
            cx.new(|cx| crate::settings::Settings::new(shark.downgrade(), &state, w, cx));
        let claude = settings.read(cx).roots_input("claude");
        let codex = settings.read(cx).roots_input("codex");
        codex.update(cx, |input, cx| input.set_value("/tmp/close-b", w, cx));
        assert!(!State::load().usage_roots.contains_key("codex"));
        // Blur saves too (see Settings::new); closing must not lose a field that never blurred.
        drop((settings, claude, codex));
    })
    .unwrap();
    // Release callbacks run when the update that dropped the entity ends.
    assert_eq!(State::load().usage_roots["codex"], "/tmp/close-b");
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_import_displays_current_species_without_waiting_for_usage(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let mut imported = crate::pokemon::Companion::default();
        imported.egg_tokens = crate::pokemon::Balance::EGG_HATCH_THRESHOLD;
        imported.hatch(crate::pokemon::EvoLine {
            base_id: 25,
            rarity: crate::pokemon::Rarity::Common,
            tree: crate::pokemon::EvoNode {
                species_id: 25,
                children: vec![],
            },
            names: std::collections::BTreeMap::from([(
                25,
                std::collections::BTreeMap::from([("en".into(), "Pikachu".into())]),
            )]),
        });
        shark.update(cx, |s, cx| s.apply_pokemon_save(imported, cx));
        assert!(shark.read(cx).state.pokemon.baselines.is_empty());
        assert_eq!(
            shark.read(cx).state.pokemon.active.as_ref().unwrap().name(),
            "Pikachu"
        );
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-usage-open-home", cx);
        w.render_frame(cx);
        w.click("pokemon-home-open-details", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-details").visible());
        w.press("escape", cx);
        shark.update(cx, |s, cx| {
            s.apply_pokemon_save(crate::pokemon::Companion::default(), cx)
        });
        assert!(shark.read(cx).state.pokemon.active.is_none());
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.render_frame(cx);
        assert!(w.try_find("pokemon-tab-Home").is_none());
        assert!(w.try_find("pokemon-home").is_none());
        assert!(w.try_find("pokemon-details").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_snapshot_history_collapses_and_reopens_with_all_ten_rows(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let snapshots = State::pokemon_snapshots();
        let _ = std::fs::remove_dir_all(&snapshots.directory);
        for index in 0..10 {
            let mut s = crate::pokemon::Companion::default();
            s.lifetime_tokens = index;
            snapshots.create(&s, 1_700_000_000 + index).unwrap();
        }
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        assert!(w.try_find(("pokemon-restore", 0usize)).is_none());
        assert!(w.find("pokemon-snapshot").visible());
        w.click("pokemon-snapshot-history", cx);
        w.render_frame(cx);
        for index in 0usize..10 {
            assert!(w.try_find(("pokemon-restore", index)).is_some());
        }
        w.scroll(
            "settings-page",
            ScrollDelta::Pixels(point(px(0.), px(-1600.))),
            cx,
        );
        w.render_frame(cx);
        assert!(w.find(("pokemon-restore", 9usize)).visible());
        w.scroll(
            "settings-page",
            ScrollDelta::Pixels(point(px(0.), px(1600.))),
            cx,
        );
        w.render_frame(cx);
        w.click("pokemon-snapshot-history", cx);
        w.render_frame(cx);
        assert!(w.try_find(("pokemon-restore", 0usize)).is_none());
        w.click("pokemon-snapshot-history", cx);
        w.close_dialog(cx);
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        assert!(w.try_find(("pokemon-restore", 0usize)).is_none());
        assert!(w.find("pokemon-snapshot").visible());
        assert_eq!(snapshots.list().len(), 10);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_empty_snapshot_history_explains_and_allows_creation(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let _ = std::fs::remove_dir_all(&State::pokemon_snapshots().directory);
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        assert!(w.try_find("pokemon-snapshots-empty").is_none());
        w.click("pokemon-snapshot-history", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-snapshots-empty").visible());
        assert!(w.find("pokemon-snapshot").visible());
        w.click("pokemon-snapshot", cx);
        w.render_frame(cx);
        assert!(w.try_find("pokemon-snapshots-empty").is_none());
        assert!(w.try_find(("pokemon-restore", 0usize)).is_some());
        assert_eq!(State::pokemon_snapshots().list().len(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn animation_quality_select_is_shown_and_choice_persists_for_restart(
    cx: &mut TestAppContext,
) {
    use crate::pokemon::animation::Quality;
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        assert!(w.find("pokemon-animation-quality").visible());
        for quality in [Quality::Balanced, Quality::Smooth, Quality::PowerSaver] {
            shark.update(cx, |s, cx| s.set_pokemon_animation_quality(quality, cx));
            assert_eq!(shark.read(cx).state.pokemon_animation_quality, quality);
            assert_eq!(State::load().pokemon_animation_quality, quality);
        }
    })
    .unwrap();
}
