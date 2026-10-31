use super::super::{Balance, FreshEgg, Item, Rarity};

const RARITIES: [Rarity; 4] = [
    Rarity::Common,
    Rarity::Uncommon,
    Rarity::Rare,
    Rarity::Legendary,
];

#[test]
fn testGraduationTotalIsConstantPerRarityRegardlessOfStages() {
    for rarity in RARITIES {
        for forms in 1..=3 {
            let total: u64 = (0..forms)
                .map(|stage| Balance::phase_threshold(rarity, forms, stage, 1))
                .sum();
            assert_eq!(total, rarity.graduation_total(), "{rarity:?}, {forms}");
        }
    }
}

#[test]
fn testRepeatGrowthHalvesEveryPhaseThreshold() {
    for rarity in RARITIES {
        for forms in 1..=3 {
            for stage in 0..forms {
                let normal = Balance::phase_threshold(rarity, forms, stage, 1);
                assert_eq!(
                    Balance::phase_threshold(rarity, forms, stage, 2),
                    normal / 2
                );
                assert_eq!(Balance::phase_threshold(rarity, forms, stage, 0), normal);
            }
        }
    }
}

#[test]
fn testHigherStageCostsMore() {
    let thresholds: Vec<_> = (0..3)
        .map(|stage| Balance::phase_threshold(Rarity::Common, 3, stage, 1))
        .collect();
    assert_eq!(thresholds, [125_000_000, 250_000_000, 375_000_000]);
}

#[test]
fn testRarerCostsMore() {
    assert!(
        RARITIES
            .windows(2)
            .all(|pair| pair[0].graduation_total() < pair[1].graduation_total())
    );
}

#[test]
fn testRarityDerivation() {
    assert_eq!(Rarity::from_capture_rate(255, false, false), Rarity::Common);
    assert_eq!(
        Rarity::from_capture_rate(100, false, false),
        Rarity::Uncommon
    );
    assert_eq!(Rarity::from_capture_rate(45, false, false), Rarity::Rare);
    assert_eq!(Rarity::from_capture_rate(3, true, false), Rarity::Legendary);
    assert_eq!(Rarity::from_capture_rate(3, false, true), Rarity::Legendary);
}

#[test]
fn testCaptureRateBoundaries() {
    for (rate, expected) in [
        (45, Rarity::Rare),
        (46, Rarity::Uncommon),
        (120, Rarity::Uncommon),
        (121, Rarity::Common),
    ] {
        assert_eq!(Rarity::from_capture_rate(rate, false, false), expected);
    }
}

#[test]
fn testLegendaryAndMythicalOverrideCaptureRate() {
    assert_eq!(
        Rarity::from_capture_rate(255, true, false),
        Rarity::Legendary
    );
    assert_eq!(
        Rarity::from_capture_rate(255, false, true),
        Rarity::Legendary
    );
}

#[test]
fn testGarbageDefaultsAreClamped() {
    assert_eq!(Balance::clamp_difficulty(0.0), 0.1);
    assert_eq!(Balance::clamp_difficulty(-5.0), 0.1);
    assert_eq!(Balance::clamp_difficulty(20.0), 2.0);
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(Balance::clamp_difficulty(invalid), 1.0);
    }
}

#[test]
fn testPriceIsOneBillion() {
    assert_eq!(FreshEgg::price(None), 1_000_000_000);
}

#[test]
fn minimum_difficulty_preserves_positive_growth_and_shop_prices() {
    assert!(Balance::scaled(Balance::EGG_HATCH_THRESHOLD, 0.1) > 0);
    for rarity in RARITIES {
        for forms in 1..=3 {
            for stage in 0..forms {
                assert!(
                    Balance::scaled(Balance::phase_threshold(rarity, forms, stage, 1), 0.1) > 0
                );
            }
        }
    }
    for item in [Item::RareCandy, Item::Mint, Item::ShinyCharm] {
        assert!(Balance::scaled(item.price(), 0.1) > 0);
    }
}
