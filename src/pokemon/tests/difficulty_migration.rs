use super::super::*;
fn sample(egg: bool, boost: bool) -> Companion {
    let mut state = if egg {
        Companion::default()
    } else {
        super::economy::hatched()
    };
    if let Some(mon) = state.active.as_mut() {
        mon.growth_boost = boost;
    }
    let threshold = state.progress().1;
    if let Some(mon) = state.active.as_mut() {
        mon.tokens = threshold / 2;
        mon.reconcile_profile(1.);
    } else {
        state.egg_tokens = threshold / 2;
    }
    state.lifetime_tokens = 900_000_000;
    state.spent_tokens = 123_000_000;
    state.baselines.insert(
        "test".into(),
        UsageBaseline {
            day: "2026-09-11".into(),
            total: 1234,
        },
    );
    state
}
#[test]
fn difficulty_save_keeps_progress_profile_and_usage() {
    for egg in [false, true] {
        for boost in [false, true] {
            let mut state = sample(egg, boost);
            let before = state.clone();
            for difficulty in [0.1, 2., 0.5, 1.] {
                state.set_growth_difficulty(difficulty);
                assert_eq!(state.progress().0 * 2, state.progress().1);
                assert_eq!(
                    serde_json::to_value(state.active.as_ref().and_then(|m| m.profile.as_ref()))
                        .unwrap(),
                    serde_json::to_value(before.active.as_ref().and_then(|m| m.profile.as_ref()))
                        .unwrap()
                );
                assert_eq!(state.lifetime_tokens, before.lifetime_tokens);
                assert_eq!(state.spent_tokens, before.spent_tokens);
                assert_eq!(state.balance(), before.balance());
                assert_eq!(
                    serde_json::to_value(&state.baselines).unwrap(),
                    serde_json::to_value(&before.baselines).unwrap()
                );
                assert_eq!(
                    state.active.as_ref().map(|m| m.stage),
                    if egg { None } else { Some(0) }
                );
                assert!(state.collection.is_empty());
            }
        }
    }
}
#[test]
fn both_difficulty_controls_clamp_to_new_range() {
    let mut state = sample(false, false);
    state.set_growth_difficulty(0.001);
    state.set_shop_difficulty(10.);
    assert_eq!(state.growth_difficulty, 0.1);
    assert_eq!(state.shop_difficulty, 2.);
    state.set_growth_difficulty(20.);
    state.set_shop_difficulty(0.01);
    assert_eq!(state.growth_difficulty, 2.);
    assert_eq!(state.shop_difficulty, 0.1);
}
