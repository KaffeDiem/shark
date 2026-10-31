use super::super::*;
use super::economy::{hatched, line};
fn seeded(key: &str, epoch: Option<&str>, weekly: bool) -> Companion {
    let mut s = Companion::default();
    s.observe_limit(key, epoch, 0., weekly, true);
    s
}
// upstream: RareCandyTests.swift::testSessionGrantsOne
#[test]
fn testSessionGrantsOne() {
    let mut s = seeded("s", None, false);
    assert_eq!(s.observe_limit("s", None, 100., false, true), 1);
    assert!(s.limit_baselines["s"].claimed);
}
// upstream: RareCandyTests.swift::testWeeklyGrantsFive
#[test]
fn testWeeklyGrantsFive() {
    let mut s = seeded("wk", None, true);
    assert_eq!(s.observe_limit("wk", None, 100., true, true), 5);
}
// upstream: RareCandyTests.swift::testBelow100NoGrant
#[test]
fn testBelow100NoGrant() {
    let mut s = seeded("s", None, false);
    assert_eq!(s.observe_limit("s", None, 99.9, false, true), 0);
    assert!(!s.limit_baselines["s"].claimed);
}
// upstream: RareCandyTests.swift::testNoDoubleGrantWhileAt100
#[test]
fn testNoDoubleGrantWhileAt100() {
    let mut s = seeded("s", Some("E1"), false);
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 1);
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 0);
}
// upstream: RareCandyTests.swift::testRearmAfterDropBelow100
#[test]
fn testRearmAfterDropBelow100() {
    let mut s = seeded("s", None, false);
    assert_eq!(s.observe_limit("s", None, 100., false, true), 1);
    s.observe_limit("s", None, 40., false, true);
    assert!(!s.limit_baselines["s"].claimed);
    assert_eq!(s.observe_limit("s", None, 100., false, true), 1);
}
// upstream: RareCandyTests.swift::testRearmWhenWindowEpochAdvancesWhileStillAt100
#[test]
fn testRearmWhenWindowEpochAdvancesWhileStillAt100() {
    let mut s = seeded("claude.fiveHour", Some("2026-09-17T10:00:00Z"), false);
    assert_eq!(
        s.observe_limit(
            "claude.fiveHour",
            Some("2026-09-17T10:00:00Z"),
            100.,
            false,
            true
        ),
        1
    );
    assert_eq!(
        s.observe_limit(
            "claude.fiveHour",
            Some("2026-09-17T15:00:00Z"),
            100.,
            false,
            true
        ),
        1
    );
    assert_eq!(
        s.limit_baselines["claude.fiveHour"].epoch.as_deref(),
        Some("2026-09-17T15:00:00Z")
    );
}
// upstream: RareCandyTests.swift::testVolatileMicrosecondEpochDoesNotRegrant
#[test]
fn testVolatileMicrosecondEpochDoesNotRegrant() {
    let key = "claude.account.fiveHour";
    let mut s = seeded(key, Some("2026-09-28T17:10:00.928794+00:00"), false);
    assert_eq!(
        s.observe_limit(
            key,
            Some("2026-09-28T17:10:00.928794+00:00"),
            100.,
            false,
            true
        ),
        1
    );
    assert_eq!(
        s.observe_limit(
            key,
            Some("2026-09-28T17:10:00.935515+00:00"),
            100.,
            false,
            true
        ),
        0
    );
    assert!(s.limit_baselines[key].claimed);
}
// upstream: RareCandyTests.swift::testCodexJitterEpochDoesNotRegrant
#[test]
fn testCodexJitterEpochDoesNotRegrant() {
    let key = "codex.primary";
    let mut s = seeded(key, Some("1790613843"), false);
    assert_eq!(
        s.observe_limit(key, Some("1790613843"), 100., false, true),
        1
    );
    assert_eq!(
        s.observe_limit(key, Some("1790613844"), 100., false, true),
        0
    );
    assert_eq!(
        s.observe_limit(key, Some("1790613843"), 100., false, true),
        0
    );
    assert!(s.limit_baselines[key].claimed);
}
// upstream: RareCandyTests.swift::testRollingWindowMinorDriftDoesNotRegrant
#[test]
fn testRollingWindowMinorDriftDoesNotRegrant() {
    let key = "generic.5h";
    let mut s = seeded(key, Some("2026-09-28T19:10:12Z"), false);
    assert_eq!(
        s.observe_limit(key, Some("2026-09-28T19:10:12Z"), 100., false, true),
        1
    );
    assert_eq!(
        s.observe_limit(key, Some("2026-09-28T19:11:12Z"), 100., false, true),
        0
    );
    assert_eq!(
        s.observe_limit(key, Some("2026-09-28T19:15:12Z"), 100., false, true),
        0
    );
}
// upstream: RareCandyTests.swift::testSessionFullResetWhileStillAt100Regrants
#[test]
fn testSessionFullResetWhileStillAt100Regrants() {
    let key = "codex.primary";
    let mut s = seeded(key, Some("1790613843"), false);
    assert_eq!(
        s.observe_limit(key, Some("1790613843"), 100., false, true),
        1
    );
    assert_eq!(
        s.observe_limit(key, Some("1790631843"), 100., false, true),
        1
    );
    assert_eq!(s.limit_baselines[key].epoch.as_deref(), Some("1790631843"));
}
// upstream: RareCandyTests.swift::testWeeklyFullResetWhileStillAt100Regrants
#[test]
fn testWeeklyFullResetWhileStillAt100Regrants() {
    let key = "claude.sevenDay";
    let mut s = seeded(key, Some("2026-09-20T00:00:00Z"), true);
    s.observe_limit(key, Some("2026-09-20T00:00:00Z"), 100., true, true);
    assert_eq!(
        s.observe_limit(key, Some("2026-09-27T00:00:00Z"), 100., true, true),
        5
    );
    assert_eq!(
        s.limit_baselines[key].epoch.as_deref(),
        Some("2026-09-27T00:00:00Z")
    );
}
// upstream: RareCandyTests.swift::testLearningEpochForFirstTimeDoesNotRegrant
#[test]
fn testLearningEpochForFirstTimeDoesNotRegrant() {
    let mut s = seeded("claude.fiveHour", None, false);
    s.observe_limit("claude.fiveHour", None, 100., false, true);
    assert_eq!(
        s.observe_limit("claude.fiveHour", Some("E1"), 100., false, true),
        0
    );
    assert_eq!(
        s.limit_baselines["claude.fiveHour"].epoch.as_deref(),
        Some("E1")
    );
    assert!(s.limit_baselines["claude.fiveHour"].claimed);
}
// upstream: RareCandyTests.swift::testMixedWindows
#[test]
fn testMixedWindows() {
    let mut s = Companion::default();
    for (key, weekly) in [
        ("claude.fiveHour", false),
        ("claude.sevenDay", true),
        ("codex.primary", false),
    ] {
        s.observe_limit(key, None, 0., weekly, true);
    }
    let total = s.observe_limit("claude.fiveHour", None, 100., false, true)
        + s.observe_limit("claude.sevenDay", None, 100., true, true)
        + s.observe_limit("codex.primary", None, 50., false, true);
    assert_eq!(total, 6);
    assert_eq!(s.inventory.candies, 6);
}
// upstream: RareCandyTests.swift::testDecodesWithoutInventoryFields
#[test]
fn testDecodesWithoutInventoryFields() {
    let s: Companion = serde_json::from_str("{}").unwrap();
    assert_eq!(s.inventory.candies, 0);
    assert_eq!(s.inventory.mints, 0);
    assert!(!s.inventory.shiny_charm);
    assert!(s.limit_baselines.is_empty());
}
// upstream: RareCandyTests.swift::testInventoryRoundTrip
#[test]
fn testInventoryRoundTrip() {
    let mut s = seeded("s", Some("E1"), false);
    s.inventory.candies = 7;
    s.observe_limit("s", Some("E1"), 100., false, true);
    let restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(restored.inventory.candies, 8);
    assert!(restored.limit_baselines["s"].claimed);
}
// upstream: RareCandyTests.swift::testFirstRunSeedsWithoutGranting
#[test]
fn testFirstRunSeedsWithoutGranting() {
    let mut s = Companion::default();
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 0);
    assert_eq!(s.inventory.candies, 0);
    assert!(s.limit_baselines["s"].claimed);
}
// upstream: RareCandyTests.swift::testNoGrantForAlreadySeededWindow
#[test]
fn testNoGrantForAlreadySeededWindow() {
    let mut s = Companion::default();
    s.observe_limit("s", Some("E1"), 100., false, true);
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 0);
    assert_eq!(s.inventory.candies, 0);
}
// upstream: RareCandyTests.swift::testGrantTierPersistsAcrossRestartNoDoubleGrant
#[test]
fn testGrantTierPersistsAcrossRestartNoDoubleGrant() {
    let mut s = seeded("s", Some("E1"), false);
    s.observe_limit("s", Some("E1"), 100., false, true);
    let mut restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(
        restored.observe_limit("s", Some("E1"), 100., false, true),
        0
    );
    assert_eq!(restored.inventory.candies, 1);
}
// upstream: RareCandyTests.swift::testRearmPersistsAcrossRestart
#[test]
fn testRearmPersistsAcrossRestart() {
    let mut s = seeded("s", None, false);
    s.observe_limit("s", None, 100., false, true);
    s.observe_limit("s", None, 40., false, true);
    let mut restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(restored.observe_limit("s", None, 100., false, true), 1);
    assert_eq!(restored.inventory.candies, 2);
}
// upstream: RareCandyTests.swift::testUseProgressesWithoutEvolution
#[test]
fn testUseProgressesWithoutEvolution() {
    let mut s = hatched();
    s.inventory.candies = 1;
    let before = s.lifetime_tokens;
    assert!(s.use_candy(1).unwrap().is_empty());
    assert_eq!(s.active.as_ref().unwrap().tokens, 100_000_000);
    assert_eq!(s.active.as_ref().unwrap().stage, 0);
    assert_eq!(s.inventory.candies, 0);
    assert_eq!(s.lifetime_tokens, before);
}
// upstream: RareCandyTests.swift::testUseEvolvesWhenCrossingThreshold
#[test]
fn testUseEvolvesWhenCrossingThreshold() {
    let mut s = hatched();
    s.add_growth(50_000_000);
    s.inventory.candies = 1;
    assert!(matches!(
        s.use_candy(1).unwrap().as_slice(),
        [Event::Evolved(_)]
    ));
    assert_eq!(s.active.as_ref().unwrap().species_id(), 2);
    assert_eq!(s.active.as_ref().unwrap().stage, 1);
}
// upstream: RareCandyTests.swift::testSingleCandyAdvancesAtMostOneStage
#[test]
fn testSingleCandyAdvancesAtMostOneStage() {
    let mut s = hatched();
    s.add_growth(124_000_000);
    s.inventory.candies = 1;
    s.use_candy(1).unwrap();
    assert_eq!(s.active.as_ref().unwrap().stage, 1);
}
// upstream: RareCandyTests.swift::testSingleCandyAdvancesAtMostOneBoostedStage
#[test]
fn testSingleCandyAdvancesAtMostOneBoostedStage() {
    let mut s = hatched();
    s.add_growth(750_000_000);
    s.egg_tokens = s.progress().1;
    s.hatch(line(3));
    assert!(s.active.as_ref().unwrap().growth_boost);
    s.add_growth(s.progress().1 - 1);
    s.inventory.candies = 1;
    assert!(matches!(
        s.use_candy(1).unwrap().as_slice(),
        [Event::Evolved(_)]
    ));
    assert_eq!(s.active.as_ref().unwrap().stage, 1);
    assert_eq!(s.active.as_ref().unwrap().tokens, 99_999_999);
    assert!(s.progress().0 < s.progress().1);
}
// upstream: RareCandyTests.swift::testUseGraduatesFinalStage
#[test]
fn testUseGraduatesFinalStage() {
    let mut s = Companion::default();
    s.egg_tokens = s.progress().1;
    s.hatch(line(1));
    s.add_growth(700_000_000);
    s.inventory.candies = 1;
    assert!(matches!(
        s.use_candy(1).unwrap().as_slice(),
        [Event::Graduated(_)]
    ));
    assert!(s.active.is_none());
    assert_eq!(s.collection.len(), 1);
}
// upstream: RareCandyTests.swift::testCannotUseOnEgg
#[test]
fn testCannotUseOnEgg() {
    let mut s = Companion::default();
    s.inventory.candies = 2;
    assert!(s.use_candy(1).is_err());
    assert_eq!(s.inventory.candies, 2);
}
// upstream: RareCandyTests.swift::testCannotUseWithoutStock
#[test]
fn testCannotUseWithoutStock() {
    let mut s = hatched();
    assert_eq!(s.inventory.candies, 0);
    assert!(s.use_candy(1).is_err());
}
// upstream: RareCandyTests.swift::testSequentialCandyUseMatchesDemo
#[test]
fn testSequentialCandyUseMatchesDemo() {
    let mut s = hatched();
    s.add_growth(100_000_000);
    s.inventory.candies = 3;
    assert!(matches!(
        s.use_candy(1).unwrap().as_slice(),
        [Event::Evolved(_)]
    ));
    assert_eq!(s.active.as_ref().unwrap().stage, 1);
    assert!(s.use_candy(1).unwrap().is_empty());
    assert_eq!(s.active.as_ref().unwrap().stage, 1);
    assert!(matches!(
        s.use_candy(1).unwrap().as_slice(),
        [Event::Evolved(_)]
    ));
    assert_eq!(s.active.as_ref().unwrap().stage, 2);
    assert_eq!(s.inventory.candies, 0);
}
// upstream: RareCandyTests.swift::testBatchCapsAtGraduationAndPreservesUnusedCandies
#[test]
fn testBatchCapsAtGraduationAndPreservesUnusedCandies() {
    for remaining in [250_000_000, 300_000_000] {
        let mut s = Companion::default();
        s.egg_tokens = s.progress().1;
        s.hatch(line(1));
        s.add_growth(750_000_000 - remaining);
        s.inventory.candies = 5;
        assert_eq!(s.max_candy_use_count(), 3);
        let preview = s.plan_candy_use(5).unwrap();
        assert_eq!(preview.count, 3);
        assert!(preview.graduates);
        assert_eq!(preview.discarded_xp, 300_000_000 - remaining);
        assert!(matches!(
            s.use_candy(5).unwrap().as_slice(),
            [Event::Graduated(_)]
        ));
        assert!(s.active.is_none());
        assert_eq!(s.inventory.candies, 2);
        assert_eq!(s.egg_tokens, 0);
        assert_eq!(s.collection.len(), 1);
        assert_eq!(s.max_candy_use_count(), 0);
        assert!(s.use_candy(5).is_err());
    }
}
// upstream: RareCandyTests.swift::testBatchCarriesXPThroughMultipleEvolutions
#[test]
fn testBatchCarriesXPThroughMultipleEvolutions() {
    let mut s = hatched();
    s.add_growth(100_000_000);
    s.inventory.candies = 10;
    let before = s.lifetime_tokens;
    let plan = s.plan_candy_use(3).unwrap();
    assert_eq!(s.max_candy_use_count(), 7);
    assert_eq!(plan.count, 3);
    assert!(plan.evolves);
    assert!(!plan.graduates);
    assert_eq!(plan.carryover_xp, 25_000_000);
    assert_eq!(plan.discarded_xp, 0);
    let events = s.use_candy(3).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Evolved(_))));
    assert_eq!(s.active.as_ref().unwrap().stage, 2);
    assert_eq!(s.active.as_ref().unwrap().tokens, plan.carryover_xp);
    assert_eq!(s.inventory.candies, 7);
    assert_eq!(s.feedback.candy_amount, 300_000_000);
    assert_eq!(s.lifetime_tokens, before);
}
// upstream: RareCandyTests.swift::testBatchCapsAtInventoryAndRejectsNonpositiveAmounts
#[test]
fn testBatchCapsAtInventoryAndRejectsNonpositiveAmounts() {
    let mut s = hatched();
    s.inventory.candies = 1;
    assert!(s.plan_candy_use(0).is_none());
    assert!(s.use_candy(0).is_err());
    assert!(serde_json::from_str::<u64>("-1").is_err());
    assert!(serde_json::from_str::<u64>("-9223372036854775808").is_err());
    assert_eq!(s.inventory.candies, 1);
    assert_eq!(s.max_candy_use_count(), 1);
    let plan = s.plan_candy_use(u64::MAX).unwrap();
    assert_eq!(plan.count, 1);
    assert!(!plan.evolves);
    assert!(s.use_candy(u64::MAX).unwrap().is_empty());
    assert_eq!(s.active.as_ref().unwrap().tokens, CANDY_XP);
    assert_eq!(s.inventory.candies, 0);
}
// upstream: RareCandyTests.swift::testBatchPreviewMatchesGrowthWithDifficultyAndRepeatBonus
#[test]
fn testBatchPreviewMatchesGrowthWithDifficultyAndRepeatBonus() {
    for difficulty in [0.1, 0.5, 1., 2.] {
        for boosted in [false, true] {
            let mut s = hatched();
            s.set_growth_difficulty(difficulty);
            if boosted {
                s.add_growth(10_000_000_000);
                s.egg_tokens = s.progress().1;
                s.hatch(line(3));
            }
            s.add_growth(s.progress().1 - 1);
            s.inventory.candies = 20;
            let preview = s.plan_candy_use(1).unwrap();
            s.use_candy(1).unwrap();
            if preview.graduates {
                assert!(s.active.is_none());
            } else {
                assert_eq!(s.active.as_ref().unwrap().tokens, preview.carryover_xp);
                let final_plan = s.plan_candy_use(20).unwrap();
                assert!(final_plan.graduates);
                let stock = s.inventory.candies;
                s.use_candy(20).unwrap();
                assert_eq!(s.inventory.candies, stock - final_plan.count);
            }
            let restored: Companion =
                serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
            assert_eq!(restored.inventory.candies, s.inventory.candies);
            assert!(restored.active.is_none());
            assert_eq!(restored.collection.len(), s.collection.len());
        }
    }
}
// upstream: RareCandyTests.swift::testUseBumpsCandyFeedback
#[test]
fn testUseBumpsCandyFeedback() {
    let mut s = hatched();
    s.inventory.candies = 1;
    let before = s.feedback.candy_seq;
    s.use_candy(1).unwrap();
    assert_eq!(s.feedback.candy_seq, before + 1);
    assert_eq!(s.feedback.candy_amount, CANDY_XP);
}
// upstream: RareCandyTests.swift::testOwnedItemsReflectsStock
#[test]
fn testOwnedItemsReflectsStock() {
    let mut s = Companion::default();
    assert!(s.owned_items().is_empty());
    s.inventory.candies = 3;
    assert_eq!(s.owned_items(), [(Item::RareCandy, 3)]);
}
// upstream: RareCandyTests.swift::testConsumeCandyFeedbackResets
#[test]
fn testConsumeCandyFeedbackResets() {
    let mut s = hatched();
    s.inventory.candies = 1;
    s.use_candy(1).unwrap();
    assert_eq!(s.feedback.candy_amount, CANDY_XP);
    s.consume_candy_feedback();
    assert_eq!(s.feedback.candy_amount, 0);
}
// upstream: RareCandyTests.swift::testGrantAfterSeedWhenEpochAdvancesWithoutUtilDip
#[test]
fn testGrantAfterSeedWhenEpochAdvancesWithoutUtilDip() {
    let mut s = Companion::default();
    s.observe_limit("claude.fiveHour", Some("E1"), 100., false, true);
    assert_eq!(s.inventory.candies, 0);
    s.observe_limit("claude.fiveHour", Some("E1"), 100., false, true);
    assert_eq!(s.inventory.candies, 0);
    s.observe_limit("claude.fiveHour", Some("E2"), 100., false, true);
    assert_eq!(s.inventory.candies, 1);
    assert_eq!(
        s.limit_baselines["claude.fiveHour"].epoch.as_deref(),
        Some("E2")
    );
}
// The Rust adapter seeds each provider separately instead of granting retroactively to newly found accounts.
// upstream: RareCandyTests.swift::testSessionGrantAfterSeed
#[test]
fn testSessionGrantAfterSeed() {
    let mut s = seeded("claude.fiveHour", None, false);
    s.observe_limit("claude.fiveHour", None, 100., false, true);
    assert_eq!(s.inventory.candies, 1);
}
// upstream: RareCandyTests.swift::testWeeklyGrantsFiveCandies
#[test]
fn testWeeklyGrantsFiveCandies() {
    let mut s = seeded("claude.sevenDay", None, true);
    s.observe_limit("claude.sevenDay", None, 100., true, true);
    assert_eq!(s.inventory.candies, 5);
}
// Evolution data is persisted in Rust. An invalid/unloaded line is rejected on decode before items can be used.
// upstream: RareCandyTests.swift::testCannotUseWhileLineUnloaded
#[test]
fn testCannotUseWhileLineUnloaded() {
    let mut s = hatched();
    s.inventory.candies = 1;
    let mut json = serde_json::to_value(&s).unwrap();
    json["active"]["line"] = serde_json::Value::Null;
    let restored: Companion = serde_json::from_value(json).unwrap();
    let mut restored = restored;
    restored.sanitize();
    assert!(restored.active.is_none());
    assert!(restored.use_candy(1).is_err());
    assert_eq!(restored.inventory.candies, 1);
}

#[test]
fn candy_preview_preserves_disguise_until_reveal_and_waits_after_threshold() {
    let mut plain = hatched();
    plain.inventory.candies = 20;
    let mut disguised = plain.clone();
    disguised.active.as_mut().unwrap().ditto_disguise = Some(1);
    assert_eq!(plain.plan_candy_use(20), disguised.plan_candy_use(20));
    let threshold = disguised.progress().1;
    disguised.add_growth(threshold);
    assert_eq!(disguised.max_candy_use_count(), 0);
    assert!(disguised.plan_candy_use(1).is_none());
}
