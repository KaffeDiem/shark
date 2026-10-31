//! Independent native interaction regressions from the Sol UI QA pass.
use super::*;
use crate::pokemon::{Balance, Companion, EvoLine, EvoNode, Rarity};
use std::collections::BTreeMap;

fn hatched() -> Companion {
    let mut p = Companion::default();
    p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    p.hatch(EvoLine {
        base_id: 25,
        tree: EvoNode {
            species_id: 25,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(25, BTreeMap::from([("en".into(), "Pikachu".into())]))]),
    });
    p.active.as_mut().unwrap().shiny = false;
    p
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_keyboard_reaches_each_tab_and_restores_previous_focus(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            s.sidebar_focus.focus(w, cx);
            s.show_pokemon(w, cx);
        });
        w.render_frame(cx);
        for id in [
            "pokemon-refresh",
            "pokemon-tab-Home",
            "pokemon-tab-Pokédex",
            "pokemon-tab-Bag",
            "pokemon-tab-Shop",
            "pokemon-tab-Usage",
        ] {
            w.press("tab", cx);
            w.render_frame(cx);
            assert_eq!(w.find(id).focused(), Some(true), "Tab must reach {id}");
        }
        w.press("shift-tab", cx);
        w.render_frame(cx);
        assert_eq!(w.find("pokemon-tab-Shop").focused(), Some(true));
        w.press("enter", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-shop").visible());
        w.press("escape", cx);
        assert!(shark.read(cx).usage_panel.is_none());
        assert!(shark.read(cx).sidebar_focus.is_focused(w));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_large_individual_picker_stays_inside_panel(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = hatched();
    for _ in 0..30 {
        state
            .pokemon
            .collection
            .push(hatched().active.take().unwrap());
    }
    let (window, _) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Pokédex", cx);
        w.click("pokemon-dex-25", cx);
        w.render_frame(cx);
        let panel = w.find("usage-panel").bounds();
        for i in 0..31 {
            let b = w.find(format!("pokemon-individual-{i}")).bounds();
            assert!(
                b.left() >= panel.left() && b.right() <= panel.right(),
                "individual {i} overflows panel: {b:?} vs {panel:?}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_license_full_attribution_is_scrollable(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| crate::settings::open(&s.state, w, cx));
        w.render_frame(cx);
        w.click("LICENSE", cx);
        w.render_frame(cx);
        assert!(w.find("license-shark").visible());
        let initial = w.find("license-pokeapi").bounds().top();
        w.scroll(
            "settings-page",
            ScrollDelta::Pixels(point(px(0.), px(-1600.))),
            cx,
        );
        w.render_frame(cx);
        assert!(
            w.find("license-pokeapi").visible(),
            "last attribution link must be reachable"
        );
        assert!(
            w.find("license-pokeapi").bounds().top() < initial,
            "long license content must scroll"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_footer_and_popover_fit_small_windows_and_scroll(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for mode in [crate::state::Mode::Dark, crate::state::Mode::Light] {
        for rail in [false, true] {
            for (width, height) in [(640., 480.), (800., 600.)] {
                let mut state = State::default();
                state.appearance.mode = mode;
                state.pokemon = hatched();
                let (window, shark) = boot(
                    cx,
                    state,
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(width), px(height)),
                        ))),
                        ..Default::default()
                    },
                );
                cx.update_window(window, |_, w, cx| {
                    shark.update(cx, |s, cx| {
                        s.rail = rail;
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let footer = w.find("pokemon-footer").bounds();
                    assert!(footer.left() >= px(0.) && footer.bottom() <= w.viewport_size().height);
                    w.click("pokemon-footer", cx);
                    w.render_frame(cx);
                    let panel = w.find("usage-panel").bounds();
                    assert!(
                        panel.left() >= px(0.)
                            && panel.top() >= px(0.)
                            && panel.right() <= w.viewport_size().width
                            && panel.bottom() <= w.viewport_size().height,
                        "popover outside viewport {panel:?} vs {:?}",
                        w.viewport_size()
                    );
                    w.scroll(
                        "usage-panel",
                        ScrollDelta::Pixels(point(px(0.), px(-1200.))),
                        cx,
                    );
                    w.click("pokemon-Growth-less", cx);
                    let before = w.find("pokemon-save-difficulty").bounds().top();
                    w.scroll(
                        "usage-panel",
                        ScrollDelta::Pixels(point(px(0.), px(-1200.))),
                        cx,
                    );
                    w.render_frame(cx);
                    assert!(
                        w.find("pokemon-save-difficulty").visible(),
                        "Home's final control must be reachable in a short viewport"
                    );
                    assert!(w.find("pokemon-save-difficulty").bounds().top() <= before);
                    w.remove_window();
                })
                .unwrap();
            }
        }
    }
}

// upstream: PopoverNavigationTests.swift::testOpenDexEntryShowsDetailInCollection
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_home_opens_growing_species_when_navigation_is_pinned(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = hatched();
    let mut archived = hatched().active.take().unwrap();
    archived.line.base_id = 1;
    archived.line.tree.species_id = 1;
    archived.line.names =
        BTreeMap::from([(1, BTreeMap::from([("en".into(), "Bulbasaur".into())]))]);
    archived.path = vec![1];
    archived.graduated = true;
    state.pokemon.collection.push(archived);
    assert!(state.pokemon.pin(Some(1)));
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(
            w.find("pokemon-footer")
                .label()
                .unwrap()
                .starts_with("Bulbasaur,")
        );
        w.click("pokemon-footer", cx);
        w.render_frame(cx);
        assert_eq!(
            w.find("pokemon-home-open-details").label(),
            Some("Open Pikachu details")
        );
        w.click("pokemon-home-open-details", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-details").visible());
        w.click("pokemon-pin-selected", cx);
        assert_eq!(
            shark.read(cx).state.pokemon.representative_subject(),
            Some((25, false))
        );
        w.click("pokemon-dex-back", cx);
        assert!(w.find("pokemon-dex-25").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_usage_tab_keeps_companion_navigation_and_disable_hides_it(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-tab-Usage", cx);
        w.render_frame(cx);
        assert!(w.find("usage-status-claude").visible());
        assert!(w.find("usage-status-codex").visible());
        assert!(w.try_find("pokemon-home").is_none());
        w.click("pokemon-tab-Home", cx);
        assert!(w.find("pokemon-home").visible());
        w.click("pokemon-tab-Pokédex", cx);
        assert!(w.find("pokemon-dex").visible());
        w.click("pokemon-tab-Usage", cx);
        shark.update(cx, |s, cx| s.set_pokemon_enabled(false, cx));
        w.render_frame(cx);
        assert!(w.find("usage-status-claude").visible());
        assert!(w.try_find("pokemon-tab-Home").is_none());
        assert!(w.try_find("pokemon-footer").is_none());
        w.press("escape", cx);
        assert!(
            shark.read(cx).usage_panel.is_none(),
            "Escape closes usage after disabling the focused companion controls"
        );
        shark.update(cx, |s, cx| {
            s.set_pokemon_enabled(true, cx);
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        assert!(w.find("usage-status-claude").visible());
        assert!(
            w.try_find("pokemon-panel").is_none(),
            "standalone usage retains its direct account view"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_every_catalog_form_can_be_pinned_from_native_individual_picker(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for &species in crate::pokemon::forms::SPECIES {
        let forms = crate::pokemon::forms::entries(species);
        let mut state = State::default();
        for (i, entry) in forms.iter().enumerate() {
            let mut mon = hatched().active.take().unwrap();
            let base = crate::pokemon::forms::line_base(species);
            mon.line.base_id = base;
            mon.line.tree = EvoNode {
                species_id: base,
                children: if base == species {
                    vec![]
                } else {
                    vec![EvoNode {
                        species_id: species,
                        children: vec![],
                    }]
                },
            };
            mon.line.names = [base, species]
                .into_iter()
                .map(|id| (id, BTreeMap::from([("en".into(), format!("Species {id}"))])))
                .collect();
            mon.path = if base == species {
                vec![species]
            } else {
                vec![base, species]
            };
            mon.stage = mon.path.len() - 1;
            mon.form = Some(entry.form.clone());
            mon.shiny = i % 2 == 1;
            mon.caught_at = i as u64;
            state.pokemon.collection.push(mon);
        }
        let (window, shark) = boot(cx, state, WindowOptions::default());
        cx.update_window(window, |_, w, cx| {
            w.render_frame(cx);
            w.click("pokemon-footer", cx);
            w.click("pokemon-tab-Pokédex", cx);
            w.click(format!("pokemon-dex-{species}"), cx);
            for (index, entry) in forms.iter().rev().enumerate() {
                w.click(format!("pokemon-form-{}", entry.form.0), cx);
                w.click("pokemon-pin-selected", cx);
                let selected = &shark.read(cx).state.pokemon;
                let original = forms.len() - 1 - index;
                assert_eq!(
                    selected.representative_subject(),
                    Some((species, original % 2 == 1)),
                    "shiny identity for species {species}, form {:?}",
                    entry.form
                );
                assert_eq!(
                    selected.representative_sprite_form(),
                    Some(entry.form.clone()),
                    "form identity for species {species}"
                );
                assert_eq!(
                    State::load().pokemon.representative_sprite_form(),
                    Some(entry.form.clone()),
                    "pin persists after restart"
                );
            }
            w.remove_window();
        })
        .unwrap();
    }
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_usage_accounting_scrolls_without_horizontal_clipping(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(
        cx,
        State::default(),
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(800.), px(600.)),
            ))),
            ..Default::default()
        },
    );
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            for provider in crate::usage::Provider::ALL {
                s.usage_reports[provider.index()] = crate::usage::logs::Report::from_entries(
                    vec![crate::usage::logs::Entry {
                        id: "fixture".into(),
                        date: crate::usage::now(),
                        model: "fixture".into(),
                        input: 100,
                        cache_read: 200,
                        explicit_cost: Some(1.25),
                        cost_is_estimate: Some(true),
                        ..Default::default()
                    }],
                    crate::usage::now(),
                );
            }
            s.show_pokemon(w, cx);
        });
        w.render_frame(cx);
        w.click("pokemon-tab-Usage", cx);
        w.render_frame(cx);
        let panel = w.find("usage-panel").bounds();
        for provider in crate::usage::Provider::ALL {
            let accounting = w
                .find(format!("usage-accounting-{}", provider.source().id()))
                .bounds();
            assert!(
                accounting.left() >= panel.left() && accounting.right() <= panel.right(),
                "inclusive accounting must fit panel"
            );
        }
        w.scroll(
            "usage-panel",
            ScrollDelta::Pixels(point(px(0.), px(-2000.))),
            cx,
        );
        w.render_frame(cx);
        assert!(
            w.find("usage-accounting-codex").visible(),
            "second provider's cache/cost breakdown must be reachable"
        );
    })
    .unwrap();
}
