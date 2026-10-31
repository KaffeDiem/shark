use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::Provider;
use core::prelude::v1::test;
use gpui::TestAppContext;

// upstream: RareCandyTests.swift::testGrantCarriesWindowName
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn candy_notice_identifies_the_window_once(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, _, cx| {
        shark.update(cx, |s, _| {
            let index = crate::usage::providers::ProviderHandle::from(Provider::Claude).index();
            s.usage[index].finish(Ok(vec![crate::usage::Allowance {
                label: "5 hours".into(),
                remaining: Some(50.),
                resets_at: Some(1_800_000_000.),
                window: Some(18_000.),
            }]));
            s.observe_pokemon_limits(Provider::Claude);
            assert_eq!(s.state.pokemon.inventory.candies, 0);
            s.usage[index].allowances[0].remaining = Some(0.);
            s.observe_pokemon_limits(Provider::Claude);
            assert_eq!(s.state.pokemon.inventory.candies, 1);
            assert_eq!(
                s.pokemon_ui.notice.as_deref(),
                Some("Claude 5-hour limit reached — received 1 Rare Candy!")
            );
        })
    })
    .unwrap();
}
