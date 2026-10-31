use super::super::*;
use super::economy::{hatched, line};
fn seeded(key: &str, epoch: Option<&str>, weekly: bool) -> Companion {
    let mut s = Companion::default();
    s.observe_limit(key, epoch, 0., weekly, true);
    s
}
#[test]
fn testSessionGrantsOne() {
    let mut s = seeded("s", None, false);
    assert_eq!(s.observe_limit("s", None, 100., false, true), 1);
    assert!(s.limit_baselines["s"].claimed);
}
#[test]
fn testWeeklyGrantsFive() {
    let mut s = seeded("wk", None, true);
    assert_eq!(s.observe_limit("wk", None, 100., true, true), 5);
}
#[test]
fn testBelow100NoGrant() {
    let mut s = seeded("s", None, false);
    assert_eq!(s.observe_limit("s", None, 99.9, false, true), 0);
    assert!(!s.limit_baselines["s"].claimed);
}
#[test]
fn testNoDoubleGrantWhileAt100() {
    let mut s = seeded("s", Some("E1"), false);
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 1);
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 0);
}
#[test]
fn testRearmAfterDropBelow100() {
    let mut s = seeded("s", None, false);
    assert_eq!(s.observe_limit("s", None, 100., false, true), 1);
    s.observe_limit("s", None, 40., false, true);
    assert!(!s.limit_baselines["s"].claimed);
    assert_eq!(s.observe_limit("s", None, 100., false, true), 1);
}
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
#[test]
fn testDecodesWithoutInventoryFields() {
    let s: Companion = serde_json::from_str("{}").unwrap();
    assert_eq!(s.inventory.candies, 0);
    assert_eq!(s.inventory.mints, 0);
    assert!(!s.inventory.shiny_charm);
    assert!(s.limit_baselines.is_empty());
}
#[test]
fn testInventoryRoundTrip() {
    let mut s = seeded("s", Some("E1"), false);
    s.inventory.candies = 7;
    s.observe_limit("s", Some("E1"), 100., false, true);
    let restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(restored.inventory.candies, 8);
    assert!(restored.limit_baselines["s"].claimed);
}
#[test]
fn testFirstRunSeedsWithoutGranting() {
    let mut s = Companion::default();
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 0);
    assert_eq!(s.inventory.candies, 0);
    assert!(s.limit_baselines["s"].claimed);
}
#[test]
fn testNoGrantForAlreadySeededWindow() {
    let mut s = Companion::default();
    s.observe_limit("s", Some("E1"), 100., false, true);
    assert_eq!(s.observe_limit("s", Some("E1"), 100., false, true), 0);
    assert_eq!(s.inventory.candies, 0);
}
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
#[test]
fn testRearmPersistsAcrossRestart() {
    let mut s = seeded("s", None, false);
    s.observe_limit("s", None, 100., false, true);
    s.observe_limit("s", None, 40., false, true);
    let mut restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(restored.observe_limit("s", None, 100., false, true), 1);
    assert_eq!(restored.inventory.candies, 2);
}
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
#[test]
fn testSingleCandyAdvancesAtMostOneStage() {
    let mut s = hatched();
    s.add_growth(124_000_000);
    s.inventory.candies = 1;
    s.use_candy(1).unwrap();
    assert_eq!(s.active.as_ref().unwrap().stage, 1);
}
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
#[test]
fn testCannotUseOnEgg() {
    let mut s = Companion::default();
    s.inventory.candies = 2;
    assert!(s.use_candy(1).is_err());
    assert_eq!(s.inventory.candies, 2);
}
#[test]
fn testCannotUseWithoutStock() {
    let mut s = hatched();
    assert_eq!(s.inventory.candies, 0);
    assert!(s.use_candy(1).is_err());
}
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
#[test]
fn testBatchCapsAtGraduationAndPreservesUnusedCandies() {
    for remaining in [250_000_000, 300_000_000] {
        let mut s = Companion::default();
        s.egg_tokens = s.progress().1;
        s.hatch(line(1));
        s.add_growth(750_000_000 - remaining);
        s.inventory.candies = 5;
        assert!(matches!(
            s.use_candy(5).unwrap().as_slice(),
            [Event::Graduated(_)]
        ));
        assert!(s.active.is_none());
        assert_eq!(s.inventory.candies, 2);
        assert_eq!(s.egg_tokens, 0);
        assert_eq!(s.collection.len(), 1);
        assert!(s.use_candy(5).is_err());
    }
}
#[test]
fn testBatchCarriesXPThroughMultipleEvolutions() {
    let mut s = hatched();
    s.add_growth(100_000_000);
    s.inventory.candies = 10;
    let before = s.lifetime_tokens;
    let events = s.use_candy(3).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Evolved(_))));
    assert_eq!(s.active.as_ref().unwrap().stage, 2);
    assert_eq!(s.active.as_ref().unwrap().tokens, 25_000_000);
    assert_eq!(s.inventory.candies, 7);
    assert_eq!(s.lifetime_tokens, before);
}
#[test]
fn testBatchCapsAtInventoryAndRejectsNonpositiveAmounts() {
    let mut s = hatched();
    s.inventory.candies = 1;
    assert!(s.use_candy(0).is_err());
    assert!(serde_json::from_str::<u64>("-1").is_err());
    assert!(serde_json::from_str::<u64>("-9223372036854775808").is_err());
    assert_eq!(s.inventory.candies, 1);
    assert!(s.use_candy(u64::MAX).unwrap().is_empty());
    assert_eq!(s.active.as_ref().unwrap().tokens, CANDY_XP);
    assert_eq!(s.inventory.candies, 0);
}
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
#[test]
fn testSessionGrantAfterSeed() {
    let mut s = seeded("claude.fiveHour", None, false);
    s.observe_limit("claude.fiveHour", None, 100., false, true);
    assert_eq!(s.inventory.candies, 1);
}
#[test]
fn testWeeklyGrantsFiveCandies() {
    let mut s = seeded("claude.sevenDay", None, true);
    s.observe_limit("claude.sevenDay", None, 100., true, true);
    assert_eq!(s.inventory.candies, 5);
}
// Evolution data is persisted in Rust. An invalid/unloaded line is rejected on decode before items can be used.
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
