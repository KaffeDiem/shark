use super::*;
use crate::app::ui_tests::{UI_LOCK, boot, test_home};
use crate::usage::logs::Report;
use crate::usage::providers::{Registry, UsageProvider};
use core::prelude::v1::test;
use gpui_kit::test::TestWindowExt;
struct Tool {
    id: String,
    name: String,
}
impl UsageProvider for Tool {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn remaining_usage(&self) -> Result<Vec<Allowance>, Failure> {
        Err(Failure::Unsupported)
    }
    fn activity(&self, _: f64) -> Daily {
        Daily::default()
    }
}
fn sources(s: &mut Shark, count: usize) {
    let mut registry = Registry::default();
    for i in 0..count {
        registry
            .register(Tool {
                id: format!("source-{i}"),
                name: format!("Future Coding Provider {i}"),
            })
            .unwrap();
    }
    let registry = Box::leak(Box::new(registry));
    s.usage_providers = registry.handles().collect();
    s.usage = vec![Default::default(); count];
    s.usage_activity = vec![Default::default(); count];
    s.usage_queries = vec![0; count];
    s.usage_reports = vec![Report::default(); count];
    s.pokemon_ui.scanning = vec![false; count];
}
// upstream: ProviderTabLayoutTests.swift::testTabListMatchesTheRegisteredProviders
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_tabs_come_from_registered_sources(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.show_usage(w, cx));
        w.render_frame(cx);
        for provider in &shark.read(cx).usage_providers {
            assert!(
                w.find(format!("usage-provider-tab-{}", provider.id()))
                    .visible()
            );
        }
        assert_eq!(
            shark
                .read(cx)
                .usage_providers
                .iter()
                .map(|p| p.id())
                .collect::<Vec<_>>(),
            ["claude", "codex"]
        );
    })
    .unwrap();
}
// upstream: ProviderTabLayoutTests.swift::testAllProviderTabsExceedPopoverContentWidthWhenLaidOutInOneRow
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn future_source_tabs_reproduce_natural_overflow_without_expanding_parent(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            sources(s, 14);
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        let width = w.find("usage-provider-tabs").bounds().size.width;
        let sum = (0..14)
            .map(|i| {
                f32::from(
                    w.find(format!("usage-provider-tab-source-{i}"))
                        .bounds()
                        .size
                        .width,
                )
            })
            .sum::<f32>();
        assert!(
            sum > f32::from(width) * 2.,
            "fixture must actually require horizontal scrolling"
        );
        assert!(width <= w.find("usage-panel").bounds().size.width);
    })
    .unwrap();
}
// upstream: ProviderTabLayoutTests.swift::testProviderTabBarStaysSingleRowWithAllProviders
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn native_provider_tabs_keep_one_row_for_two_or_fourteen_sources(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        let mut heights = vec![];
        for count in [2, 14] {
            shark.update(cx, |s, cx| {
                sources(s, count);
                s.show_usage(w, cx);
            });
            w.render_frame(cx);
            let bar = w.find("usage-provider-tabs").bounds();
            heights.push(bar.size.height);
            for i in 0..count {
                let tab = w.find(format!("usage-provider-tab-source-{i}")).bounds();
                assert!(tab.top() >= bar.top());
                assert!(tab.bottom() <= bar.bottom() + px(0.5));
            }
        }
        assert_eq!(heights, [px(28.), px(28.)]);
    })
    .unwrap();
}
// upstream: ProviderTabLayoutTests.swift::testProviderTabBarFitsPopoverContentWidth
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn scrollable_source_tabs_stay_inside_popover_width(cx: &mut TestAppContext) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| {
            sources(s, 14);
            s.show_usage(w, cx);
        });
        w.render_frame(cx);
        let panel = w.find("usage-panel").bounds();
        let tabs = w.find("usage-provider-tabs").bounds();
        assert!(tabs.left() >= panel.left());
        assert!(tabs.right() <= panel.right());
    })
    .unwrap();
}
// upstream: ProviderTabLayoutTests.swift::testNoSingleProviderTabExceedsContentWidth
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn even_a_long_future_source_name_cannot_expand_one_tab_past_content(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window,|_,w,cx|{shark.update(cx,|s,cx|{let mut registry=Registry::default();registry.register(Tool{id:"long-name".into(),name:"Future source with a deliberately exceptionally long descriptive coding-tool name".into()}).unwrap();let registry=Box::leak(Box::new(registry));s.usage_providers=registry.handles().collect();s.usage.truncate(1);s.usage_activity.truncate(1);s.usage_reports.truncate(1);s.usage_queries.truncate(1);s.pokemon_ui.scanning.truncate(1);s.show_usage(w,cx);});w.render_frame(cx);assert!(w.find("usage-provider-tab-long-name").bounds().size.width<=w.find("usage-provider-tabs").bounds().size.width);assert!(w.find("usage-provider-tab-long-name").bounds().size.height<=px(28.));}).unwrap();
}
#[gpui_kit::test]
#[allow(clippy::await_holding_lock)]
async fn selecting_provider_filters_details_without_refetching_or_losing_combined_totals(
    cx: &mut TestAppContext,
) {
    let _turn = UI_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    test_home();
    let (window, shark) = boot(cx, State::default(), WindowOptions::default());
    cx.update_window(window, |_, w, cx| {
        shark.update(cx, |s, cx| s.show_usage(w, cx));
        w.render_frame(cx);
        let queries = shark.read(cx).usage_queries.clone();
        w.click("usage-provider-tab-codex", cx);
        w.render_frame(cx);
        assert!(w.try_find("usage-claude").is_none());
        assert!(w.find("usage-codex").visible());
        assert!(w.find("usage-combined-totals").visible());
        assert_eq!(shark.read(cx).usage_queries, queries);
        w.click("usage-provider-all", cx);
        w.render_frame(cx);
        assert!(w.find("usage-claude").visible());
        assert!(w.find("usage-codex").visible());
    })
    .unwrap();
}
