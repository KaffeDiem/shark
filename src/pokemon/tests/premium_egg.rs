use super::super::*;
use super::economy::{hatched, line};
fn egg(tier: Option<Rarity>) -> Companion {
    let mut s = Companion::default();
    s.egg_tier = tier;
    s.egg_tokens = s.progress().1;
    s
}
// upstream: PremiumEggTests.swift::testCaptureRateCeilingIsTheSameThresholdAsClassification
#[test]
fn testCaptureRateCeilingIsTheSameThresholdAsClassification() {
    for rate in 0..=255 {
        let classified = Rarity::from_capture_rate(rate, false, false);
        for tier in [Rarity::Common, Rarity::Uncommon, Rarity::Rare] {
            assert_eq!(
                tier.includes(rate),
                classified.rank() >= tier.rank(),
                "{rate} {tier:?}"
            );
        }
    }
}
// upstream: PremiumEggTests.swift::testLegendaryHasNoCaptureRateCeilingButPassesLowerTierFilters
#[test]
fn testLegendaryHasNoCaptureRateCeilingButPassesLowerTierFilters() {
    assert_eq!(Rarity::Legendary.capture_rate_ceiling(), None);
    assert!(!Rarity::Legendary.includes(3));
    for rate in [3, 30, 45] {
        assert!(Rarity::Rare.includes(rate));
        assert!(Rarity::Uncommon.includes(rate));
    }
    assert!(
        !hatched()
            .shop_entries()
            .contains(&ShopEntry::Egg(Some(Rarity::Legendary)))
    );
}
// upstream: PremiumEggTests.swift::testPricesFollowGraduationTotalRatio
#[test]
fn testPricesFollowGraduationTotalRatio() {
    assert_eq!(FreshEgg::price(None), 1_000_000_000);
    assert_eq!(FreshEgg::price(Some(Rarity::Uncommon)), 2_500_000_000);
    assert_eq!(FreshEgg::price(Some(Rarity::Rare)), 4_000_000_000);
    assert_eq!(
        hatched()
            .shop_entries()
            .iter()
            .filter_map(|e| match e {
                ShopEntry::Egg(tier) => Some(*tier),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [None, Some(Rarity::Uncommon), Some(Rarity::Rare)]
    );
}
// upstream: PremiumEggTests.swift::testRareEggIsNotDominatedAtMeasuredPoolComposition
#[test]
fn testRareEggIsNotDominatedAtMeasuredPoolComposition() {
    let uncommon = 3135.;
    let rare_plus = 3053. + 339.;
    let share = rare_plus / (uncommon + rare_plus);
    let uncommon_price = FreshEgg::price(Some(Rarity::Uncommon)) as f64;
    let rare_price = FreshEgg::price(Some(Rarity::Rare)) as f64;
    assert!(rare_price < uncommon_price / share);
    assert!(share < uncommon_price / rare_price);
    assert!(rare_plus / (uncommon * 0.7 + rare_plus) < uncommon_price / rare_price);
}
// upstream: PremiumEggTests.swift::testBuyPremiumEggRecordsGuaranteeAndDebitsTierPrice
#[test]
fn testBuyPremiumEggRecordsGuaranteeAndDebitsTierPrice() {
    let mut s = hatched();
    assert!(s.can_buy_egg(Some(Rarity::Rare)));
    s.fresh_egg(Some(Rarity::Rare)).unwrap();
    assert_eq!(s.min_rarity(), Some(Rarity::Rare));
    assert_eq!(s.spent_tokens, 4_000_000_000);
    assert!(s.active.is_none());
    assert_eq!(s.egg_tokens, 0);
    assert_eq!(s.collection.len(), 1);
    assert!(s.collection[0].released);
    assert!(!s.collection[0].graduated);
}
// upstream: PremiumEggTests.swift::testCannotBuyAnyEggWhileIncubating
#[test]
fn testCannotBuyAnyEggWhileIncubating() {
    let mut s = egg(None);
    s.lifetime_tokens = 20_000_000_000;
    for tier in [None, Some(Rarity::Uncommon), Some(Rarity::Rare)] {
        assert!(!s.can_buy_egg(tier));
        assert!(s.fresh_egg(tier).is_err());
    }
    assert_eq!(s.spent_tokens, 0);
}
// upstream: PremiumEggTests.swift::testFundsAreCheckedAgainstTierPrice
#[test]
fn testFundsAreCheckedAgainstTierPrice() {
    let mut s = hatched();
    s.lifetime_tokens = 3_000_000_000;
    assert!(s.can_buy_egg(None));
    assert!(s.can_buy_egg(Some(Rarity::Uncommon)));
    assert!(!s.can_buy_egg(Some(Rarity::Rare)));
    assert!(s.fresh_egg(Some(Rarity::Rare)).is_err());
    assert!(s.active.is_some());
    assert_eq!(s.spent_tokens, 0);
}
// upstream: PremiumEggTests.swift::testPurchaseStartsFromCleanRollState
#[test]
fn testPurchaseStartsFromCleanRollState() {
    let mut s = hatched();
    s.fresh_egg(Some(Rarity::Rare)).unwrap();
    assert_eq!(s.egg_tier, Some(Rarity::Rare));
    assert_eq!(s.egg_tokens, 0);
    assert!(s.active.is_none());
}
// upstream: PremiumEggTests.swift::testGuaranteeSurvivesRestart
#[test]
fn testGuaranteeSurvivesRestart() {
    let mut s = hatched();
    s.fresh_egg(Some(Rarity::Uncommon)).unwrap();
    let restored: Companion = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(restored.egg_tier, Some(Rarity::Uncommon));
    assert_eq!(restored.min_rarity(), Some(Rarity::Uncommon));
}
// upstream: PremiumEggTests.swift::testHatchDiscardsSpeciesBelowGuaranteeWhenIndexIsStale
#[test]
fn testHatchDiscardsSpeciesBelowGuaranteeWhenIndexIsStale() {
    let mut s = egg(Some(Rarity::Rare));
    assert!(s.hatch(line(1)).is_empty());
    assert!(s.active.is_none());
    assert_eq!(s.egg_tier, Some(Rarity::Rare));
    assert_eq!(s.egg_tokens, Balance::EGG_HATCH_THRESHOLD);
}
// upstream: PremiumEggTests.swift::testSameSpeciesHatchesNormallyWithoutGuarantee
#[test]
fn testSameSpeciesHatchesNormallyWithoutGuarantee() {
    let mut s = egg(None);
    s.hatch(line(1));
    assert_eq!(s.active.unwrap().line.rarity, Rarity::Common);
}
// upstream: PremiumEggTests.swift::testHatchConsumesGuarantee
#[test]
fn testHatchConsumesGuarantee() {
    let mut s = egg(Some(Rarity::Rare));
    let mut l = line(1);
    l.rarity = Rarity::Rare;
    s.hatch(l);
    assert!(s.active.is_some());
    assert_eq!(s.egg_tier, None);
    assert_eq!(s.min_rarity(), None);
}
// upstream: PremiumEggTests.swift::testGuaranteeDoesNotSurviveIntoTheNextEgg
#[test]
fn testGuaranteeDoesNotSurviveIntoTheNextEgg() {
    let mut s = egg(Some(Rarity::Rare));
    let mut l = line(1);
    l.rarity = Rarity::Rare;
    s.hatch(l);
    assert_eq!(s.egg_tier, None);
    s.add_growth(6_000_000_000);
    assert!(s.active.is_none());
    assert_eq!(s.collection.len(), 1);
    assert_eq!(s.egg_tier, None);
    assert_eq!(s.min_rarity(), None);
}
// upstream: PremiumEggTests.swift::testCannotBuyTierThatIsNotSold
#[test]
fn testCannotBuyTierThatIsNotSold() {
    let mut s = hatched();
    s.lifetime_tokens = 20_000_000_000;
    for tier in [Rarity::Common, Rarity::Legendary] {
        assert!(!s.can_buy_egg(Some(tier)));
        assert!(s.fresh_egg(Some(tier)).is_err());
    }
    assert_eq!(s.spent_tokens, 0);
    assert!(s.active.is_some());
}
// upstream: PremiumEggTests.swift::testGuaranteeTravelsWithSave
#[test]
fn testGuaranteeTravelsWithSave() {
    let mut s = Companion::default();
    s.egg_tier = Some(Rarity::Uncommon);
    s.lifetime_tokens = 1_000_000;
    let imported = storage::rebase(
        s,
        &Companion::default(),
        [(
            "test".into(),
            UsageBaseline {
                day: "d".into(),
                total: 0,
            },
        )]
        .into(),
    );
    assert_eq!(imported.egg_tier, Some(Rarity::Uncommon));
}
// upstream: PremiumEggTests.swift::testSanitizedDropsGuaranteeAndItsPreRollWhenActiveExists
#[test]
fn testSanitizedDropsGuaranteeAndItsPreRollWhenActiveExists() {
    let mut s = hatched();
    s.egg_tier = Some(Rarity::Rare);
    s.sanitize();
    assert_eq!(s.egg_tier, None);
    let mut s = egg(Some(Rarity::Rare));
    s.sanitize();
    assert_eq!(s.egg_tier, Some(Rarity::Rare));
}
// upstream: PremiumEggTests.swift::testUnsatisfiableGuaranteeIsNormalizedAwayAndEggStillHatches
#[test]
fn testUnsatisfiableGuaranteeIsNormalizedAwayAndEggStillHatches() {
    let mut s = egg(Some(Rarity::Legendary));
    s.sanitize();
    assert_eq!(s.egg_tier, None);
    s.hatch(line(1));
    assert!(s.active.is_some());
}
// upstream: PremiumEggTests.swift::testUnknownGuaranteeDecodesAsNoGuarantee
#[test]
fn testUnknownGuaranteeDecodesAsNoGuarantee() {
    let s: Companion = serde_json::from_str(r#"{"egg_tier":"mythic"}"#).unwrap();
    assert_eq!(s.egg_tier, None);
}

fn tiered() -> Vec<api::BaseSpecies> {
    [(1, 255), (2, 100), (3, 30), (4, 3)]
        .into_iter()
        .map(|(id, capture_rate)| api::BaseSpecies { id, capture_rate })
        .collect()
}
fn sampled(seed: u64, tier: Option<Rarity>) -> Rarity {
    let mut rng = seed;
    let entries = tiered();
    let id = api::choose_base(
        Ok(entries.clone()),
        tier,
        &Default::default(),
        &mut rng,
        |_| panic!("index path must not request REST"),
    )
    .unwrap();
    let entry = entries.into_iter().find(|e| e.id == id).unwrap();
    let rarity = Rarity::from_capture_rate(entry.capture_rate, id == 4, false);
    let mut s = egg(tier);
    s.random_seed = seed;
    let mut l = line(1);
    l.base_id = id;
    l.tree.species_id = id;
    l.rarity = rarity;
    s.hatch(l);
    s.active.unwrap().line.rarity
}
// upstream: PremiumEggTests.swift::testWeightedRollNeverGoesBelowGuarantee
#[test]
fn testWeightedRollNeverGoesBelowGuarantee() {
    let guaranteed: Vec<_> = (1..=20)
        .map(|seed| sampled(seed, Some(Rarity::Rare)))
        .collect();
    let controls: Vec<_> = (1..=20).map(|seed| sampled(seed, None)).collect();
    assert_eq!(guaranteed.len(), 20);
    assert!(guaranteed.iter().all(|&r| r >= Rarity::Rare));
    assert!(controls.contains(&Rarity::Common));
}
// upstream: PremiumEggTests.swift::testUncommonEggHatchesUncommonAndExcludesOnlyCommon
#[test]
fn testUncommonEggHatchesUncommonAndExcludesOnlyCommon() {
    let sampled: Vec<_> = (1..=20)
        .map(|seed| sampled(seed, Some(Rarity::Uncommon)))
        .collect();
    assert!(!sampled.contains(&Rarity::Common));
    assert!(sampled.contains(&Rarity::Uncommon));
}
// upstream: PremiumEggTests.swift::testLegendaryStillReachableFromPremiumEgg
#[test]
fn testLegendaryStillReachableFromPremiumEgg() {
    assert!((1..=60).any(|seed| sampled(seed, Some(Rarity::Rare)) == Rarity::Legendary));
}
fn rest_sample(seed: u64, tier: Option<Rarity>) -> Option<Rarity> {
    let mut rng = seed;
    let rate = |id| {
        if id % 3 == 0 {
            30
        } else if id % 2 == 0 {
            100
        } else {
            255
        }
    };
    let id = api::choose_base(
        Err("offline index".into()),
        tier,
        &Default::default(),
        &mut rng,
        |id| {
            Ok(Some(api::BaseSpecies {
                id,
                capture_rate: rate(id),
            }))
        },
    )
    .ok()?;
    let rarity = Rarity::from_capture_rate(rate(id), false, false);
    let mut s = egg(tier);
    s.random_seed = seed;
    let mut l = line(1);
    l.base_id = id;
    l.tree.species_id = id;
    l.rarity = rarity;
    s.hatch(l);
    Some(s.active.unwrap().line.rarity)
}
// upstream: PremiumEggTests.swift::testRestFallbackRespectsGuarantee
#[test]
fn testRestFallbackRespectsGuarantee() {
    let outcomes: Vec<_> = (1..=10)
        .filter_map(|seed| rest_sample(seed, Some(Rarity::Rare)))
        .collect();
    assert!(!outcomes.is_empty());
    assert!(outcomes.iter().all(|&r| r == Rarity::Rare));
}
// upstream: PremiumEggTests.swift::testRestFallbackHonoursUncommonTierWithoutOverFiltering
#[test]
fn testRestFallbackHonoursUncommonTierWithoutOverFiltering() {
    let outcomes: Vec<_> = (1..=20)
        .filter_map(|seed| rest_sample(seed, Some(Rarity::Uncommon)))
        .collect();
    assert!(!outcomes.contains(&Rarity::Common));
    assert!(outcomes.contains(&Rarity::Uncommon));
}
// upstream: PremiumEggTests.swift::testRestFallbackWithoutGuaranteeCanHatchCommon
#[test]
fn testRestFallbackWithoutGuaranteeCanHatchCommon() {
    assert!(
        (1..=20)
            .filter_map(|seed| rest_sample(seed, None))
            .any(|r| r == Rarity::Common)
    );
}
