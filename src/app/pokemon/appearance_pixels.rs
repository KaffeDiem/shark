//! Actual Metal pixels, with deterministic GIF fixtures generated in a temporary cache.
use super::*;
use crate::app::ui_tests::test_home;
use crate::pokemon::{Balance, EvoLine, EvoNode, Rarity};
use gpui_kit::test::TestWindowExt;
fn individual(shiny: bool, caught_at: u64) -> Mon {
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
    let mut mon = p.active.unwrap();
    mon.shiny = shiny;
    mon.caught_at = caught_at;
    mon.graduated = true;
    mon
}
fn fixture(directory: &Path, shiny: bool) {
    let mut bytes = vec![];
    image::codecs::gif::GifEncoder::new(&mut bytes)
        .encode_frame(image::Frame::from_parts(
            image::RgbaImage::from_pixel(
                24,
                40,
                image::Rgba(if shiny {
                    [0, 0, 255, 255]
                } else {
                    [255, 0, 0, 255]
                }),
            ),
            0,
            0,
            image::Delay::from_numer_denom_ms(100, 1),
        ))
        .unwrap();
    crate::pokemon::sprites::load_with(
        directory,
        crate::pokemon::sprites::Request::pokemon(25, true, shiny, None),
        |_| Ok(bytes.clone()),
    )
    .unwrap();
}
fn count_color(image: &image::RgbaImage, bounds: Bounds<Pixels>, shiny: bool) -> usize {
    let scale = image.width() as f32 / 1120.;
    let x0 = (f32::from(bounds.left()) * scale).max(0.) as u32;
    let x1 = (f32::from(bounds.right()) * scale).min(image.width() as f32) as u32;
    let y0 = (f32::from(bounds.top()) * scale).max(0.) as u32;
    let y1 = (f32::from(bounds.bottom()) * scale).min(image.height() as f32) as u32;
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = image.get_pixel(x, y);
            let (primary, other) = if shiny { (p[2], p[0]) } else { (p[0], p[2]) };
            primary > 220 && other < 35 && p[1] < 35 && p[3] > 220
        })
        .count()
}
fn settle(cx: &mut gpui_kit::HeadlessAppContext, window: AnyWindowHandle) {
    cx.advance_clock(Duration::from_millis(50));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        w.refresh();
        w.render_frame(cx);
    })
    .unwrap();
}
fn assert_color(
    cx: &mut gpui_kit::HeadlessAppContext,
    window: AnyWindowHandle,
    id: &str,
    shiny: bool,
) {
    settle(cx, window);
    let bounds = cx
        .update_window(window, |_, w, _| w.find(id.to_string()).bounds())
        .unwrap();
    let image = cx.capture_screenshot(window).unwrap();
    let expected = count_color(&image, bounds, shiny);
    let wrong = count_color(&image, bounds, !shiny);
    assert!(
        expected > 30,
        "{id}: expected {} fixture pixels, got {expected}",
        if shiny { "shiny blue" } else { "normal red" }
    );
    assert_eq!(
        wrong, 0,
        "{id}: opposite appearance must never remain mounted"
    );
}
fn save(cx: &mut gpui_kit::HeadlessAppContext, window: AnyWindowHandle, name: &str) {
    if let Some(directory) = std::env::var_os("SHARK_RENDER_DIR") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        cx.capture_screenshot(window)
            .unwrap()
            .save(directory.join(format!("pokemon-appearance-{name}.png")))
            .unwrap();
    }
}
pub(crate) fn render_pixels(cx: &mut gpui_kit::HeadlessAppContext) {
    let home = test_home();
    let directory = home.join("appearance-render-sprites");
    fixture(&directory, false);
    fixture(&directory, true);
    for (mode, label) in [
        (crate::state::Mode::Light, "light"),
        (crate::state::Mode::Dark, "dark"),
    ] {
        for shiny_first in [false, true] {
            let order = if shiny_first {
                "shiny-first"
            } else {
                "normal-first"
            };
            let mut state = State::default();
            state.appearance.mode = mode;
            state.pokemon.collection =
                vec![individual(shiny_first, 1), individual(!shiny_first, 2)];
            for (index, species) in [
                13, 14, 114, 129, 130, 131, 140, 141, 165, 166, 169, 177, 178, 265, 266, 267, 287,
                15,
            ]
            .into_iter()
            .enumerate()
            {
                let mut caught = individual(false, 0);
                caught.line = EvoLine {
                    base_id: species,
                    tree: EvoNode {
                        species_id: species,
                        children: vec![],
                    },
                    rarity: [
                        Rarity::Common,
                        Rarity::Uncommon,
                        Rarity::Rare,
                        Rarity::Legendary,
                    ][index % 4],
                    names: BTreeMap::from([(
                        species,
                        BTreeMap::from([("en".into(), "Crabominable".into())]),
                    )]),
                };
                caught.path = vec![species];
                state.pokemon.collection.push(caught);
            }
            assert!(state.pokemon.pin_appearance(25, false, None));
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
            cx.update_window(window, |_, w, cx| {
                shark.update(cx, |s, cx| s.seed_pokemon_preview_sprites(&directory, cx));
                w.render_frame(cx);
                w.click("pokemon-footer", cx);
                w.click("pokemon-tab-Pokédex", cx);
                w.render_frame(cx);
            })
            .unwrap();
            cx.update_window(window, |_, w, _| {
                let cell = w.find("pokemon-dex-13").bounds();
                let name = w.find("pokemon-dex-name-13");
                assert!(
                    name.bounds().left() >= cell.left() && name.bounds().right() <= cell.right()
                );
                assert_eq!(name.label(), Some("Crabominable"));
            })
            .unwrap();
            assert_color(cx, window, "pokemon-dex-25", true);
            save(cx, window, &format!("grid-{label}-{order}"));
            cx.update_window(window, |_, w, cx| {
                w.scroll("usage-panel", ScrollDelta::Lines(point(0., -30.)), cx);
                w.click("pokemon-page-next", cx);
                assert!(w.find("pokemon-dex-287").visible());
                w.click("pokemon-page-previous", cx);
                assert!(w.find("pokemon-dex-13").visible());
                w.scroll("usage-panel", ScrollDelta::Lines(point(0., 30.)), cx);
                w.click("pokemon-dex-25", cx);
                w.render_frame(cx);
            })
            .unwrap();
            assert_color(cx, window, "pokemon-detail-evolution", false);
            save(cx, window, &format!("detail-normal-{label}-{order}"));
            cx.update_window(window, |_, w, cx| {
                w.click("pokemon-color-shiny", cx);
                w.render_frame(cx);
            })
            .unwrap();
            assert_color(cx, window, "pokemon-detail-evolution", true);
            save(cx, window, &format!("detail-shiny-{label}-{order}"));
            cx.update_window(window, |_, w, cx| {
                w.click("pokemon-color-normal", cx);
                w.render_frame(cx);
            })
            .unwrap();
            assert_color(cx, window, "pokemon-detail-evolution", false);
            cx.update_window(window, |_, w, cx| {
                w.click("pokemon-color-shiny", cx);
                w.scroll("usage-panel", ScrollDelta::Lines(point(0., -20.)), cx);
                w.render_frame(cx);
                w.click("pokemon-pin-selected", cx);
                assert_eq!(
                    shark.read(cx).state.pokemon.representative_subject(),
                    Some((25, true))
                );
                assert_eq!(shark.read(cx).pokemon_ui.sprite_key, Some((25, true, None)));
                w.press("escape", cx);
                w.render_frame(cx);
            })
            .unwrap();
            assert_color(cx, window, "pokemon-footer", true);
            for entry in 0..2 {
                cx.update_window(window, |_, w, cx| {
                    w.click("pokemon-footer", cx);
                    w.click("pokemon-tab-Pokédex", cx);
                    w.click("pokemon-segment-log", cx);
                    w.render_frame(cx);
                })
                .unwrap();
                for index in 0..2 {
                    let shiny = cx
                        .update_window(window, |_, _, cx| {
                            shark.read(cx).state.pokemon.catch_log()[index].displays_shiny()
                        })
                        .unwrap();
                    assert_color(cx, window, &format!("pokemon-catch-{index}"), shiny);
                }
                save(cx, window, &format!("log-{label}-{order}-reentry-{entry}"));
                cx.update_window(window, |_, w, cx| w.press("escape", cx))
                    .unwrap();
            }
            cx.update_window(window, |_, w, _| w.remove_window())
                .unwrap();
        }
    }
}
