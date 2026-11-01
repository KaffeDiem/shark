//! Additional adversarial regressions found during the integration QA review.
use super::*;

fn line() -> EvoLine {
    EvoLine {
        base_id: 1,
        tree: EvoNode {
            species_id: 1,
            children: vec![EvoNode {
                species_id: 2,
                children: vec![EvoNode {
                    species_id: 3,
                    children: vec![],
                }],
            }],
        },
        rarity: Rarity::Common,
        names: BTreeMap::new(),
    }
}
fn hatched() -> Companion {
    let mut s = Companion::default();
    s.egg_tokens = Balance::EGG_HATCH_THRESHOLD;
    s.hatch_with_disguise_roll(line(), false);
    s
}

#[test]
fn qa_released_profile_keeps_earned_growth_after_reload() {
    for difficulty in [0.1, 1., 2.] {
        for boosted in [false, true] {
            for stage in 0..3 {
                let mut s = hatched();
                s.set_growth_difficulty(difficulty);
                s.active.as_mut().unwrap().growth_boost = boosted;
                for _ in 0..stage {
                    s.add_growth(s.progress().1);
                }
                s.add_growth(s.progress().1 / 3);
                let earned = s.active.as_ref().unwrap().profile.clone().unwrap();
                s.lifetime_tokens = 1_000_000_000;
                s.fresh_egg(None).unwrap();
                let mut loaded = storage::decode(&storage::encode(&s, 0).unwrap())
                    .unwrap()
                    .state;
                assert_eq!(
                    loaded.collection[0].profile.as_ref().unwrap(),
                    &earned,
                    "difficulty={difficulty}, boosted={boosted}, stage={stage}"
                );
                let once = serde_json::to_value(&loaded).unwrap();
                loaded.sanitize();
                assert_eq!(serde_json::to_value(&loaded).unwrap(), once);
            }
        }
    }
}

#[test]
fn qa_restore_retains_fraction_under_current_device_difficulty() {
    for active in [false, true] {
        for (old, new) in [(2., 0.1), (0.1, 2.)] {
            let mut imported = if active {
                hatched()
            } else {
                Companion::default()
            };
            imported.set_growth_difficulty(old);
            let threshold = imported.progress().1;
            if let Some(mon) = imported.active.as_mut() {
                mon.tokens = threshold / 2;
            } else {
                imported.egg_tokens = threshold / 2;
            }
            let mut current = Companion::default();
            current.set_growth_difficulty(new);
            let result = storage::rebase(imported, &current, BTreeMap::new());
            assert_eq!(result.growth_difficulty, new);
            assert_eq!(
                result.progress().0,
                result.progress().1 / 2,
                "active={active}, old={old}, new={new}"
            );
            assert!(!result.ready_to_hatch());
        }
    }
}

#[test]
fn qa_cached_profile_moves_refresh_when_growth_crosses_learn_level() {
    let mut s = hatched();
    let details = profile::Details {
        species_id: 1,
        types: vec![],
        gender_rate: 0,
        base_stats: BTreeMap::new(),
        abilities: vec![],
        moves: vec![profile::Move {
            name: "learned-at-nine".into(),
            learn_methods: vec![profile::LearnMethod {
                method: "level-up".into(),
                level: 9,
            }],
        }],
    };
    s.enrich_profiles(&details);
    assert!(
        s.active
            .as_ref()
            .unwrap()
            .profile
            .as_ref()
            .unwrap()
            .moves
            .is_empty()
    );
    s.add_growth(40_000_000);
    let p = s.active.as_ref().unwrap().profile.as_ref().unwrap();
    assert!(p.level >= 9);
    assert_eq!(
        p.moves.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["learned-at-nine"]
    );
}

#[test]
fn qa_epoch_parser_rejects_extreme_years_and_nonfinite_offsets_without_panicking() {
    for text in [
        "9223372036854775807-01-01T00:00:00Z",
        "-9223372036854775808-01-01T00:00:00Z",
        "2026-10-31T00:00:00+NaN:00",
        "2026-10-31T00:00:00+-1:00",
        "2026-02-31T00:00:00Z",
    ] {
        assert_eq!(limits::parse_epoch(text), None, "{text}");
    }
    assert!(limits::parse_epoch("2024-02-29T12:30:00.5+01:00").is_some());
    assert_eq!(limits::parse_epoch("2025-02-29T12:30:00Z"), None);
}

