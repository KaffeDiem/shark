use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::pokemon::{Balance, Companion, EvoLine, EvoNode, Rarity};
use crate::usage::summary::UsageLedger;
use chrono::{Local, TimeZone};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
use std::collections::BTreeMap;

fn date(text: &str) -> chrono::NaiveDate {
    text.parse().unwrap()
}
fn now() -> f64 {
    Local
        .from_local_datetime(&date("2026-09-18").and_hms_opt(12, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp() as f64
}
fn ledger() -> UsageLedger {
    UsageLedger {
        covered_since: Some("2026-08-01".into()),
        tokens_by_day: BTreeMap::from([
            ("2026-09-17".into(), 12345),
            ("2026-09-15".into(), 900),
            ("2026-08-20".into(), 900),
        ]),
    }
}
fn mon(species: u16, name: &str, form: Option<&str>, id: &str) -> Mon {
    let mut p = Companion::default();
    p.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    let base = if species == 413 { 412 } else { species };
    p.hatch(EvoLine {
        base_id: base,
        tree: EvoNode {
            species_id: base,
            children: if species == 413 {
                vec![EvoNode {
                    species_id: 413,
                    children: vec![],
                }]
            } else {
                vec![]
            },
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(species, BTreeMap::from([("en".into(), name.into())]))]),
    });
    let mut mon = p.active.take().unwrap();
    if species == 413 {
        mon.path = vec![412, 413];
        mon.stage = 1;
    }
    mon.shiny = false;
    mon.ditto_disguise = None;
    mon.form = form.map(|f| forms::Form(f.into()));
    mon.profile.as_mut().unwrap().instance_id = id.into();
    mon.graduated = true;
    mon.caught_at = (now() - 86400.) as u64;
    mon
}
fn open(s: &mut Shark, w: &mut Window, cx: &mut Context<Shark>) {
    s.show_usage(w, cx);
    s.usage_panel.as_mut().unwrap().reference_time = Some(now());
    s.state.usage_ledger = ledger();
    s.open_usage_recap(cx);
}

#[test]
fn current_period_labels_follow_scope() {
    assert_eq!(current_label(Scope::Year), "This year");
    assert_eq!(current_label(Scope::Week), "This week");
    assert_eq!(current_label(Scope::Month), "This month");
}
#[test]
fn accessible_meter_descriptions_distinguish_zero_from_unrecorded() {
    let mut observed = ledger();
    observed.tokens_by_day.retain(|key, _| key == "2026-09-17");
    for scope in Scope::ALL {
        let period = Period::containing(scope, date("2026-09-18"), 0);
        let recap = Recap::make(&observed, &[], period, date("2026-09-18"));
        let used = recap
            .buckets
            .iter()
            .find(|b| b.tokens == 12345)
            .unwrap_or_else(|| recap.buckets.iter().find(|b| b.tokens > 0).unwrap());
        let zero = recap
            .buckets
            .iter()
            .find(|b| b.has_data && b.tokens == 0)
            .unwrap();
        let missing = recap.buckets.iter().rev().find(|b| !b.has_data).unwrap();
        assert!(meter_label(used, scope).ends_with(&format!(", {}", grouped(used.tokens, ','))));
        assert!(meter_label(zero, scope).ends_with(", 0"));
        assert!(!meter_label(missing, scope).ends_with(", 0"));
        if scope == Scope::Year {
            assert!(meter_label(used, scope).starts_with("September 2026, "));
            assert!(meter_label(recap.buckets.first().unwrap(), scope).ends_with(", no data"));
        }
    }
}
// Adaptation: native Shark interface remains English; localized formatter/labels are tested above.
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_recap_card_renders_every_scope_and_preserves_layout(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx));
        for (scope, count) in [(Scope::Week, 7), (Scope::Month, 30), (Scope::Year, 12)] {
            w.render_frame(cx);
            w.click(format!("usage-recap-scope-{}", scope_name(scope)), cx);
            w.render_frame(cx);
            let card = w.find("usage-recap-card").bounds();
            let panel = w.find("usage-panel").bounds();
            assert!(card.size.width > px(250.));
            assert!(card.left() >= panel.left() && card.right() <= panel.right());
            assert!(w.find("usage-recap-period").visible());
            assert!(w.find("usage-recap-total").visible());
            assert_eq!(shark.read(cx).recap_content().0.buckets.len(), count);
            if scope == Scope::Month {
                assert!(
                    w.find("usage-recap-tick-2026-09-15").bounds().size.height <= px(12.),
                    "tick={:?}",
                    w.find("usage-recap-tick-2026-09-15").bounds()
                );
            }
            let meters = w.find("usage-recap-meters").bounds();
            assert!(meters.right() <= card.right());
            for meter in &shark.read(cx).recap_content().0.buckets {
                let bar = w.find(format!("usage-recap-meter-{}", meter.date));
                assert!(bar.bounds().left() >= meters.left() - px(0.5));
                assert!(bar.bounds().right() <= meters.right() + px(0.5));
                assert!(!bar.label().unwrap().is_empty());
            }
        }
    })
    .unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_graduate_strip_keeps_forms_and_current_companion_identity(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let mut state = State::default();
    let unown = mon(201, "Unown", Some("q"), "unown-q");
    let wormadam = mon(413, "Wormadam", Some("sandy"), "wormadam-sandy");
    assert_eq!(graduate_name(&unown), "Unown [Q]");
    assert_eq!(graduate_name(&wormadam), "Wormadam");
    let mut active = mon(25, "Pikachu", None, "active");
    active.graduated = false;
    state.pokemon.active = Some(active);
    state.pokemon.collection = vec![unown, wormadam];
    state.pokemon.representative = Some(201);
    let (window, shark) = boot(cx, state, WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx));
        w.render_frame(cx);
        assert_eq!(
            w.find("usage-recap-graduate-unown-q").label().as_deref(),
            Some("Unown [Q]")
        );
        assert_eq!(
            w.find("usage-recap-graduate-wormadam-sandy")
                .label()
                .as_deref(),
            Some("Wormadam")
        );
        for id in [
            "usage-recap-sprite-201-false-q",
            "usage-recap-sprite-413-false-sandy",
            "usage-recap-sprite-25-false-default",
        ] {
            assert!(w.find(id).visible());
        }
        assert_eq!(shark.read(cx).state.pokemon.representative, Some(201));
    })
    .unwrap();
}

