use super::*;
use crate::pokemon::{Balance, Companion, EvoLine, EvoNode, Item, Rarity};
use crate::usage::{Allowance, Provider};
use std::collections::BTreeMap;

fn line(id: u16, name: &str) -> EvoLine {
    EvoLine {
        base_id: id,
        tree: EvoNode {
            species_id: id,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(id, BTreeMap::from([("en".into(), name.into())]))]),
    }
}

fn companion() -> Companion {
    let mut p = Companion::default();
    p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    p.hatch(line(25, "Pikachu"));
    p.active.as_mut().unwrap().shiny = false;
    p
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_footer_rail_disable_and_usage_access(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = companion();
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            let now = crate::usage::now();
            for index in 0..s.usage_providers.len() {
                s.usage_reports[index] = crate::usage::logs::Report::from_entries(
                    vec![crate::usage::logs::Entry {
                        id: format!("usage-first-{index}"),
                        date: now,
                        model: "fixture".into(),
                        input: 1000,
                        ..Default::default()
                    }],
                    now,
                );
                s.usage_activity[index] = s.usage_reports[index].daily.clone();
            }
            for (index, snapshot) in s.usage.iter_mut().enumerate() {
                snapshot.finish(Ok(vec![Allowance {
                    label: "Weekly".into(),
                    remaining: Some(if index == 0 { 42. } else { 68. }),
                    resets_at: None,
                    window: Some(604800.),
                }]));
            }
            cx.notify();
        });
        w.render_frame(cx);
        assert!(w.try_find("usage-trigger-claude").is_none());
        assert!(w.try_find("usage-trigger-codex").is_none());
        assert!(w.try_find("show-usage").is_none());
        assert!(w.find("pokemon-footer").visible());
        w.click("pokemon-footer", cx);
        w.render_frame(cx);
        assert!(w.try_find("pokemon-tab-Home").is_none());
        assert!(w.try_find("pokemon-home").is_none());
        assert!(w.find("pokemon-usage-progress").visible());
        let progress = w.find("pokemon-usage-progress").bounds();
        assert!(w.find("usage-codex-0").bounds().bottom() <= progress.top());
        assert!(w.try_find("usage-trend").is_none());
        assert!(w.try_find("usage-accounting-claude").is_none());
        let before = shark.read(cx).usage_reports[0].today();
        w.click("usage-toggle-details", cx);
        w.render_frame(cx);
        assert!(
            w.find("pokemon-usage-progress").bounds().bottom()
                <= w.find("usage-trend").bounds().top()
        );
        assert!(w.try_find("usage-accounting-claude").is_some());
        w.click("usage-toggle-details", cx);
        w.render_frame(cx);
        assert!(w.try_find("usage-trend").is_none());
        assert_eq!(shark.read(cx).usage_reports[0].today(), before);
        for provider in ["claude", "codex"] {
            assert!(w.try_find(format!("usage-heatmap-{provider}")).is_none());
        }
        assert!(w.find("usage-claude-0").visible());
        assert!(w.find("usage-codex-0").visible());
        w.click("pokemon-usage-open-home", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-home").visible());
        assert_eq!(
            shark
                .read(cx)
                .state
                .pokemon
                .active
                .as_ref()
                .unwrap()
                .species_id(),
            25
        );
        w.press("escape", cx);
        shark.update(cx, |s, cx| {
            s.rail = true;
            cx.notify();
        });
        w.render_frame(cx);
        let footer = w.find("usage-footer").bounds();
        let pokemon = w.find("pokemon-footer").bounds();
        assert!(pokemon.left() >= footer.left() && pokemon.right() <= footer.right());
        assert!(w.try_find("usage-trigger-claude").is_none());
        assert!(w.try_find("usage-trigger-codex").is_none());
        assert!(w.try_find("show-usage").is_none());
        w.click("pokemon-footer", cx);
        w.render_frame(cx);
        assert!(w.find("usage-claude-0").visible());
        assert!(w.find("usage-codex-0").visible());
        w.press("escape", cx);
        shark.update(cx, |s, cx| s.set_pokemon_enabled(false, cx));
        w.render_frame(cx);
        assert!(w.try_find("pokemon-footer").is_none());
        assert!(w.find("show-usage").visible());
        assert!(w.try_find("usage-trigger-claude").is_none());
        assert!(w.try_find("usage-trigger-codex").is_none());
        w.click("show-usage", cx);
        w.render_frame(cx);
        assert!(w.try_find("usage-heatmap-claude").is_none());
        assert!(w.try_find("usage-heatmap-codex").is_none());
        assert!(w.find("usage-claude-0").visible());
        assert!(w.find("usage-codex-0").visible());
        w.press("escape", cx);
        assert_eq!(
            shark
                .read(cx)
                .state
                .pokemon
                .active
                .as_ref()
                .unwrap()
                .species_id(),
            25
        );
        assert!(State::load().pokemon_disabled);
        shark.update(cx, |s, cx| s.set_pokemon_enabled(true, cx));
        w.render_frame(cx);
        assert!(w.find("pokemon-footer").visible());
        assert!(!State::load().pokemon_disabled);
        assert!(w.try_find("show-usage").is_none());
        assert!(w.try_find("usage-trigger-claude").is_none());
        assert!(w.try_find("usage-trigger-codex").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_usage_baselines_toggle_and_restart(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, cx| {
            for total in [100, 150, 150] {
                s.usage_activity[0] = crate::usage::activity::Daily {
                    first_day: 20000,
                    tokens: vec![total],
                };
                s.observe_pokemon_usage(Provider::Claude, cx);
            }
            assert_eq!(s.state.pokemon.lifetime_tokens, 50);
            assert_eq!(State::load().pokemon.lifetime_tokens, 50);
            s.set_pokemon_enabled(false, cx);
            s.usage_activity[0].tokens = vec![300];
            s.observe_pokemon_usage(Provider::Claude, cx);
            assert_eq!(s.state.pokemon.lifetime_tokens, 50);
            s.set_pokemon_enabled(true, cx);
            s.usage_activity[0].tokens = vec![400];
            s.observe_pokemon_usage(Provider::Claude, cx);
            assert_eq!(
                s.state.pokemon.lifetime_tokens, 50,
                "first enabled scan excludes disabled interval"
            );
            s.usage_activity[0].tokens = vec![450];
            s.observe_pokemon_usage(Provider::Claude, cx);
            assert_eq!(s.state.pokemon.lifetime_tokens, 100);
            s.state.pokemon = State::load().pokemon;
            s.observe_pokemon_usage(Provider::Claude, cx);
            assert_eq!(
                s.state.pokemon.lifetime_tokens, 100,
                "restart cannot replay an observation"
            );
            s.usage_activity[0] = crate::usage::activity::Daily {
                first_day: 20001,
                tokens: vec![25],
            };
            s.observe_pokemon_usage(Provider::Claude, cx);
            assert_eq!(s.state.pokemon.lifetime_tokens, 125);
        });
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_limit_candy_uses_official_rows_once_per_epoch(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, _| {
            s.usage[0].finish(Ok(vec![Allowance {
                label: "Weekly".into(),
                remaining: Some(50.),
                resets_at: Some(200000.),
                window: Some(604800.),
            }]));
            s.observe_pokemon_limits(Provider::Claude);
            s.usage[0].allowances[0].remaining = Some(0.);
            s.observe_pokemon_limits(Provider::Claude);
            assert_eq!(s.state.pokemon.inventory.candies, 5);
            s.usage[0].allowances[0].remaining = Some(1.);
            s.observe_pokemon_limits(Provider::Claude);
            s.usage[0].allowances[0].remaining = Some(0.);
            s.observe_pokemon_limits(Provider::Claude);
            assert_eq!(
                s.state.pokemon.inventory.candies, 5,
                "same reset epoch cannot reward twice after corrections"
            );
            s.usage[0].allowances[0].resets_at = Some(804800.);
            s.observe_pokemon_limits(Provider::Claude);
            assert_eq!(s.state.pokemon.inventory.candies, 10);
        });
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_shop_bag_difficulty_and_candy_graduation_updates_footer(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    state.pokemon = companion();
    state.pokemon.lifetime_tokens = 2_000_000_000;
    state.pokemon.active.as_mut().unwrap().tokens = 700_000_000;
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.click("pokemon-usage-open-home", cx);
        w.render_frame(cx);
        let original = shark.read(cx).state.pokemon.growth_difficulty;
        w.click("pokemon-Growth-less", cx);
        w.click("pokemon-Shop prices-less", cx);
        assert_eq!(shark.read(cx).state.pokemon.shop_difficulty, 1.);
        assert_eq!(
            shark.read(cx).state.pokemon.growth_difficulty,
            original,
            "draft does not apply before Save"
        );
        w.render_frame(cx);
        w.click("pokemon-save-difficulty", cx);
        assert_eq!(shark.read(cx).state.pokemon.growth_difficulty, 0.9);
        assert_eq!(shark.read(cx).state.pokemon.shop_difficulty, 0.9);
        w.click("pokemon-tab-Shop", cx);
        w.render_frame(cx);
        w.click("pokemon-buy-Rare Candy", cx);
        assert_eq!(shark.read(cx).state.pokemon.inventory.candies, 1);
        assert_eq!(
            shark.read(cx).state.pokemon.spent_tokens,
            shark.read(cx).state.pokemon.price(Item::RareCandy)
        );
        w.click("pokemon-tab-Bag", cx);
        w.render_frame(cx);
        w.click("pokemon-use-candy", cx);
        assert!(shark.read(cx).state.pokemon.active.is_none());
        assert_eq!(shark.read(cx).state.pokemon.inventory.candies, 0);
        assert_eq!(shark.read(cx).state.pokemon.collection.len(), 1);
        w.press("escape", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-footer").visible());
        assert_eq!(shark.read(cx).state.pokemon.title(), "Token Egg");
        assert_eq!(State::load().pokemon.collection.len(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_dex_pages_filter_details_back_and_pin(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    for id in 1..=18 {
        let mut p = Companion::default();
        p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
        p.hatch(line(id, &format!("Species {id}")));
        let mut mon = p.active.take().unwrap();
        mon.graduated = true;
        mon.shiny = id == 18;
        state.pokemon.collection.push(mon);
    }
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.click("pokemon-footer", cx);
        w.render_frame(cx);
        w.click("pokemon-usage-open-home", cx);
        w.click("pokemon-tab-Pokédex", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-dex-1").visible());
        assert!(w.try_find("pokemon-dex-18").is_none());
        w.click("pokemon-page-next", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-dex-18").visible());
        w.click("pokemon-dex-18", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-details").visible());
        w.click("pokemon-pin-selected", cx);
        assert_eq!(
            shark.read(cx).state.pokemon.representative_subject(),
            Some((18, true))
        );
        w.click("pokemon-dex-back", cx);
        w.render_frame(cx);
        assert!(
            w.find("pokemon-dex-18").visible(),
            "Back keeps original page"
        );
        w.click("pokemon-filter-shiny", cx);
        w.render_frame(cx);
        assert!(w.find("pokemon-dex-18").visible());
        assert!(w.try_find("pokemon-dex-17").is_none());
        w.click("pokemon-follow", cx);
        assert!(
            shark
                .read(cx)
                .state
                .pokemon
                .representative_subject()
                .is_none()
        );
    })
    .unwrap();
}

/// Opt-in real Metal renders. Artwork comes from a caller-provided runtime cache, never bundled.
pub(super) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for (mode, label) in [
        (crate::state::Mode::Dark, "dark"),
        (crate::state::Mode::Light, "light"),
    ] {
        for rail in [false, true] {
            let mut state = State::default();
            state.appearance.mode = mode;
            state.pokemon = companion();
            state.pokemon.lifetime_tokens = 5_000_000_000;
            state.pokemon.inventory.candies = 3;
            for (id, name) in [(1, "Bulbasaur"), (4, "Charmander"), (7, "Squirtle")] {
                let mut p = Companion::default();
                p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
                p.hatch(line(id, name));
                let mut mon = p.active.take().unwrap();
                mon.graduated = true;
                state.pokemon.collection.push(mon);
            }
            state.save().unwrap();
            let (window, shark) = cx
                .update(|cx| {
                    gpui_kit::open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(0.), px(0.)),
                                size(px(1120.), px(840.)),
                            ))),
                            focus: false,
                            show: false,
                            ..Default::default()
                        },
                        cx,
                        |w, cx| cx.new(|cx| Shark::new(w, cx)),
                    )
                })
                .unwrap();
            for page in [
                "footer",
                "footer-disabled",
                "home",
                "dex",
                "shop",
                "usage",
                "settings",
            ] {
                cx.update_window(window, |_, w, cx| {
                    shark.update(cx, |s, cx| {
                        s.rail = rail;
                        s.state.pokemon_disabled = page == "footer-disabled";
                        for (index, snapshot) in s.usage.iter_mut().enumerate() {
                            snapshot.finish(Ok(vec![Allowance {
                                label: "5 hours".into(),
                                remaining: Some(if index == 0 { 42. } else { 68. }),
                                resets_at: Some(crate::usage::now() + 9000.),
                                window: Some(18000.),
                            }]));
                        }
                        if let Some(path) = std::env::var_os("SHARK_POKEMON_PREVIEW_SPRITES") {
                            s.seed_pokemon_preview_sprites(&PathBuf::from(path), cx);
                        }
                        if page == "footer" || page == "footer-disabled" {
                            s.close_usage(w, cx);
                        } else if page == "settings" {
                            s.close_usage(w, cx);
                            crate::settings::open(&s.state, w, cx);
                        } else {
                            s.show_pokemon(w, cx);
                            s.pokemon_ui.tab = match page {
                                "dex" => super::super::pokemon::Page::Dex,
                                "shop" => super::super::pokemon::Page::Shop,
                                "usage" => super::super::pokemon::Page::Usage,
                                _ => super::super::pokemon::Page::Home,
                            };
                        }
                        if page == "usage" {
                            for provider in crate::usage::Provider::ALL {
                                let i = provider.index();
                                s.usage_reports[i] = crate::usage::logs::Report::from_entries(
                                    vec![crate::usage::logs::Entry {
                                        id: format!("render-{}", provider.source().id()),
                                        date: crate::usage::now(),
                                        model: "fixture".into(),
                                        input: 100,
                                        cache_read: 200,
                                        output: 50,
                                        explicit_cost: Some(1.25),
                                        cost_is_estimate: Some(true),
                                        ..Default::default()
                                    }],
                                    crate::usage::now(),
                                );
                                s.usage_activity[i] = s.usage_reports[i].daily.clone();
                                // Activity excludes cache; companion accounting includes it.
                                if let Some(today) = s.usage_activity[i].tokens.last_mut() {
                                    *today = 150;
                                }
                                s.usage[i].finish(Ok(vec![crate::usage::Allowance {
                                    label: "5 hours".into(),
                                    remaining: Some(if i == 0 { 42. } else { 68. }),
                                    resets_at: Some(crate::usage::now() + 9000.),
                                    window: Some(18000.),
                                }]));
                            }
                        }
                        cx.notify();
                    });
                    w.render_frame(cx);
                    w.refresh();
                    w.render_frame(cx);
                    assert!(w.try_find("usage-trigger-claude").is_none());
                    assert!(w.try_find("usage-trigger-codex").is_none());
                    if page != "footer-disabled" {
                        let footer = w.find("pokemon-footer").bounds();
                        assert!(w.try_find("pokemon-footer-tokens").is_none());
                        let claude = w.find("pokemon-footer-quota-claude").bounds();
                        let codex = w.find("pokemon-footer-quota-codex").bounds();
                        assert_eq!(claude.top(), codex.top(), "both providers share one line");
                        assert!(codex.left() > claude.right());
                        assert!(footer.size.height <= px(if rail { 48. } else { 32. }));
                        for id in ["pokemon-footer-quota-claude", "pokemon-footer-quota-codex"] {
                            let element = w.find(id);
                            assert!(element.visible());
                            let bounds = element.bounds();
                            assert!(
                                bounds.left() >= footer.left() && bounds.right() <= footer.right(),
                                "{id}: {bounds:?} outside {footer:?}"
                            );
                        }
                    }
                    if page == "footer-disabled" {
                        assert!(w.find("show-usage").visible());
                        assert!(w.try_find("pokemon-footer").is_none());
                        assert!(w.try_find("usage-panel").is_none());
                    } else {
                        assert!(w.find("pokemon-footer").visible());
                        assert!(w.try_find("show-usage").is_none());
                        if page == "footer" {
                            assert!(w.try_find("usage-panel").is_none());
                        }
                    }
                })
                .unwrap();
                // Let cached GIF decoding settle, then render the production widget again.
                cx.advance_clock(Duration::from_millis(100));
                cx.update_window(window, |_, w, cx| {
                    w.render_frame(cx);
                })
                .unwrap();
                let screenshot = cx
                    .capture_screenshot(window)
                    .expect("Pokémon real renderer");
                if (page == "footer" || page == "dex")
                    && std::env::var_os("SHARK_POKEMON_PREVIEW_SPRITES").is_some()
                {
                    let bounds = cx
                        .update_window(window, |_, w, cx| {
                            w.activate_window();
                            w.render_frame(cx);
                            w.find(if page == "footer" {
                                "pokemon-footer"
                            } else {
                                "pokemon-dex-25"
                            })
                            .bounds()
                        })
                        .unwrap();
                    cx.run_until_parked();
                    cx.update_window(window, |_, w, cx| {
                        cx.set_reduce_motion(false);
                        w.render_frame(cx);
                        assert!(
                            w.is_window_active(),
                            "Activate callback must settle before checking GIF frames"
                        );
                    })
                    .unwrap();
                    let before = cx.capture_screenshot(window).unwrap();
                    let scale = before.width() as f32 / 1120.;
                    let left = (f32::from(bounds.left()) * scale).max(0.) as u32;
                    let top = (f32::from(bounds.top()) * scale).max(0.) as u32;
                    let right =
                        (f32::from(bounds.right()) * scale).min(before.width() as f32) as u32;
                    let bottom =
                        (f32::from(bounds.bottom()) * scale).min(before.height() as f32) as u32;
                    let mut animated = false;
                    for _ in 0..10 {
                        // GPUI's GIF decoder uses wall-clock frame delays, not the test executor clock.
                        std::thread::sleep(Duration::from_millis(80));
                        cx.advance_clock(Duration::from_millis(80));
                        cx.run_until_parked();
                        cx.update_window(window, |_, w, cx| w.render_frame(cx))
                            .unwrap();
                        let after = cx.capture_screenshot(window).unwrap();
                        if (left..right).any(|x| {
                            (top..bottom).any(|y| before.get_pixel(x, y) != after.get_pixel(x, y))
                        }) {
                            animated = true;
                            break;
                        }
                    }
                    assert!(
                        animated,
                        "Production {page} must advance cached GIF frames while active"
                    );
                }

                if let Some(directory) = std::env::var_os("SHARK_RENDER_DIR") {
                    let directory = PathBuf::from(directory);
                    fs::create_dir_all(&directory).unwrap();
                    screenshot
                        .save(directory.join(format!(
                            "pokemon-{page}-{label}-{}.png",
                            if rail { "rail" } else { "expanded" }
                        )))
                        .unwrap();
                }
                if page == "settings" {
                    cx.update_window(window, |_, w, cx| w.close_dialog(cx))
                        .unwrap();
                }
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
    super::super::pokemon::appearance_pixels::render_pixels(cx);
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn pokemon_refresh_keeps_footer_and_popover_anchor_stable(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    for rail in [false, true] {
        for (width, height) in [(640., 480.), (1120., 840.)] {
            let mut state = State::default();
            state.pokemon = companion();
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
                let allowances = || {
                    vec![Allowance {
                        label: "Weekly".into(),
                        remaining: Some(42.),
                        resets_at: Some(crate::usage::now() + 3600.),
                        window: Some(604800.),
                    }]
                };
                shark.update(cx, |s, cx| {
                    s.rail = rail;
                    for snapshot in &mut s.usage {
                        snapshot.finish(Ok(allowances()));
                    }
                    cx.notify();
                });
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                let footer = w.find("usage-footer").bounds();
                for remaining in [100., 42.] {
                    shark.update(cx, |s, cx| {
                        for snapshot in &mut s.usage {
                            snapshot.allowances[0].remaining = Some(remaining);
                        }
                        cx.notify();
                    });
                    w.render_frame(cx);
                    let claude = w.find("pokemon-footer-quota-claude").bounds();
                    let codex = w.find("pokemon-footer-quota-codex").bounds();
                    assert_eq!(claude.top(), codex.top());
                    assert!(codex.left() > claude.right());
                    assert!(
                        codex.right() <= footer.right(),
                        "quotas must fit the navigation"
                    );
                    assert!(footer.size.height <= px(if rail { 64. } else { 48. }));
                }
                w.click("pokemon-footer", cx);
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                assert_eq!(
                    w.find("usage-footer").bounds(),
                    footer,
                    "opening and refreshing must not resize the footer; rail={rail}"
                );
                let panel = w.find("usage-panel").bounds();
                let viewport = w.viewport_size();
                let assert_panel = |current: Bounds<Pixels>| {
                    assert_eq!(current.left(), panel.left());
                    assert_eq!(current.bottom(), panel.bottom());
                    assert_eq!(current.size.width, panel.size.width);
                    assert!(
                        current.top() >= px(8.)
                            && current.left() >= px(8.)
                            && current.right() <= viewport.width - px(8.)
                            && current.bottom() <= viewport.height - px(8.),
                        "dynamic Usage content must fit the viewport: {current:?}"
                    );
                };
                for provider in 0..2 {
                    shark.update(cx, |s, cx| {
                        s.usage[provider].finish(Ok(allowances()));
                        cx.notify();
                    });
                    w.render_frame(cx);
                    w.refresh();
                    w.render_frame(cx);
                    assert_eq!(w.find("usage-footer").bounds(), footer);
                    assert_panel(w.find("usage-panel").bounds());
                }
                assert!(w.find("refresh-usage").visible());
                w.click("refresh-usage", cx);
                w.render_frame(cx);
                w.refresh();
                w.render_frame(cx);
                assert_eq!(w.find("usage-footer").bounds(), footer);
                assert_panel(w.find("usage-panel").bounds());
                for result in [
                    Err(crate::usage::Failure::Temporary),
                    Err(crate::usage::Failure::SignIn),
                    Ok(allowances()),
                ] {
                    shark.update(cx, |s, cx| {
                        for snapshot in &mut s.usage {
                            snapshot.finish(result.clone());
                        }
                        cx.notify();
                    });
                    w.render_frame(cx);
                    w.refresh();
                    w.render_frame(cx);
                    assert_eq!(
                        w.find("usage-footer").bounds(),
                        footer,
                        "error and recovery states must preserve footer geometry"
                    );
                    assert_panel(w.find("usage-panel").bounds());
                }
                w.remove_window();
            })
            .unwrap();
        }
    }
}
