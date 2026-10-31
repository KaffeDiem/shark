use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
#[test]
fn boundaries_and_affordances_match_content_and_position() {
    assert!(!needs_scroll(6, 40., false, 332.));
    assert!(needs_scroll(7, 40., false, 332.));
    assert!(needs_scroll(9, 40., false, 332.));
    assert!(!needs_scroll(4, 56., true, 316.));
    assert!(needs_scroll(5, 56., true, 316.));
    assert!(!needs_scroll(99, 40., false, f32::INFINITY));
    assert_eq!(row_width(3, 40., false), 148.);
    assert_eq!(row_width(9, 40., false), 472.);
    assert_eq!(affordance(0., 472., 332.), (false, true));
    assert_eq!(affordance(70., 472., 332.), (true, true));
    assert_eq!(affordance(140., 472., 332.), (true, false));
    assert_eq!(affordance(0., 148., 332.), (false, false));
    assert_eq!(page_target(true, 0., 9, 40., false, 332.), 6);
    assert!(page_target(true, 54., 9, 40., false, 332.) > 6);
    assert_eq!(page_target(true, 300., 9, 40., false, 332.), 8);
    assert_eq!(page_target(false, 0., 9, 40., false, 332.), 0);
    assert_eq!(page_target(false, 140., 9, 40., false, 332.), 0);
}
#[test]
fn only_owned_species_link_to_dex() {
    let owned = BTreeSet::from([1, 2]);
    for (content, target) in [
        (LineContent::Species(2), Some(2)),
        (LineContent::Species(3), None),
        (LineContent::Mystery, None),
    ] {
        assert_eq!(
            link_target(
                &LineItem {
                    content,
                    state: LineState::Future
                },
                &owned
            ),
            target
        );
    }
}
struct Preview {
    shark: Entity<Shark>,
    mon: Mon,
    nodes: Vec<LineItem>,
    thumb: f32,
    names: bool,
    max: f32,
}
impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shark.update(cx, |s, cx| {
            s.render_evolution_nodes(
                &self.mon,
                &self.nodes,
                self.thumb,
                self.names,
                self.max,
                true,
                cx,
            )
        })
    }
}
fn mon() -> Mon {
    let mut c = Companion::default();
    c.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    c.hatch(crate::pokemon::EvoLine {
        base_id: 1,
        rarity: Rarity::Common,
        tree: crate::pokemon::EvoNode {
            species_id: 1,
            children: vec![],
        },
        names: BTreeMap::from([(1, BTreeMap::from([("en".into(), "W".repeat(20))]))]),
    });
    c.active.unwrap()
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_evolution_rows_match_measured_width_and_scroll_limits(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        for (count, thumb, names, max) in (1..=9)
            .map(|count| (count, 40., false, f32::INFINITY))
            .chain([
                (9, 40., false, 332.),
                (5, 56., true, 316.),
                (3, 40., false, 332.),
                (2, 40., true, f32::INFINITY),
            ])
        {
            shark.update(cx, |s, _| {
                s.pokemon_ui
                    .evolution_home_scroll
                    .set_offset(point(px(0.), px(0.)))
            });
            let mon = mon();
            let nodes = (1..=count)
                .map(|id| LineItem {
                    content: if id == count {
                        LineContent::Mystery
                    } else {
                        LineContent::Species(id as u16)
                    },
                    state: LineState::Future,
                })
                .collect();
            let preview = cx.new(|_| Preview {
                shark: shark.clone(),
                mon,
                nodes,
                thumb,
                names,
                max,
            });
            w.open_dialog(cx, move |d, _, _| {
                crate::ui::dialog(d).w(px(620.)).child(preview.clone())
            });
            w.render_frame(cx);
            let natural = row_width(count, thumb, names);
            let row = w.find("pokemon-home-evolution-row").bounds();
            let viewport = w.find("pokemon-home-evolution").bounds();
            assert!(
                (f32::from(row.size.width) - natural).abs() < 0.5,
                "count{count} names{names} row{row:?}"
            );
            assert!(
                (f32::from(viewport.size.width) - natural.min(max)).abs() < 0.5,
                "viewport{viewport:?}"
            );
            if count == 9 && max.is_infinite() {
                assert!(viewport.size.width > px(332.));
            }
            if natural > max {
                assert!(viewport.size.width <= px(max));
                let before = row.left();
                w.click("pokemon-home-evolution-next", cx);
                w.render_frame(cx);
                assert!(
                    w.find("pokemon-home-evolution-row").bounds().left() < before,
                    "count={count} names={names} before={before:?} after={:?} offset={:?}",
                    w.find("pokemon-home-evolution-row").bounds(),
                    shark.read(cx).pokemon_ui.evolution_home_scroll.offset()
                );
            }
            w.close_dialog(cx);
        }
    })
    .unwrap();
}