#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn recap_controls_navigate_whole_periods_and_escape_returns_to_usage(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| open(s, w, cx));
        w.render_frame(cx);
        w.click("usage-recap-next", cx);
        assert_eq!(
            shark.read(cx).usage_panel.as_ref().unwrap().recap,
            Some((Scope::Week, 0))
        );
        w.click("usage-recap-scope-Month", cx);
        w.render_frame(cx);
        w.click("usage-recap-previous", cx);
        w.render_frame(cx);
        assert_eq!(
            shark.read(cx).usage_panel.as_ref().unwrap().recap,
            Some((Scope::Month, -1))
        );
        assert_eq!(
            shark.read(cx).recap_content().0.period.start,
            date("2026-08-01")
        );
        w.click("usage-recap-next", cx);
        w.render_frame(cx);
        assert_eq!(
            shark.read(cx).recap_content().0.period.start,
            date("2026-09-01")
        );
        w.click("usage-recap-scope-Year", cx);
        w.render_frame(cx);
        w.click("usage-recap-previous", cx);
        assert_eq!(
            shark.read(cx).usage_panel.as_ref().unwrap().recap,
            Some((Scope::Year, 0))
        );
        w.press("escape", cx);
        w.render_frame(cx);
        assert!(w.try_find("usage-recap").is_none());
        assert!(w.find("usage-panel").visible());
        w.press("escape", cx);
        w.render_frame(cx);
        assert!(w.try_find("usage-panel").is_none());
    })
    .unwrap();
}
