use super::super::*;
use super::economy::{hatched, line};
fn raised(growth: f64, shop: f64) -> Companion {
    let mut s = hatched();
    s.set_growth_difficulty(growth);
    s.set_shop_difficulty(shop);
    s
}
fn stage(s: &Companion) -> Option<usize> {
    s.active.as_ref().map(|m| m.stage)
}
fn remaining(s: &Companion) -> u64 {
    let (used, total) = s.progress();
    total.saturating_sub(used)
}
#[test]
fn testRepeatBoostComposesWithDifficultyAndLiveChanges() {
    let mut s = raised(0.5, 1.);
    s.add_growth(375_000_000);
    assert!(s.active.is_none());
    s.egg_tokens = s.progress().1;
    s.hatch(line(3));
    assert!(s.active.as_ref().unwrap().growth_boost);
    assert_eq!(s.progress().1, 31_250_000);
    s.add_growth(31_249_999);
    assert_eq!(stage(&s), Some(0));
    assert_eq!(remaining(&s), 1);
    s.add_growth(1);
    assert_eq!(stage(&s), Some(1));
    assert_eq!(s.progress().0, 0);
    s.add_growth(s.progress().1 / 2);
    s.set_growth_difficulty(0.25);
    assert_eq!(stage(&s), Some(1));
    assert_eq!(s.progress().0 * 2, s.progress().1);
    s.add_growth(remaining(&s));
    assert_eq!(stage(&s), Some(2));
    s.add_growth(remaining(&s));
    assert!(s.active.is_none());
    assert_eq!(s.collection.last().unwrap().species_id(), 3);
}
#[test]
fn testSameTokensEvolveOnlyWhenDifficultyLowered() {
    let mut normal = raised(1., 1.);
    normal.add_growth(62_500_000);
    assert_eq!(stage(&normal), Some(0));
    assert_eq!(normal.active.unwrap().species_id(), 1);
    let mut easy = raised(0.5, 1.);
    easy.add_growth(62_500_000);
    assert_eq!(stage(&easy), Some(1));
    assert_eq!(easy.active.unwrap().species_id(), 2);
}
#[test]
fn testHarderDifficultyBlocksEvolutionThatWouldOtherwiseHappen() {
    let mut normal = raised(1., 1.);
    normal.add_growth(125_000_000);
    assert_eq!(stage(&normal), Some(1));
    let mut hard = raised(2., 1.);
    hard.add_growth(125_000_000);
    assert_eq!(stage(&hard), Some(0));
}
#[test]
fn testDisplayedTokensToNextMatchesTheThresholdThatActuallyEvolves() {
    let mut s = raised(0.5, 1.);
    let shown = remaining(&s);
    assert_eq!(shown, 62_500_000);
    s.add_growth(shown - 1);
    assert_eq!(stage(&s), Some(0));
    assert_eq!(remaining(&s), 1);
    s.add_growth(1);
    assert_eq!(stage(&s), Some(1));
}
#[test]
fn testEggHatchPointMovesWithDifficulty() {
    for (difficulty, expected) in [(1., false), (0.5, true)] {
        let mut s = Companion::default();
        s.set_growth_difficulty(difficulty);
        s.observe_usage("t", "2026-01-01", 0, true);
        s.observe_usage("t", "2026-01-01", 2_500_000, true);
        s.hatch(line(3));
        assert_eq!(s.active.is_some(), expected);
    }
}
#[test]
fn testShopGateAndChargedAmountFollowDifficulty() {
    let mut normal = raised(1., 1.);
    normal.lifetime_tokens = 250_000_000;
    assert!(normal.balance() < normal.price(Item::RareCandy));
    let mut cheap = raised(1., 0.5);
    cheap.lifetime_tokens = 250_000_000;
    assert!(cheap.balance() >= cheap.price(Item::RareCandy));
    cheap.buy(Item::RareCandy, 1).unwrap();
    assert_eq!(cheap.spent_tokens, 250_000_000);
    assert_eq!(cheap.inventory.candies, 1);
}
#[test]
fn testEggPriceScalesAndIsActuallyCharged() {
    let mut s = raised(1., 0.5);
    s.lifetime_tokens = 1_000_000_000;
    assert_eq!(s.egg_price(None), 500_000_000);
    s.fresh_egg(None).unwrap();
    assert_eq!(s.spent_tokens, 500_000_000);
}
#[test]
fn testGrowthSliderDoesNotMoveShopPricesAndViceVersa() {
    let growth = raised(0.1, 1.);
    assert_eq!(growth.price(Item::RareCandy), 500_000_000);
    assert_eq!(growth.egg_price(Some(Rarity::Rare)), 4_000_000_000);
    let shop = raised(1., 0.1);
    assert_eq!(remaining(&shop), 125_000_000);
}
#[test]
fn testGradedEggRatioSurvivesScaling() {
    let s = raised(1., 0.4);
    let plain = s.egg_price(None) as f64;
    assert_eq!(s.egg_price(Some(Rarity::Uncommon)) as f64 / plain, 2.5);
    assert_eq!(s.egg_price(Some(Rarity::Rare)) as f64 / plain, 4.);
}
#[test]
fn testLoweringDifficultyPreservesIncompleteStage() {
    let mut s = raised(1., 1.);
    s.add_growth(124_999_999);
    assert_eq!(stage(&s), Some(0));
    s.set_growth_difficulty(0.5);
    assert_eq!(stage(&s), Some(0));
    assert_eq!(remaining(&s), 1);
    s.add_growth(0);
    assert_eq!(stage(&s), Some(0));
    s.add_growth(1);
    assert_eq!(stage(&s), Some(1));
}
#[test]
fn testShopPriceChangesLiveWithoutRestart() {
    let mut s = raised(1., 1.);
    assert_eq!(s.price(Item::RareCandy), 500_000_000);
    s.set_shop_difficulty(0.5);
    assert_eq!(s.price(Item::RareCandy), 250_000_000);
}
#[test]
fn testDifficultyPersistsToDefaults() {
    let mut s = hatched();
    s.set_growth_difficulty(0.3);
    s.set_shop_difficulty(1.7);
    let decoded: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(decoded.growth_difficulty, 0.3);
    assert_eq!(decoded.shop_difficulty, 1.7);
}
#[test]
fn testZeroInDefaultsCannotProduceZeroThreshold() {
    let s = raised(0., 1.);
    assert_eq!(s.growth_difficulty, 0.1);
    assert!(remaining(&s) > 0);
    let mut egg = Companion::default();
    egg.set_growth_difficulty(0.);
    assert!(egg.progress().1 > 0);
}
#[test]
fn testNothingCollapsesToZeroAtMinimumDifficulty() {
    assert!(Balance::scaled(Balance::EGG_HATCH_THRESHOLD, 0.1) > 0);
    for rarity in [
        Rarity::Common,
        Rarity::Uncommon,
        Rarity::Rare,
        Rarity::Legendary,
    ] {
        for forms in 1..=3 {
            for stage in 0..forms {
                assert!(
                    Balance::scaled(Balance::phase_threshold(rarity, forms, stage, 1), 0.1) > 0
                );
            }
        }
    }
    let s = raised(1., 0.1);
    for price in [
        s.price(Item::RareCandy),
        s.price(Item::Mint),
        s.price(Item::ShinyCharm),
        s.egg_price(None),
        s.egg_price(Some(Rarity::Uncommon)),
        s.egg_price(Some(Rarity::Rare)),
    ] {
        assert!(price > 0);
    }
}
#[test]
fn testSmallUsageChunksMatchSingleDeltaAtHardDifficulty() {
    let mut small = raised(2., 1.);
    let mut large = raised(2., 1.);
    for _ in 0..20 {
        small.add_growth(1);
    }
    large.add_growth(20);
    let a = small.active.unwrap().profile.unwrap();
    let b = large.active.unwrap().profile.unwrap();
    assert_eq!(a.growth_tokens, 10);
    assert_eq!(a.growth_tokens, b.growth_tokens);
    assert_eq!(a.level, b.level);
}