#[test]
fn qa_failed_purchases_are_atomic_at_integer_boundaries() {
    for item in [Item::RareCandy, Item::Mint, Item::ShinyCharm] {
        for quantity in [0, 1, 2, u64::MAX] {
            let mut state = hatched();
            state.lifetime_tokens = u64::MAX;
            state.spent_tokens = u64::MAX - 1;
            state.inventory.candies = u64::MAX;
            state.inventory.mints = u64::MAX;
            let before = serde_json::to_value(&state).unwrap();
            assert!(state.buy(item, quantity).is_err());
            assert_eq!(serde_json::to_value(&state).unwrap(), before);
        }
    }
    for item in [Item::RareCandy, Item::Mint] {
        let mut state = hatched();
        state.lifetime_tokens = u64::MAX;
        state.inventory.candies = u64::MAX;
        state.inventory.mints = u64::MAX;
        let before = serde_json::to_value(&state).unwrap();
        assert!(state.buy(item, 1).is_err());
        assert_eq!(serde_json::to_value(&state).unwrap(), before);
    }
}

#[test]
fn qa_buy_and_candy_cannot_create_or_consume_wallet_credit_twice() {
    let mut state = hatched();
    state.lifetime_tokens = 4_000_000_000;
    state.buy(Item::RareCandy, 8).unwrap();
    assert_eq!(state.balance(), 0);
    state.use_candy(u64::MAX).unwrap();
    assert_eq!(state.inventory.candies, 0);
    assert!(state.active.is_none());
    assert_eq!(state.collection.len(), 1);
    assert_eq!(state.lifetime_tokens, 4_000_000_000);
    assert_eq!(state.spent_tokens, 4_000_000_000);
    let before = serde_json::to_value(&state).unwrap();
    assert!(state.use_candy(1).is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
}

#[test]
fn qa_missing_reset_timestamp_cannot_rearm_an_already_claimed_known_epoch() {
    let mut state = Companion::default();
    let epoch = Some("1800000000");
    state.observe_limit("claude:session", epoch, 50., false, true);
    assert_eq!(
        state.observe_limit("claude:session", epoch, 100., false, true),
        1
    );
    state.observe_limit("claude:session", None, 50., false, true);
    assert_eq!(
        state.observe_limit("claude:session", epoch, 100., false, true),
        0
    );
    assert_eq!(state.inventory.candies, 1);
}

// Intentional anti-duplication adaptation: retain the day's high-water mark.
#[test]
fn qa_restored_logs_cannot_recredit_same_day_usage_after_regression() {
    let mut state = Companion::default();
    state.observe_usage("claude", "2026-10-31", 100_000, true);
    state.observe_usage("claude", "2026-10-31", 200_000, true);
    state.observe_usage("claude", "2026-10-31", 50_000, true);
    state.observe_usage("claude", "2026-10-31", 200_000, true);
    assert_eq!(state.lifetime_tokens, 100_000);
    state.observe_usage("claude", "2026-10-31", 250_000, true);
    assert_eq!(state.lifetime_tokens, 150_000);
    assert_eq!(state.egg_tokens, 150_000);
}

#[test]
fn qa_older_quota_epoch_cannot_claim_the_current_window() {
    for weekly in [false, true] {
        let mut state = Companion::default();
        state.observe_limit("quota", Some("1800000000"), 50., weekly, true);
        state.observe_limit("quota", Some("1800604800"), 50., weekly, true);
        assert_eq!(
            state.observe_limit("quota", Some("1800000000"), 100., weekly, true),
            0
        );
        assert_eq!(state.inventory.candies, 0);
        assert!(!state.limit_baselines["quota"].claimed);
        assert_eq!(
            state.limit_baselines["quota"].epoch.as_deref(),
            Some("1800604800")
        );
        assert_eq!(
            state.observe_limit("quota", Some("1800604800"), 100., weekly, true),
            if weekly { 5 } else { 1 }
        );
    }
}

#[test]
fn qa_profile_details_keep_two_valid_lowercase_types() {
    let stats = profile::STAT_ORDER
        .iter()
        .map(|name| serde_json::json!({"stat":{"name":name},"base_stat":50}))
        .collect::<Vec<_>>();
    let pokemon = serde_json::json!({"stats":stats,"types":[{"type":{"name":"GRASS"}},{"type":{"name":"grass"}},{"type":{"name":"stellar"}},{"type":{"name":"grass"}},{"type":{"name":"poison"}}]});
    let details = api::parse_details(1, &serde_json::json!({"gender_rate":1}), &pokemon).unwrap();
    assert_eq!(details.types, ["grass", "poison"]);
    let mut legacy = serde_json::to_value(&details).unwrap();
    legacy.as_object_mut().unwrap().remove("types");
    assert!(
        serde_json::from_value::<profile::Details>(legacy)
            .unwrap()
            .types
            .is_empty()
    );
}

#[test]
fn qa_mint_never_repeats_a_legacy_lowercase_nature() {
    let template = hatched();
    for seed in 0..128 {
        let mut state = template.clone();
        state.active.as_mut().unwrap().nature = "adamant".into();
        state.inventory.mints = 1;
        state.random_seed = seed;
        state.use_mint().unwrap();
        assert!(
            !state
                .active
                .as_ref()
                .unwrap()
                .nature
                .eq_ignore_ascii_case("adamant"),
            "seed={seed}"
        );
    }
}
