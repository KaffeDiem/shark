//! Source-case migrations that exercise the production companion navigation.
use super::*;
use crate::pokemon::{Balance, Companion, EvoLine, EvoNode, Rarity};
use gpui_kit::test::TestEventExt;
use std::collections::BTreeMap;
fn mon(id: u16, name: &str) -> crate::pokemon::Mon {
    let mut p = Companion::default();
    p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    p.hatch(EvoLine {
        base_id: id,
        tree: EvoNode {
            species_id: id,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(id, BTreeMap::from([("en".into(), name.into())]))]),
    });
    let mut mon = p.active.unwrap();
    mon.shiny = false;
    mon
}
fn collection(count: u16) -> State {
    let mut state = State::default();
    for id in 1..=count {
        let mut m = mon(id, &format!("Species {id}"));
        m.graduated = true;
        m.caught_at = id as u64;
        state.pokemon.collection.push(m);
    }
    state
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: DexPageButtonHitAreaTests.swift::testClickOutsideGlyphButInsideHitAreaPages
// upstream: DexPageButtonHitAreaTests.swift::testHitAreaMeetsPointerTargetMinimum
async fn dex_pager_corners_and_wheel_are_real_targets(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, collection(40), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        let next = w.find("pokemon-page-next").bounds();
        assert!(next.size.height >= px(24.) && next.size.width >= px(24.));
        w.click_at("pokemon-page-next", point(px(3.), px(3.)), cx);
        assert!(w.find("pokemon-dex-17").visible());
        w.click_at("pokemon-page-previous", point(px(3.), px(3.)), cx);
        assert!(w.find("pokemon-dex-1").visible());
        w.scroll("pokemon-dex-grid", ScrollDelta::Lines(point(0., -1.)), cx);
        assert!(w.find("pokemon-dex-17").visible());
        w.scroll("pokemon-dex-grid", ScrollDelta::Lines(point(0., 1.)), cx);
        assert!(w.find("pokemon-dex-1").visible());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: DexBackNavigationTests.swift::testBackFromDetailReturnsToThePageOfThatSpecies
// upstream: DexBackNavigationTests.swift::testReturnPageIsUsedOnlyOnce
// upstream: DexBackNavigationTests.swift::testBackToTheLogLeavesTheGridOnItsFirstPage
async fn home_detail_back_finds_species_page_and_segment_resets_it(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = collection(20);
    state.pokemon.active = Some(mon(18, "Species 18"));
    let (window, _) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-home-open-details", cx);
        w.click("pokemon-dex-back", cx);
        assert!(w.find("pokemon-dex-20").visible());
        assert!(w.try_find("pokemon-dex-1").is_none());
        w.click("pokemon-segment-log", cx);
        w.click("pokemon-segment-dex", cx);
        assert!(w.find("pokemon-dex-1").visible());
        assert!(w.try_find("pokemon-dex-20").is_none());
        w.click("pokemon-segment-log", cx);
        w.click("pokemon-catch-0", cx);
        w.click("pokemon-dex-back", cx);
        w.click("pokemon-segment-dex", cx);
        assert!(w.find("pokemon-dex-1").visible());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: DexBackNavigationTests.swift::testBackFromDetailReturnsTheLogToWhereItWas
async fn catch_log_back_retains_scroll_until_segment_changes(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, collection(20), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        w.click("pokemon-segment-log", cx);
        assert!(w.find("pokemon-catch-0").visible());
        w.scroll(
            "pokemon-catch-log",
            ScrollDelta::Pixels(point(px(0.), px(-2000.))),
            cx,
        );
        assert!(w.find("pokemon-catch-19").visible());
        w.click("pokemon-catch-19", cx);
        w.click("pokemon-dex-back", cx);
        assert!(w.find("pokemon-catch-19").visible());
        w.click("pokemon-segment-dex", cx);
        w.click("pokemon-segment-log", cx);
        assert!(w.find("pokemon-catch-0").visible());
        assert!(w.try_find("pokemon-catch-19").is_none());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn dex_native_search_filters_clear_and_sort_controls(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = collection(3);
    state.pokemon.collection[0]
        .line
        .names
        .get_mut(&1)
        .unwrap()
        .insert("fr".into(), "Bulbizarre".into());
    state.pokemon.collection[0].shiny = true;
    let (window, _) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        w.click("pokemon-dex-search", cx);
        w.input("Bulbizarre", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-dex-1").visible());
        assert!(w.try_find("pokemon-dex-2").is_none());
        w.click("pokemon-clear-filters", cx);
        assert!(w.find("pokemon-dex-2").visible());
        w.click("pokemon-filter-shiny", cx);
        assert!(w.find("pokemon-dex-1").visible());
        assert!(w.try_find("pokemon-dex-2").is_none());
        w.click("pokemon-clear-filters", cx);
        for label in ["Number ↓", "Name A–Z", "Name Z–A", "Rarity ↓", "Number ↑"] {
            w.click("pokemon-sort", cx);
            assert_eq!(w.find("pokemon-sort").label(), Some(label));
        }
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn precious_companion_egg_requires_explicit_review(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    let mut m = mon(25, "Pikachu");
    m.shiny = true;
    state.pokemon.active = Some(m);
    state.pokemon.lifetime_tokens = 5_000_000_000;
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Shop", cx);
        w.click("pokemon-buy-Pokémon Egg", cx);
        assert!(w.find("pokemon-egg-confirm").visible());
        w.press("enter", cx);
        assert!(shark.read(cx).state.pokemon.active.is_some());
        w.click("pokemon-buy-Pokémon Egg", cx);
        w.click("pokemon-egg-confirm", cx);
        assert!(shark.read(cx).state.pokemon.active.is_none());
        assert_eq!(shark.read(cx).state.pokemon.collection.len(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: DifficultySaveTests.swift::testDraftDoesNotApplyUntilSaveAndSavesBothControls
// upstream: DifficultySaveTests.swift::testSaveButtonAppearsBelowEditedSlidersInEveryLanguage
async fn difficulty_draft_is_discarded_on_close_and_save_appears_below_controls(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        assert!(w.try_find("pokemon-save-difficulty").is_none());
        w.click("pokemon-Growth-less", cx);
        w.click("pokemon-Shop prices-less", cx);
        assert_eq!(shark.read(cx).state.pokemon.growth_difficulty, 1.);
        assert_eq!(shark.read(cx).state.pokemon.shop_difficulty, 1.);
        let save = w.find("pokemon-save-difficulty").bounds();
        assert!(save.top() >= w.find("pokemon-Shop prices-more").bounds().bottom());
        w.press("escape", cx);
        w.click("pokemon-footer", cx);
        assert!(w.try_find("pokemon-save-difficulty").is_none());
        assert_eq!(State::load().pokemon.growth_difficulty, 1.);
        w.click("pokemon-Growth-less", cx);
        w.click("pokemon-Shop prices-less", cx);
        w.click("pokemon-save-difficulty", cx);
        assert_eq!(State::load().pokemon.growth_difficulty, 0.9);
        assert_eq!(State::load().pokemon.shop_difficulty, 0.9);
        assert!(w.try_find("pokemon-save-difficulty").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: OutsideClickMonitorTests.swift::testStartTwiceKeepsFirstTokenAndDoesNotRegisterAgain
// upstream: OutsideClickMonitorTests.swift::testStopWhenIdleDoesNotRemove
// upstream: OutsideClickMonitorTests.swift::testStartAfterStopRegistersAgain
// upstream: OutsideClickMonitorTests.swift::testNilRegisterDoesNotOccupyTheSlot
async fn window_owned_popover_coalesces_open_and_releases_on_close(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.sidebar_focus.focus(w, cx);
            s.close_usage(w, cx);
            assert!(s.sidebar_focus.is_focused(w));
            s.show_pokemon(w, cx);
        });
        let original = w.focused(cx);
        shark.update(cx, |s, cx| s.show_pokemon(w, cx));
        assert_eq!(
            w.focused(cx),
            original,
            "second open keeps the live focus scope/subscription"
        );
        w.render_frame(cx);
        w.pointer_down(
            point(w.viewport_size().width - px(8.), px(200.)),
            MouseButton::Left,
            cx,
        );
        w.pointer_up(
            point(w.viewport_size().width - px(8.), px(200.)),
            MouseButton::Left,
            cx,
        );
        assert!(shark.read(cx).usage_panel.is_none());
        shark.update(cx, |s, cx| {
            s.close_usage(w, cx);
            s.show_pokemon(w, cx);
        });
        assert_ne!(
            w.focused(cx),
            original,
            "reopen owns a fresh scope after teardown"
        );
        w.press("escape", cx);
        assert!(shark.read(cx).usage_panel.is_none());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: PopoverNavigationTests.swift::testResetReturnsToHomeFromSettings
// upstream: PopoverNavigationTests.swift::testOpenRepresentativeDexLeavesSettingsForCollection
// upstream: PopoverNavigationTests.swift::testDexDetailDroppedWhenCollectionContentLeavesScreen
// upstream: PopoverNavigationTests.swift::testDexDetailKeptWhileCollectionContentStaysOnScreen
async fn selected_collection_retap_keeps_detail_and_settings_reentry_starts_home(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, collection(3), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        w.click("pokemon-dex-2", cx);
        assert_eq!(w.find("pokemon-tab-Pokédex").checked(), Some(true));
        w.click("pokemon-tab-Pokédex", cx);
        assert!(w.find("pokemon-details").visible());
        w.click("pokemon-tab-Home", cx);
        assert!(w.try_find("pokemon-details").is_none());
        w.click("pokemon-tab-Pokédex", cx);
        w.click("pokemon-dex-1", cx);
        w.press("cmd-,", cx);
        assert!(shark.read(cx).usage_panel.is_none());
        w.press("escape", cx);
        w.click("pokemon-footer", cx);
        assert!(w.find("pokemon-home").visible());
        w.press("escape", cx);
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        w.click("pokemon-choose-navigation", cx);
        assert!(w.find("pokemon-dex").visible());
        assert!(w.try_find("pokemon-home").is_none());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: DexEntryLinkCursorTests.swift::testHoverShowsHandAndLeavingRestoresArrow
// upstream: DexEntryLinkCursorTests.swift::testDisappearingWhileHoveredRestoresArrow
// upstream: DexEntryLinkCursorTests.swift::testDisappearingUnhoveredLeavesOtherViewsCursorAlone
async fn frame_owned_links_leave_no_stale_interactive_hitbox_when_detail_replaces_grid(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, collection(2), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        let cell = w.find("pokemon-dex-1");
        assert_eq!(cell.role(), Some(Role::Button));
        let point = cell.bounds().center();
        w.pointer_move(point, None, cx);
        w.click("pokemon-dex-1", cx);
        assert!(w.try_find("pokemon-dex-1").is_none());
        w.pointer_move(point, None, cx);
        assert!(w.find("pokemon-dex-back").visible());
        w.click("pokemon-tab-Home", cx);
        assert!(w.try_find("pokemon-dex-back").is_none());
        w.press("escape", cx);
        w.pointer_move(point, None, cx);
        assert!(w.find("home-row").visible());
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: RepresentativeFooterLayoutTests.swift::testRepresentativeActionStaysIconWidthInEveryLocaleAndState
async fn representative_pin_action_stays_icon_width_before_and_after_selection(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, _) = boot(cx, collection(1), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        w.click("pokemon-dex-1", cx);
        for _ in 0..2 {
            let action = w.find("pokemon-pin-selected");
            assert!(action.bounds().size.width <= px(40.));
            assert_eq!(
                action.label(),
                Some(
                    crate::pokemon::format::Language::En
                        .interface_text(crate::pokemon::interface_labels::Label::RepresentativeSet)
                )
            );
            w.click("pokemon-pin-selected", cx);
        }
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
// upstream: IndividualPickerRenderingTests.swift::testMountedSingleAndMultipleIndividuals
async fn individual_selector_is_mounted_only_for_multiple_matching_individuals(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for count in [1, 2] {
        let mut state = State::default();
        for i in 0..count {
            let mut caught = mon(25, "Pikachu");
            caught.caught_at = i;
            state.pokemon.collection.push(caught);
        }
        let (window, _) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("pokemon-footer", cx);
            w.click("pokemon-tab-Pokédex", cx);
            w.click("pokemon-dex-25", cx);
            assert_eq!(
                w.try_find("pokemon-individual-picker").is_some(),
                count == 2
            );
            if count == 2 {
                assert!(w.find("pokemon-individual-0").visible());
                assert!(w.find("pokemon-individual-1").visible());
            }
            w.remove_window();
        })
        .unwrap();
    }
}
